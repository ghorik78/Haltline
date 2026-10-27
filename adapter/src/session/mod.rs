//! The debug session: turns DAP requests into GDB/MI commands and GDB events
//! into DAP events.
//!
//! Everything runs on one thread. Inputs (DAP requests, GDB output, GDB server
//! output) arrive through a single channel and are processed in order. MI
//! commands are synchronous; their results are routed back by token, so a
//! handler can issue commands while unrelated events wait in the channel.

mod breakpoints;
mod inspect;
#[cfg(test)]
mod tests;

use crate::config::{Config, RequestKind, ServerType};
use crate::dap::{Category, DapWriter, Request};
use crate::disasm::DisasmCache;
use crate::mi::client::{MiClient, MiEvent, MiResponse};
use crate::mi::{AsyncKind, StreamKind, Value as Mi};
use crate::server::{self, ManagedProcess, ServerCommand, ServerEvent};
use crate::svd::Device;
use crate::util::mi_quote;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use breakpoints::Bp;

/// Everything the session reacts to.
#[derive(Debug)]
pub enum Event {
    Dap(Value),
    DapClosed,
    Mi(MiEvent),
    Server(ServerEvent),
}

/// Pipes of a started GDB.
pub struct GdbProcess {
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub stderr: Option<Box<dyn Read + Send>>,
    pub child: Option<Child>,
}

/// Starts external programs. Tests replace it with in-process fakes.
pub trait Backend: Send {
    fn spawn_gdb(&mut self, program: &Path, args: &[String], cwd: &Path) -> Result<GdbProcess>;
    fn spawn_server(
        &mut self,
        cmd: &ServerCommand,
        cwd: &Path,
        env: &[(String, String)],
        sink: Box<dyn Fn(ServerEvent) + Send + Sync>,
    ) -> Result<ManagedProcess>;
}

/// Spawns real processes.
pub struct ProcessBackend;

impl Backend for ProcessBackend {
    fn spawn_gdb(&mut self, program: &Path, args: &[String], cwd: &Path) -> Result<GdbProcess> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        server::tie_to_parent(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start GDB `{}`", program.display()))?;
        Ok(GdbProcess {
            stdin: Box::new(child.stdin.take().unwrap()),
            stdout: Box::new(child.stdout.take().unwrap()),
            stderr: child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child: Some(child),
        })
    }

    fn spawn_server(
        &mut self,
        cmd: &ServerCommand,
        cwd: &Path,
        env: &[(String, String)],
        sink: Box<dyn Fn(ServerEvent) + Send + Sync>,
    ) -> Result<ManagedProcess> {
        ManagedProcess::spawn(&cmd.program, &cmd.args, cwd, env, sink)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetState {
    /// Not connected yet.
    Idle,
    Stopped,
    Running,
    Exited,
}

/// What the next stop means, when we caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    Nothing,
    /// Running to the entry point; the temporary breakpoint has this number.
    Entry(Option<String>),
    Pause,
}

#[derive(Debug, Clone, Default)]
struct StopInfo {
    reason: String,
    description: Option<String>,
    thread: i64,
    text: Option<String>,
    hit_ids: Vec<i64>,
}

/// Result of a request handler.
enum Reply {
    Body(Value),
    /// The handler already sent the response.
    Responded,
}

type HandlerResult = Result<Reply>;

fn body(v: Value) -> HandlerResult {
    Ok(Reply::Body(v))
}

pub struct Session {
    out: Arc<DapWriter>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    deferred: VecDeque<Event>,
    backend: Box<dyn Backend>,

    config: Config,
    kind: RequestKind,
    gdb: Option<MiClient>,
    gdb_child: Option<Child>,
    server: Option<ManagedProcess>,
    shutting_down: bool,
    exit_loop: bool,
    terminated_sent: bool,
    lines_start_at1: bool,

    state: TargetState,
    expect: Expect,
    resumed_by_us: bool,
    last_stop: Option<StopInfo>,
    /// Async sequence ranges whose stops were side effects of evaluations.
    swallow: Vec<(u64, u64)>,
    configured: bool,

    // Breakpoints
    bps: Vec<Bp>,
    next_bp_id: i64,
    fault_filter: bool,

    // Inspection state (reset on every stop)
    insp: inspect::Inspection,
    svd: Option<Arc<Device>>,
    disasm: DisasmCache,
    /// `:asm on/off` override; `None` follows `showDisassembly`.
    asm_override: Option<bool>,
    /// The last step request asked for instruction granularity.
    instruction_stepping: bool,
    hex: bool,
    threads_cache: Vec<Value>,
}

const GDB_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

impl Session {
    pub fn new(out: Box<dyn Write + Send>, backend: Box<dyn Backend>) -> (Self, Sender<Event>) {
        let (tx, rx) = mpsc::channel();
        let session = Self {
            out: Arc::new(DapWriter::new(out)),
            tx: tx.clone(),
            rx,
            deferred: VecDeque::new(),
            backend,
            config: Config::default(),
            kind: RequestKind::Launch,
            gdb: None,
            gdb_child: None,
            server: None,
            shutting_down: false,
            exit_loop: false,
            terminated_sent: false,
            lines_start_at1: true,
            state: TargetState::Idle,
            expect: Expect::Nothing,
            resumed_by_us: false,
            last_stop: None,
            swallow: Vec::new(),
            configured: false,
            bps: Vec::new(),
            next_bp_id: 1,
            fault_filter: false,
            insp: inspect::Inspection::default(),
            svd: None,
            disasm: DisasmCache::new(DisasmCache::dir_for_elf(&std::env::temp_dir(), None)),
            asm_override: None,
            instruction_stepping: false,
            hex: false,
            threads_cache: Vec::new(),
        };
        (session, tx)
    }

    /// Reads DAP messages from `input` on a background thread.
    pub fn spawn_dap_reader(input: Box<dyn Read + Send>, tx: Sender<Event>) {
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(input);
                loop {
                    match crate::dap::read_message(&mut reader) {
                        Ok(Some(msg)) => {
                            if tx.send(Event::Dap(msg)).is_err() {
                                return;
                            }
                        }
                        Ok(None) | Err(_) => {
                            let _ = tx.send(Event::DapClosed);
                            return;
                        }
                    }
                }
            })
            .expect("spawn dap reader");
    }

    /// Processes events until the client disconnects.
    pub fn run(mut self) {
        while !self.exit_loop {
            let event = match self.deferred.pop_front() {
                Some(e) => e,
                None => match self.rx.recv() {
                    Ok(e) => e,
                    Err(_) => break,
                },
            };
            self.handle_event(event);
        }
        self.shutdown();
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Dap(msg) => {
                if let Some(req) = Request::from_value(&msg) {
                    self.handle_request(req);
                }
            }
            Event::DapClosed => self.exit_loop = true,
            Event::Mi(e) => self.handle_mi_event(e),
            Event::Server(e) => self.handle_server_event(e),
        }
    }

    fn handle_request(&mut self, req: Request) {
        let result = match req.command.as_str() {
            "initialize" => self.initialize(&req),
            "launch" => self.launch(&req, RequestKind::Launch),
            "attach" => self.launch(&req, RequestKind::Attach),
            "configurationDone" => self.configuration_done(&req),
            "setBreakpoints" => self.set_breakpoints(&req),
            "setFunctionBreakpoints" => self.set_function_breakpoints(&req),
            "setInstructionBreakpoints" => self.set_instruction_breakpoints(&req),
            "setExceptionBreakpoints" => self.set_exception_breakpoints(&req),
            "dataBreakpointInfo" => self.data_breakpoint_info(&req),
            "setDataBreakpoints" => self.set_data_breakpoints(&req),
            "threads" => self.threads(&req),
            "stackTrace" => self.stack_trace(&req),
            "scopes" => self.scopes(&req),
            "variables" => self.variables(&req),
            "setVariable" => self.set_variable(&req),
            "setExpression" => self.set_expression(&req),
            "evaluate" => self.evaluate(&req),
            "readMemory" => self.read_memory(&req),
            "writeMemory" => self.write_memory(&req),
            "disassemble" => self.disassemble(&req),
            "exceptionInfo" => self.exception_info(&req),
            "continue" => self.continue_(&req),
            "next" => self.step(&req, StepKind::Over),
            "stepIn" => self.step(&req, StepKind::In),
            "stepOut" => self.step(&req, StepKind::Out),
            "pause" => self.pause(&req),
            "restart" => self.restart(&req),
            "terminate" => self.terminate(&req),
            "disconnect" => self.disconnect(&req),
            "cancel" => body(Value::Null),
            "source" => Err(anyhow!("source is not available for this frame")),
            other => Err(anyhow!("unsupported request `{other}`")),
        };
        match result {
            Ok(Reply::Body(b)) => self.out.respond(&req, b),
            Ok(Reply::Responded) => {}
            Err(e) => self.out.respond_error(&req, &format!("{e:#}")),
        }
    }

    // ----- helpers -------------------------------------------------------

    fn gdb(&self) -> Result<&MiClient> {
        self.gdb
            .as_ref()
            .ok_or_else(|| anyhow!("the debug session is not started"))
    }

    fn mi(&self, cmd: &str) -> Result<MiResponse> {
        Ok(self.gdb()?.cmd(cmd)?)
    }

    fn mi_timeout(&self, cmd: &str, timeout: Duration) -> Result<MiResponse> {
        Ok(self.gdb()?.cmd_timeout(cmd, timeout)?)
    }

    /// Runs a GDB CLI command and returns its console output.
    fn cli(&mut self, cmd: &str, timeout: Duration) -> Result<String> {
        let was_stopped = self.state == TargetState::Stopped;
        let resp = self.mi_timeout(
            &format!("-interpreter-exec console {}", mi_quote(cmd)),
            timeout,
        )?;
        if was_stopped {
            self.note_side_effects(&resp);
        }
        if resp.class == "running" {
            self.state = TargetState::Running;
            self.on_external_resume();
        }
        Ok(resp.console)
    }

    /// Remembers async records produced while an evaluation was running
    /// (e.g. inferior function calls) so their stop events are ignored.
    fn note_side_effects(&mut self, resp: &MiResponse) {
        if resp.seq_end > resp.seq_start {
            self.swallow.push((resp.seq_start, resp.seq_end));
        }
    }

    fn is_swallowed(&mut self, seq: u64) -> bool {
        let hit = self.swallow.iter().any(|(a, b)| seq > *a && seq <= *b);
        if let Some(max) = self.swallow.iter().map(|(_, b)| *b).max() {
            if seq >= max {
                self.swallow.clear();
            }
        }
        hit
    }

    fn require_stopped(&self) -> Result<()> {
        match self.state {
            TargetState::Stopped => Ok(()),
            TargetState::Running => bail!("the target is running; pause it first"),
            TargetState::Exited => bail!("the program has exited"),
            TargetState::Idle => bail!("the debug session is not started"),
        }
    }

    fn console(&self, text: &str) {
        self.out.console(text);
    }

    fn warn(&self, text: &str) {
        self.out.console(&format!("Warning: {text}"));
    }

    fn client_line(&self, line: i64) -> i64 {
        if self.lines_start_at1 {
            line
        } else {
            line - 1
        }
    }

    fn gdb_line(&self, line: i64) -> i64 {
        if self.lines_start_at1 {
            line
        } else {
            line + 1
        }
    }

    /// Waits for an event matching `pred`. Output is forwarded meanwhile;
    /// other events are queued and processed later in order.
    fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&Event) -> bool,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let event = match self.rx.recv_timeout(remaining) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                    return None
                }
            };
            if pred(&event) {
                return Some(event);
            }
            match event {
                Event::Server(ServerEvent::Line { text, stderr }) => {
                    self.forward_server_line(&text, stderr)
                }
                Event::Mi(MiEvent::Stream { kind, text, .. }) => {
                    self.forward_gdb_stream(kind, &text)
                }
                Event::Mi(MiEvent::Other(text)) => self.forward_gdb_other(&text),
                other => self.deferred.push_back(other),
            }
        }
    }

    fn forward_server_line(&self, text: &str, _stderr: bool) {
        if !text.trim().is_empty() {
            self.out.output(Category::Console, &format!("{text}\n"));
        }
    }

    fn forward_gdb_stream(&self, kind: StreamKind, text: &str) {
        match kind {
            StreamKind::Console => self.out.output(Category::Console, text),
            StreamKind::Target => self.out.output(Category::Stdout, text),
            StreamKind::Log => {
                if self.config.debug_output || text.starts_with("warning:") {
                    self.out.output(Category::Console, text);
                }
            }
        }
    }

    fn forward_gdb_other(&self, text: &str) {
        if !text.trim().is_empty() {
            self.out.output(Category::Stdout, &format!("{text}\n"));
        }
    }

    fn handle_server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Line { text, stderr } => self.forward_server_line(&text, stderr),
            ServerEvent::Exited(code) => {
                if !self.shutting_down {
                    self.out.output(
                        Category::Important,
                        &format!("The GDB server exited unexpectedly (code {code:?}).\n"),
                    );
                    self.send_terminated();
                }
            }
        }
    }

    fn handle_mi_event(&mut self, event: MiEvent) {
        match event {
            MiEvent::Stream { kind, text, .. } => self.forward_gdb_stream(kind, &text),
            MiEvent::Other(text) => self.forward_gdb_other(&text),
            MiEvent::Exited => {
                if !self.shutting_down {
                    self.out
                        .output(Category::Important, "GDB exited unexpectedly.\n");
                    self.send_terminated();
                }
            }
            MiEvent::Async {
                seq,
                kind: AsyncKind::Exec,
                class,
                body,
            } => match class.as_str() {
                "running" => {
                    if self.is_swallowed(seq) {
                        return;
                    }
                    let was_running = self.state == TargetState::Running;
                    self.state = TargetState::Running;
                    if !self.resumed_by_us && !was_running {
                        self.on_external_resume();
                    }
                }
                "stopped" => {
                    if self.is_swallowed(seq) {
                        self.state = TargetState::Stopped;
                        return;
                    }
                    self.on_stopped(&body);
                }
                _ => {}
            },
            MiEvent::Async {
                kind: AsyncKind::Notify,
                class,
                body,
                ..
            } => match class.as_str() {
                "thread-created" | "thread-exited" => {
                    if let Some(id) = body.i64("id") {
                        let reason = if class == "thread-created" {
                            "started"
                        } else {
                            "exited"
                        };
                        self.out
                            .event("thread", json!({"reason": reason, "threadId": id}));
                    }
                }
                _ => {}
            },
            MiEvent::Async { .. } => {}
        }
    }

    /// The target started running without a request from the client
    /// (e.g. `continue` typed in the console).
    fn on_external_resume(&mut self) {
        self.insp.reset(self.gdb.as_ref());
        self.out.event(
            "continued",
            json!({"threadId": 1, "allThreadsContinued": true}),
        );
    }

    fn send_terminated(&mut self) {
        if !self.terminated_sent {
            self.terminated_sent = true;
            self.out.event("terminated", Value::Null);
        }
    }

    // ----- lifecycle ------------------------------------------------------

    fn initialize(&mut self, req: &Request) -> HandlerResult {
        self.lines_start_at1 = req.arg_bool("linesStartAt1").unwrap_or(true);
        body(json!({
            "supportsConfigurationDoneRequest": true,
            "supportsFunctionBreakpoints": true,
            "supportsConditionalBreakpoints": true,
            "supportsHitConditionalBreakpoints": true,
            "supportsLogPoints": true,
            "supportsInstructionBreakpoints": true,
            "supportsDataBreakpoints": true,
            "supportsDisassembleRequest": true,
            "supportsSteppingGranularity": true,
            "supportsReadMemoryRequest": true,
            "supportsWriteMemoryRequest": true,
            "supportsSetVariable": true,
            "supportsSetExpression": true,
            "supportsEvaluateForHovers": true,
            "supportsRestartRequest": true,
            "supportsTerminateRequest": true,
            "supportTerminateDebuggee": true,
            "supportsExceptionInfoRequest": true,
            "supportsValueFormattingOptions": true,
            "supportsCancelRequest": false,
            "exceptionBreakpointFilters": [{
                "filter": "faults",
                "label": "Faults (HardFault, MemManage, BusFault, UsageFault)",
                "description": "Stop when the CPU enters a fault handler and explain the fault",
                "default": true,
            }],
        }))
    }

    fn launch(&mut self, req: &Request, kind: RequestKind) -> HandlerResult {
        if self.gdb.is_some() {
            bail!("the session is already started");
        }
        let mut args = req.arguments.clone();
        if args.is_null() {
            args = json!({});
        }
        args["request"] = json!(match kind {
            RequestKind::Launch => "launch",
            RequestKind::Attach => "attach",
        });
        let config = Config::from_json(&args)?;
        self.kind = kind;
        let unknown = config.unknown_keys();
        if !unknown.is_empty() {
            self.warn(&format!(
                "unknown configuration keys ignored: {}",
                unknown.join(", ")
            ));
        }
        config.validate()?;
        self.hex = config.hex_values;
        self.fault_filter = false;
        self.disasm = DisasmCache::new(DisasmCache::dir_for_elf(
            &std::env::temp_dir(),
            config.executable_path().as_deref(),
        ));
        if let Some(svd_path) = config.svd_path() {
            match Device::load(&svd_path) {
                Ok(device) => {
                    self.console(&format!(
                        "Loaded SVD {} ({} peripherals)",
                        device.name,
                        device.peripherals.len()
                    ));
                    self.svd = Some(Arc::new(device));
                }
                Err(e) => self.warn(&format!("{e:#}; the Peripherals view is disabled")),
            }
        }
        self.config = config;

        if let Err(e) = self.start_backend() {
            self.shutdown_processes();
            return Err(e);
        }
        self.state = TargetState::Stopped;
        self.out.respond(req, Value::Null);
        self.out.event("initialized", Value::Null);
        Ok(Reply::Responded)
    }

    fn start_backend(&mut self) -> Result<()> {
        let cwd = self.config.base_dir();
        let port = match self.config.port {
            Some(p) => p,
            None => server::free_port()?,
        };
        if let Some(cmd) = server::server_command(&self.config, port)? {
            self.start_server(&cmd, &cwd)?;
        }
        let gdb_path = server::find_gdb(&self.config)?;
        server::check_program(&gdb_path, "GDB")?;
        let mut gdb_args = vec!["--interpreter=mi3".to_string(), "-q".into(), "-nx".into()];
        gdb_args.extend(self.config.gdb_args.iter().cloned());
        let proc = self.backend.spawn_gdb(&gdb_path, &gdb_args, &cwd)?;
        let tx = self.tx.clone();
        let trace: Option<crate::mi::client::Trace> = if self.config.debug_output {
            let out = self.out.clone();
            Some(Arc::new(move |line: &str| {
                out.output(Category::Console, &format!("[mi] {line}\n"))
            }))
        } else {
            None
        };
        if let Some(stderr) = proc.stderr {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = tx.send(Event::Mi(MiEvent::Stream {
                        seq: 0,
                        kind: StreamKind::Log,
                        text: format!("warning: {line}\n"),
                    }));
                }
            });
        }
        self.gdb = Some(MiClient::start(
            proc.stdin,
            proc.stdout,
            move |e| {
                let _ = tx.send(Event::Mi(e));
            },
            trace,
        ));
        self.gdb_child = proc.child;

        let version = self
            .mi("-gdb-version")
            .map(|r| r.console)
            .unwrap_or_default();
        if let Some(first) = version.lines().next() {
            self.console(&format!("Using {first}"));
        }
        for cmd in [
            "-gdb-set mi-async on",
            "-gdb-set pagination off",
            "-gdb-set confirm off",
            "-gdb-set width 0",
            "-gdb-set height 0",
            "-gdb-set print object on",
        ] {
            self.mi(cmd)
                .with_context(|| format!("GDB rejected `{cmd}`"))?;
        }
        let _ = self.mi("-enable-pretty-printing");
        let maps: Vec<(String, String)> = self
            .config
            .source_file_map
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        for (from, to) in maps {
            self.cli(
                &format!("set substitute-path {from} {to}"),
                Duration::from_secs(5),
            )?;
        }
        if let Some(elf) = self.config.executable_path() {
            let path = elf.to_string_lossy().replace('\\', "/");
            self.mi_timeout(
                &format!("-file-exec-and-symbols {}", mi_quote(&path)),
                Duration::from_secs(60),
            )
            .context("GDB cannot load the executable")?;
        }

        self.connect_target(port)?;

        let commands = match self.kind {
            RequestKind::Launch => match &self.config.override_launch_commands {
                Some(cmds) => cmds.clone(),
                None => {
                    let mut cmds = self.config.pre_launch_commands.clone();
                    cmds.extend(self.config.reset_commands());
                    if self.config.should_load() {
                        cmds.push("load".into());
                        cmds.extend(self.config.reset_commands());
                    }
                    cmds.extend(self.semihosting_commands());
                    cmds.extend(self.config.post_launch_commands.iter().cloned());
                    cmds
                }
            },
            RequestKind::Attach => match &self.config.override_attach_commands {
                Some(cmds) => cmds.clone(),
                None => {
                    let mut cmds = self.config.pre_attach_commands.clone();
                    cmds.extend(self.semihosting_commands());
                    cmds.extend(self.config.post_attach_commands.iter().cloned());
                    cmds
                }
            },
        };
        self.run_commands(&commands)?;
        self.flush_registers();
        Ok(())
    }

    /// OpenOCD enables semihosting with a monitor command; other servers
    /// take a command line flag (see `server_command`).
    fn semihosting_commands(&self) -> Vec<String> {
        if self.config.semihosting && self.config.server_type == ServerType::Openocd {
            vec!["monitor arm semihosting enable".into()]
        } else {
            Vec::new()
        }
    }

    fn start_server(&mut self, cmd: &ServerCommand, cwd: &Path) -> Result<()> {
        server::check_program(&cmd.program, self.config.server_type.name())?;
        self.console(&format!(
            "Starting {}: {} {}",
            self.config.server_type.name(),
            cmd.program.display(),
            cmd.args.join(" ")
        ));
        let tx = self.tx.clone();
        let env: Vec<(String, String)> = self
            .config
            .server_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let process = self.backend.spawn_server(
            cmd,
            cwd,
            &env,
            Box::new(move |e| {
                let _ = tx.send(Event::Server(e));
            }),
        )?;
        self.server = Some(process);
        if cmd.ready_texts.is_empty() {
            return Ok(());
        }
        let timeout = Duration::from_millis(self.config.server_timeout.unwrap_or(15_000));
        let ready = cmd.ready_texts.clone();
        let found = self.wait_for(timeout, |e| match e {
            Event::Server(ServerEvent::Line { text, .. }) => server::is_ready_line(text, &ready),
            Event::Server(ServerEvent::Exited(_)) => true,
            _ => false,
        });
        match found {
            Some(Event::Server(ServerEvent::Line { text, stderr })) => {
                self.forward_server_line(&text, stderr);
                Ok(())
            }
            Some(Event::Server(ServerEvent::Exited(code))) => Err(self.server_failure(Some(code))),
            _ => {
                self.warn(&format!(
                    "{} did not report readiness within {} ms; trying to connect anyway",
                    self.config.server_type.name(),
                    timeout.as_millis()
                ));
                Ok(())
            }
        }
    }

    fn server_failure(&self, code: Option<Option<i32>>) -> anyhow::Error {
        let output = self
            .server
            .as_ref()
            .map(|s| s.recent_output())
            .unwrap_or_default();
        let mut msg = format!(
            "{} failed to start{}",
            self.config.server_type.name(),
            match code {
                Some(Some(c)) => format!(" (exit code {c})"),
                _ => String::new(),
            }
        );
        if let Some(hint) = server::diagnose_server_output(&output) {
            msg.push_str(&format!(": {hint}"));
        }
        if !output.is_empty() {
            msg.push_str(&format!("\n\nServer output:\n{output}"));
        }
        anyhow!(msg)
    }

    fn connect_target(&mut self, port: u16) -> Result<()> {
        let target = server::gdb_target(&self.config, port);
        let mode = if self.config.server_type == ServerType::Qemu {
            "remote"
        } else {
            "extended-remote"
        };
        self.console(&format!("Connecting GDB to {target}"));
        let deadline = Instant::now() + GDB_CONNECT_TIMEOUT;
        loop {
            let resp = self.mi_timeout(
                &format!("-target-select {mode} {target}"),
                GDB_CONNECT_TIMEOUT,
            );
            match resp {
                Ok(resp) => {
                    // GDB reports the halted target with a *stopped record; it
                    // is not a stop the client should hear about.
                    self.note_side_effects(&resp);
                    return Ok(());
                }
                Err(e) => {
                    let text = e.to_string();
                    let retry = text.contains("Connection refused")
                        || text.contains("Connection reset")
                        || text.contains("Connection timed out")
                        || text.contains("Remote connection closed");
                    let server_dead = self
                        .server
                        .as_ref()
                        .is_some_and(|s| s.exit_code().is_some());
                    if server_dead {
                        return Err(self.server_failure(None));
                    }
                    if !retry || Instant::now() >= deadline {
                        let mut msg =
                            format!("cannot connect to the GDB server at {target}: {text}");
                        if let Some(s) = &self.server {
                            let output = s.recent_output();
                            if let Some(hint) = server::diagnose_server_output(&output) {
                                msg.push_str(&format!(" ({hint})"));
                            }
                        }
                        bail!(msg);
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
    }

    fn run_commands(&mut self, commands: &[String]) -> Result<()> {
        for cmd in commands {
            let timeout = if cmd.trim() == "load" || cmd.starts_with("load ") {
                Duration::from_millis(self.config.load_timeout.unwrap_or(180_000))
            } else {
                Duration::from_secs(30)
            };
            let cmd = cmd.trim();
            let output = if let Some(raw) = cmd.strip_prefix('-') {
                let resp = self.mi_timeout(&format!("-{raw}"), timeout)?;
                resp.console
            } else {
                self.cli(cmd, timeout)
                    .with_context(|| format!("command `{cmd}` failed"))?
            };
            if !output.trim().is_empty() {
                self.out.output(Category::Console, &output);
            }
        }
        Ok(())
    }

    /// GDB caches registers; resets done behind its back need a flush.
    fn flush_registers(&mut self) {
        if self
            .cli("maintenance flush register-cache", Duration::from_secs(5))
            .is_err()
        {
            let _ = self.cli("flushregs", Duration::from_secs(5));
        }
    }

    fn configuration_done(&mut self, req: &Request) -> HandlerResult {
        self.out.respond(req, Value::Null);
        self.configured = true;
        if let Err(e) = self.start_execution() {
            self.out.output(
                Category::Important,
                &format!("Cannot start the program: {e:#}\n"),
            );
            self.report_stop("pause", Some("Error".into()), 1, None);
        }
        Ok(Reply::Responded)
    }

    /// After launch or reset: stop at the entry, run to `main`, or just run.
    fn start_execution(&mut self) -> Result<()> {
        if self.kind == RequestKind::Attach {
            if self.config.continue_on_attach {
                return self.resume("-exec-continue");
            }
            self.report_stop("pause", Some("Attached".into()), 1, None);
            return Ok(());
        }
        if self.config.stop_on_entry {
            self.report_stop("entry", Some("Reset".into()), 1, None);
            return Ok(());
        }
        if let Some(entry) = self.config.entry_point().map(str::to_string) {
            let hw = if self.config.force_hardware_breakpoints {
                " -h"
            } else {
                ""
            };
            match self.mi(&format!("-break-insert -t{hw} {}", mi_quote(&entry))) {
                Ok(resp) => {
                    let number = resp
                        .body
                        .get("bkpt")
                        .and_then(|b| b.str("number"))
                        .map(str::to_string);
                    self.expect = Expect::Entry(number);
                }
                Err(e) => {
                    self.warn(&format!("cannot run to `{entry}`: {e:#}"));
                    self.report_stop("entry", Some("Reset".into()), 1, None);
                    return Ok(());
                }
            }
        }
        self.resume("-exec-continue")
    }

    /// Sends an execution command (`-exec-continue`, `-exec-next`, ...).
    fn resume(&mut self, cmd: &str) -> Result<()> {
        self.require_stopped()?;
        self.insp.reset(self.gdb.as_ref());
        self.resumed_by_us = true;
        let result = self.mi(cmd);
        match result {
            Ok(_) => {
                self.state = TargetState::Running;
                Ok(())
            }
            Err(e) => {
                self.resumed_by_us = false;
                Err(e)
            }
        }
    }

    /// Resumes after an internal stop (log point, unmet hit condition).
    fn resume_silently(&mut self) {
        if let Err(e) = self.resume("-exec-continue") {
            self.warn(&format!("cannot resume: {e:#}"));
            self.report_stop("pause", None, 1, None);
        }
    }

    fn continue_(&mut self, _req: &Request) -> HandlerResult {
        if self.state == TargetState::Running {
            return body(json!({"allThreadsContinued": true}));
        }
        self.resume("-exec-continue")?;
        body(json!({"allThreadsContinued": true}))
    }

    fn pause(&mut self, _req: &Request) -> HandlerResult {
        if self.state != TargetState::Running {
            return body(Value::Null);
        }
        self.expect = Expect::Pause;
        self.mi("-exec-interrupt")?;
        body(Value::Null)
    }

    /// Halts a running target for an internal operation. Returns `true` if
    /// the target was running and must be resumed afterwards.
    fn halt_for_internal_op(&mut self) -> Result<bool> {
        if self.state != TargetState::Running {
            return Ok(false);
        }
        self.mi("-exec-interrupt")?;
        let stopped = self.wait_for(Duration::from_secs(5), |e| {
            matches!(e, Event::Mi(MiEvent::Async { kind: AsyncKind::Exec, class, .. }) if class == "stopped")
        });
        if stopped.is_none() {
            bail!("the target did not halt");
        }
        self.state = TargetState::Stopped;
        Ok(true)
    }

    fn step(&mut self, req: &Request, kind: StepKind) -> HandlerResult {
        let requested_instruction = req.arg_str("granularity") == Some("instruction");
        self.instruction_stepping = requested_instruction;
        let thread = req.arg_i64("threadId").unwrap_or(1);
        let by_instruction = requested_instruction
            || self.asm_forced()
            || self.insp.top_frame_without_source(thread);
        let cmd = match (kind, by_instruction) {
            (StepKind::Over, false) => "-exec-next",
            (StepKind::Over, true) => "-exec-next-instruction",
            (StepKind::In, false) => "-exec-step",
            (StepKind::In, true) => "-exec-step-instruction",
            (StepKind::Out, _) => "-exec-finish",
        };
        self.resume(&format!("{cmd} --thread {thread}"))?;
        body(Value::Null)
    }

    fn restart(&mut self, req: &Request) -> HandlerResult {
        self.reset_target()?;
        self.out.respond(req, Value::Null);
        if let Err(e) = self.start_execution() {
            self.warn(&format!("{e:#}"));
        }
        Ok(Reply::Responded)
    }

    /// Resets (and optionally re-flashes) the target, leaving it halted.
    fn reset_target(&mut self) -> Result<()> {
        if self.gdb.is_none() {
            bail!("the debug session is not started");
        }
        if self.state == TargetState::Running {
            self.expect = Expect::Nothing;
            self.halt_for_internal_op()?;
        }
        self.insp.reset(self.gdb.as_ref());
        let commands = match &self.config.override_restart_commands {
            Some(c) => c.clone(),
            None => {
                let mut cmds = self.config.pre_restart_commands.clone();
                cmds.extend(self.config.reset_commands());
                if self.config.reload_on_restart && self.config.executable_path().is_some() {
                    cmds.push("load".into());
                    cmds.extend(self.config.reset_commands());
                }
                cmds.extend(self.config.post_restart_commands.iter().cloned());
                cmds
            }
        };
        self.run_commands(&commands)?;
        self.flush_registers();
        self.state = TargetState::Stopped;
        self.expect = Expect::Nothing;
        for bp in &mut self.bps {
            bp.hits = 0;
        }
        Ok(())
    }

    fn terminate(&mut self, req: &Request) -> HandlerResult {
        self.out.respond(req, Value::Null);
        self.shutdown();
        self.send_terminated();
        Ok(Reply::Responded)
    }

    fn disconnect(&mut self, req: &Request) -> HandlerResult {
        self.shutdown();
        self.out.respond(req, Value::Null);
        self.exit_loop = true;
        Ok(Reply::Responded)
    }

    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        if let Some(gdb) = &self.gdb {
            if gdb.is_alive() {
                if self.state == TargetState::Running {
                    let _ = gdb.cmd_timeout("-exec-interrupt", Duration::from_secs(1));
                }
                let _ = gdb.cmd_timeout("-target-disconnect", Duration::from_secs(2));
                gdb.send_no_wait("-gdb-exit");
            }
        }
        self.shutdown_processes();
    }

    fn shutdown_processes(&mut self) {
        self.shutting_down = true;
        if let Some(mut child) = self.gdb_child.take() {
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.gdb = None;
        if let Some(server) = self.server.take() {
            server.kill();
        }
        self.state = TargetState::Exited;
    }

    // ----- stops ------------------------------------------------------------

    fn on_stopped(&mut self, body: &Mi) {
        self.state = TargetState::Stopped;
        self.resumed_by_us = false;
        self.insp.reset(self.gdb.as_ref());
        let reason = body.str("reason").unwrap_or("");
        let thread = body.i64("thread-id").unwrap_or(1);
        let expect = std::mem::replace(&mut self.expect, Expect::Nothing);
        let func = body
            .get("frame")
            .and_then(|f| f.str("func"))
            .unwrap_or("")
            .to_string();

        if reason.starts_with("exited") {
            self.state = TargetState::Exited;
            let code = body.u64("exit-code").unwrap_or(0);
            self.out.event("exited", json!({"exitCode": code}));
            self.send_terminated();
            return;
        }

        let bkpt_number = body.str("bkptno").map(str::to_string).or_else(|| {
            ["wpt", "hw-rwpt", "hw-awpt"].iter().find_map(|k| {
                body.get(k)
                    .and_then(|w| w.str("number"))
                    .map(str::to_string)
            })
        });

        if let Expect::Entry(Some(number)) = &expect {
            if bkpt_number.as_deref() == Some(number.as_str()) {
                self.report_stop("entry", None, thread, None);
                return;
            }
        }

        let mut stop_reason = match reason {
            "breakpoint-hit" => "breakpoint",
            "watchpoint-trigger" | "read-watchpoint-trigger" | "access-watchpoint-trigger" => {
                "data breakpoint"
            }
            "end-stepping-range" | "function-finished" | "location-reached" => "step",
            "signal-received" => {
                let signal = body.str("signal-name").unwrap_or("");
                if expect == Expect::Pause || matches!(signal, "SIGINT" | "SIGTRAP" | "0") {
                    "pause"
                } else {
                    "exception"
                }
            }
            _ => "pause",
        }
        .to_string();
        let mut description: Option<String> = None;
        let mut text: Option<String> = None;
        let mut hit_ids = Vec::new();

        if let Some(number) = &bkpt_number {
            match self.on_breakpoint_hit(number, body) {
                breakpoints::HitAction::Resume => {
                    self.resume_silently();
                    return;
                }
                breakpoints::HitAction::Stop { id, kind } => {
                    hit_ids.push(id);
                    match kind {
                        breakpoints::BpClass::Function => {
                            stop_reason = "function breakpoint".into()
                        }
                        breakpoints::BpClass::Instruction => {
                            stop_reason = "instruction breakpoint".into()
                        }
                        breakpoints::BpClass::Data => {
                            stop_reason = "data breakpoint".into();
                            if let Some(v) = body.get("value") {
                                let old = v.str("old").unwrap_or("?");
                                let new = v.str("new").or_else(|| v.str("value")).unwrap_or("?");
                                let exp = ["wpt", "hw-rwpt", "hw-awpt"]
                                    .iter()
                                    .find_map(|k| body.get(k).and_then(|w| w.str("exp")))
                                    .unwrap_or("");
                                description = Some(if v.str("old").is_some() {
                                    format!("{exp}: {old} -> {new}")
                                } else {
                                    format!("{exp} = {new}")
                                });
                            }
                        }
                        breakpoints::BpClass::Fault => stop_reason = "exception".into(),
                        breakpoints::BpClass::Source => {}
                    }
                }
                breakpoints::HitAction::Unknown => {}
            }
        }

        // Explain faults when stopping because of one (fault filter, signal)
        // or when a pause finds the CPU stuck in a fault handler, but not on
        // every step through a handler.
        let in_fault_handler = crate::cortex::FAULT_HANDLERS.contains(&func.as_str());
        if stop_reason == "exception" || (stop_reason == "pause" && in_fault_handler) {
            let report = self.fault_report();
            if !report.is_empty() {
                let exception = self
                    .current_exception_name()
                    .unwrap_or_else(|| "Fault".into());
                stop_reason = "exception".into();
                description = Some(exception.clone());
                let joined = report.join("\n");
                self.out.output(
                    Category::Important,
                    &format!("{exception} detected:\n{joined}\n"),
                );
                text = Some(joined);
            }
        }
        self.report(StopInfo {
            reason: stop_reason,
            description,
            thread,
            text,
            hit_ids,
        });
    }

    fn report_stop(
        &mut self,
        reason: &str,
        description: Option<String>,
        thread: i64,
        text: Option<String>,
    ) {
        self.report(StopInfo {
            reason: reason.to_string(),
            description,
            thread,
            text,
            hit_ids: Vec::new(),
        });
    }

    fn report(&mut self, info: StopInfo) {
        self.state = TargetState::Stopped;
        self.insp.reset(self.gdb.as_ref());
        self.send_stopped(&info, false);
        self.last_stop = Some(info);
    }

    fn send_stopped(&self, info: &StopInfo, preserve_focus: bool) {
        let mut b = json!({
            "reason": info.reason,
            "threadId": info.thread,
            "allThreadsStopped": true,
            "preserveFocusHint": preserve_focus,
        });
        if let Some(d) = &info.description {
            b["description"] = json!(d);
        }
        if let Some(t) = &info.text {
            b["text"] = json!(t);
        }
        if !info.hit_ids.is_empty() {
            b["hitBreakpointIds"] = json!(info.hit_ids);
        }
        self.out.event("stopped", b);
    }

    /// Makes the client re-fetch threads, frames and variables (used after
    /// toggling display options).
    fn refresh_client(&mut self) {
        if self.state != TargetState::Stopped {
            return;
        }
        self.insp.reset(self.gdb.as_ref());
        if let Some(info) = self.last_stop.clone() {
            self.send_stopped(&info, false);
        }
    }

    /// Whether the top frame should show disassembly right now.
    fn asm_view_active(&self) -> bool {
        use crate::config::DisassemblyMode;
        match self.asm_override {
            Some(v) => v,
            None => match self.config.show_disassembly {
                DisassemblyMode::Always => true,
                DisassemblyMode::Never => false,
                DisassemblyMode::Auto => self.instruction_stepping,
            },
        }
    }

    /// The disassembly view was forced on, so steps go by instruction.
    fn asm_forced(&self) -> bool {
        self.asm_override == Some(true)
            || (self.asm_override.is_none()
                && self.config.show_disassembly == crate::config::DisassemblyMode::Always)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Over,
    In,
    Out,
}

/// Runs a session over stdin/stdout with real processes.
pub fn run_stdio() {
    let (session, tx) = Session::new(Box::new(std::io::stdout()), Box::new(ProcessBackend));
    Session::spawn_dap_reader(Box::new(std::io::stdin()), tx);
    session.run();
}
