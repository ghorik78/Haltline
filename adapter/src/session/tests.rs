//! Session tests against a scripted, in-process fake GDB.

use super::*;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};

type Handler = Box<dyn FnMut(&str) -> Option<Vec<String>> + Send>;

#[derive(Clone, Default)]
struct Script {
    /// Every command GDB received (without tokens).
    log: Arc<Mutex<Vec<String>>>,
    /// Records emitted after the next execution command.
    stops: Arc<Mutex<VecDeque<String>>>,
    /// Test-specific responses, tried before the defaults.
    custom: Arc<Mutex<Option<Handler>>>,
}

impl Script {
    fn commands(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn count(&self, needle: &str) -> usize {
        self.commands()
            .iter()
            .filter(|c| c.contains(needle))
            .count()
    }

    /// Waits until GDB has received a command equal to `cmd`.
    fn wait_command(&self, cmd: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.commands().iter().any(|c| c == cmd) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn push_stop(&self, record: &str) {
        self.stops.lock().unwrap().push_back(record.to_string());
    }

    fn on(&self, handler: impl FnMut(&str) -> Option<Vec<String>> + Send + 'static) {
        *self.custom.lock().unwrap() = Some(Box::new(handler));
    }
}

fn arg_after<'a>(cmd: &'a str, flag: &str) -> Option<&'a str> {
    let mut it = cmd.split_whitespace();
    while let Some(w) = it.next() {
        if w == flag {
            return it.next();
        }
    }
    None
}

fn default_response(
    script: &Script,
    cmd: &str,
    next_bp: &mut u32,
    next_var: &mut u32,
) -> Vec<String> {
    let s = |x: &str| x.to_string();
    let word = cmd.split_whitespace().next().unwrap_or("");
    match word {
        "-gdb-version" => vec![s("~\"GNU gdb (Fake) 15.1\\n\""), s("^done")],
        "-target-select" => vec![
            s("*stopped,frame={addr=\"0x08000040\",func=\"Reset_Handler\"},thread-id=\"1\",stopped-threads=\"all\""),
            s("^connected"),
        ],
        "-break-insert" => {
            *next_bp += 1;
            let line = arg_after(cmd, "--line").unwrap_or("10");
            let func = if cmd.contains("factorial") { "factorial" } else { "main" };
            vec![format!(
                "^done,bkpt={{number=\"{next_bp}\",type=\"breakpoint\",disp=\"keep\",enabled=\"y\",addr=\"0x08000100\",func=\"{func}\",file=\"main.c\",fullname=\"/fw/main.c\",line=\"{line}\",times=\"0\"}}"
            )]
        }
        "-break-watch" => {
            *next_bp += 1;
            vec![format!("^done,wpt={{number=\"{next_bp}\",exp=\"g_counter\"}}")]
        }
        "-exec-continue" | "-exec-next" | "-exec-step" | "-exec-finish" | "-exec-next-instruction"
        | "-exec-step-instruction" => {
            let mut out = vec![s("^running"), s("*running,thread-id=\"all\"")];
            if let Some(stop) = script.stops.lock().unwrap().pop_front() {
                out.push(stop);
            }
            out
        }
        "-exec-interrupt" => vec![
            s("^done"),
            s("*stopped,reason=\"signal-received\",signal-name=\"SIGINT\",frame={addr=\"0x08000110\",func=\"main\"},thread-id=\"1\",stopped-threads=\"all\""),
        ],
        "-data-evaluate-expression" => vec![s("^done,value=\"42\"")],
        "-var-create" => {
            *next_var += 1;
            vec![format!("^done,name=\"var{next_var}\",numchild=\"0\",value=\"42\",type=\"int\",has_more=\"0\"")]
        }
        "-thread-info" => vec![s("^done,threads=[{id=\"1\",target-id=\"Remote target\",state=\"stopped\"}],current-thread-id=\"1\"")],
        "-stack-info-depth" => vec![s("^done,depth=\"2\"")],
        "-stack-list-frames" => vec![s(
            "^done,stack=[frame={level=\"0\",addr=\"0x08000110\",func=\"main\",file=\"main.c\",fullname=\"/nonexistent/main.c\",line=\"12\"},frame={level=\"1\",addr=\"0x08000050\",func=\"Reset_Handler\"}]",
        )],
        "-data-disassemble" => vec![s(
            "^done,asm_insns=[{address=\"0x08000110\",func-name=\"main\",offset=\"0\",opcodes=\"00 bf\",inst=\"nop\"},{address=\"0x08000112\",func-name=\"main\",offset=\"2\",opcodes=\"fe e7\",inst=\"b.n\\t0x8000112 <main+2>\"}]",
        )],
        "-stack-list-variables" => vec![s("^done,variables=[{name=\"i\"},{name=\"n\",arg=\"1\"}]")],
        "-gdb-exit" => vec![s("^exit")],
        _ => vec![s("^done")],
    }
}

struct FakeBackend {
    script: Script,
}

impl Backend for FakeBackend {
    fn spawn_gdb(&mut self, _program: &Path, _args: &[String], _cwd: &Path) -> Result<GdbProcess> {
        let (cmd_reader, cmd_writer) = std::io::pipe()?;
        let (out_reader, mut out_writer) = std::io::pipe()?;
        let script = self.script.clone();
        std::thread::spawn(move || {
            let mut next_bp = 0;
            let mut next_var = 0;
            for line in BufReader::new(cmd_reader).lines() {
                let Ok(line) = line else { break };
                let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
                let (token, cmd) = line.split_at(digits);
                script.log.lock().unwrap().push(cmd.to_string());
                let custom = script.custom.lock().unwrap().as_mut().and_then(|h| h(cmd));
                let lines = custom
                    .unwrap_or_else(|| default_response(&script, cmd, &mut next_bp, &mut next_var));
                for l in &lines {
                    let l = if l.starts_with('^') {
                        format!("{token}{l}")
                    } else {
                        l.clone()
                    };
                    if writeln!(out_writer, "{l}").is_err() {
                        return;
                    }
                }
                let _ = writeln!(out_writer, "(gdb) ");
                let _ = out_writer.flush();
                if cmd.starts_with("-gdb-exit") {
                    return;
                }
            }
        });
        Ok(GdbProcess {
            stdin: Box::new(cmd_writer),
            stdout: Box::new(out_reader),
            stderr: None,
            child: None,
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

struct Harness {
    tx: Sender<Event>,
    rx: Receiver<Value>,
    seq: i64,
    queue: VecDeque<Value>,
    script: Script,
    output: String,
    _elf: tempfile::TempDir,
    elf: String,
}

const WAIT: Duration = Duration::from_secs(10);

impl Harness {
    fn new() -> Self {
        let script = Script::default();
        let (out_reader, out_writer) = std::io::pipe().unwrap();
        let (session, tx) = Session::new(
            Box::new(out_writer),
            Box::new(FakeBackend {
                script: script.clone(),
            }),
        );
        std::thread::spawn(move || session.run());
        let (mtx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out_reader);
            while let Ok(Some(msg)) = crate::dap::read_message(&mut reader) {
                if mtx.send(msg).is_err() {
                    break;
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("fw.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();
        Self {
            tx,
            rx,
            seq: 1,
            queue: VecDeque::new(),
            script,
            output: String::new(),
            elf: elf.to_string_lossy().into_owned(),
            _elf: dir,
        }
    }

    fn next_message(&mut self, timeout: Duration) -> Option<Value> {
        let msg = self.rx.recv_timeout(timeout).ok()?;
        if msg["event"] == "output" {
            self.output
                .push_str(msg["body"]["output"].as_str().unwrap_or(""));
        }
        Some(msg)
    }

    fn request_raw(&mut self, command: &str, args: Value) -> Value {
        let seq = self.seq;
        self.seq += 1;
        self.tx
            .send(Event::Dap(
                json!({"seq": seq, "type": "request", "command": command, "arguments": args}),
            ))
            .unwrap();
        loop {
            let msg = self
                .next_message(WAIT)
                .unwrap_or_else(|| panic!("no response to {command}; output:\n{}", self.output));
            if msg["type"] == "response" && msg["request_seq"] == seq {
                return msg;
            }
            self.queue.push_back(msg);
        }
    }

    fn request(&mut self, command: &str, args: Value) -> Value {
        let resp = self.request_raw(command, args);
        assert_eq!(resp["success"], true, "{command}: {}", resp["message"]);
        resp.get("body").cloned().unwrap_or(Value::Null)
    }

    fn event(&mut self, name: &str) -> Option<Value> {
        if let Some(pos) = self.queue.iter().position(|m| m["event"] == name) {
            return self.queue.remove(pos).map(|m| m["body"].clone());
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while let Some(msg) = self.next_message(deadline.saturating_duration_since(Instant::now()))
        {
            if msg["event"] == name {
                return Some(msg["body"].clone());
            }
            self.queue.push_back(msg);
        }
        None
    }

    /// Asserts that no event with this name arrives within a short time.
    fn no_event(&mut self, name: &str) {
        assert!(
            self.queue.iter().all(|m| m["event"] != name),
            "unexpected {name}"
        );
        let deadline = Instant::now() + Duration::from_millis(300);
        while let Some(msg) = self.next_message(deadline.saturating_duration_since(Instant::now()))
        {
            assert_ne!(msg["event"], name, "unexpected {name}: {msg}");
            self.queue.push_back(msg);
        }
    }

    fn launch(&mut self, extra: Value) {
        self.request("initialize", json!({"adapterID": "haltline"}));
        let mut config = json!({
            "executable": self.elf,
            "serverType": "external",
            "gdbTarget": "localhost:3333",
            "gdbPath": "fake-gdb",
        });
        for (k, v) in extra.as_object().unwrap() {
            config[k] = v.clone();
        }
        let command = if config["request"] == "attach" {
            "attach"
        } else {
            "launch"
        };
        self.request(command, config);
        assert!(self.event("initialized").is_some());
    }

    /// configurationDone with a scripted stop at the entry breakpoint.
    fn start_at_main(&mut self) {
        // The temporary entry breakpoint is the first one inserted.
        let n = self.script.count("-break-insert") + 1;
        self.script.push_stop(&format!(
            "*stopped,reason=\"breakpoint-hit\",disp=\"del\",bkptno=\"{n}\",frame={{addr=\"0x08000100\",func=\"main\"}},thread-id=\"1\",stopped-threads=\"all\""
        ));
        self.request("configurationDone", json!({}));
        let stop = self.event("stopped").expect("stopped at main");
        assert_eq!(stop["reason"], "entry");
    }
}

fn hit(number: u32) -> String {
    format!(
        "*stopped,reason=\"breakpoint-hit\",disp=\"keep\",bkptno=\"{number}\",frame={{addr=\"0x08000100\",func=\"main\"}},thread-id=\"1\",stopped-threads=\"all\""
    )
}

#[test]
fn initialize_advertises_capabilities() {
    let mut h = Harness::new();
    let caps = h.request("initialize", json!({"adapterID": "haltline"}));
    for key in [
        "supportsConfigurationDoneRequest",
        "supportsConditionalBreakpoints",
        "supportsHitConditionalBreakpoints",
        "supportsLogPoints",
        "supportsFunctionBreakpoints",
        "supportsInstructionBreakpoints",
        "supportsDataBreakpoints",
        "supportsSteppingGranularity",
        "supportsDisassembleRequest",
        "supportsReadMemoryRequest",
        "supportsRestartRequest",
    ] {
        assert_eq!(caps[key], true, "{key}");
    }
    assert_eq!(caps["exceptionBreakpointFilters"][0]["filter"], "faults");
}

#[test]
fn launch_runs_the_init_sequence_and_runs_to_main() {
    let mut h = Harness::new();
    h.launch(json!({"preLaunchCommands": ["monitor arm semihosting enable"]}));
    h.start_at_main();
    let cmds = h.script.commands();
    let pos = |needle: &str| {
        cmds.iter()
            .position(|c| c.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` not sent: {cmds:#?}"))
    };
    assert!(pos("-gdb-set mi-async on") < pos("-file-exec-and-symbols"));
    assert!(pos("-file-exec-and-symbols") < pos("-target-select extended-remote localhost:3333"));
    assert!(pos("-target-select") < pos("monitor arm semihosting enable"));
    assert!(pos("monitor arm semihosting enable") < pos("monitor reset halt"));
    assert!(pos("monitor reset halt") < pos("-target-download"));
    assert!(pos("-break-insert -t \"main\"") < pos("-exec-continue"));
    // The *stopped record produced while connecting is not reported.
    assert_eq!(h.script.count("-exec-continue"), 1);
}

#[cfg(unix)]
#[test]
fn openocd_launch_waits_for_the_server_and_enables_semihosting() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::new();
    let script = h._elf.path().join("fake-openocd.sh");
    let pid_file = h._elf.path().join("server.pid");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > {}\necho 'Info : Listening on port 3333 for gdb connections' >&2\nexec sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    h.launch(json!({
        "serverType": "openocd",
        "serverPath": script,
        "configFiles": ["interface/stlink.cfg", "target/stm32f4x.cfg"],
        "port": 3333,
        "semihosting": true,
    }));
    assert!(h.output.contains("Listening on port 3333"), "{}", h.output);
    let cmds = h.script.commands();
    let pos = |needle: &str| cmds.iter().position(|c| c.contains(needle)).unwrap();
    assert!(cmds[pos("-target-select")].ends_with("localhost:3333"));
    assert!(pos("monitor reset halt") < pos("monitor arm semihosting enable"));
    // The server is stopped together with the session.
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    h.request("disconnect", json!({}));
    let alive = || std::path::Path::new(&format!("/proc/{pid}")).exists();
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if cfg!(target_os = "linux") {
        assert!(!alive(), "the GDB server is still running");
    }
}

#[test]
fn stop_on_entry_and_attach_do_not_run() {
    let mut h = Harness::new();
    h.launch(json!({"stopOnEntry": true}));
    h.request("configurationDone", json!({}));
    assert_eq!(h.event("stopped").unwrap()["reason"], "entry");
    assert_eq!(h.script.count("-exec-continue"), 0);

    let mut h = Harness::new();
    h.launch(json!({"request": "attach"}));
    h.request("configurationDone", json!({}));
    let stop = h.event("stopped").unwrap();
    assert_eq!(stop["reason"], "pause");
    assert_eq!(stop["description"], "Attached");
    assert_eq!(h.script.count("monitor reset"), 0);
    assert_eq!(h.script.count("-target-download"), 0);
}

#[test]
fn hit_conditions_resume_until_met() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    let bps = h.request(
        "setBreakpoints",
        json!({"source": {"path": "/fw/main.c"}, "breakpoints": [{"line": 12, "hitCondition": "==3", "condition": "i > 0"}]}),
    );
    assert_eq!(bps["breakpoints"][0]["verified"], true);
    let insert = h
        .script
        .commands()
        .into_iter()
        .find(|c| c.contains("--line 12"))
        .unwrap();
    assert!(insert.contains("-c \"i > 0\""), "{insert}");
    let number = h.script.count("-break-insert") as u32;
    for _ in 0..3 {
        h.script.push_stop(&hit(number));
    }
    h.request("continue", json!({"threadId": 1}));
    let stop = h.event("stopped").unwrap();
    assert_eq!(stop["reason"], "breakpoint");
    assert_eq!(stop["hitBreakpointIds"][0], bps["breakpoints"][0]["id"]);
    // Entry + user continue + two silent resumes.
    assert_eq!(h.script.count("-exec-continue"), 4);
}

#[test]
fn log_points_print_and_continue() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request(
        "setBreakpoints",
        json!({"source": {"path": "/fw/main.c"}, "breakpoints": [{"line": 20, "logMessage": "value={x} {{raw}}"}]}),
    );
    let number = h.script.count("-break-insert") as u32;
    h.script.push_stop(&hit(number));
    h.script
        .push_stop("*stopped,reason=\"end-stepping-range\",thread-id=\"1\"");
    h.request("continue", json!({"threadId": 1}));
    // The log point resumes; the following (scripted) stop is a step.
    let stop = h.event("stopped").unwrap();
    assert_eq!(stop["reason"], "step");
    assert!(h.output.contains("value=42 {raw}\n"), "{}", h.output);
    assert!(h
        .script
        .commands()
        .iter()
        .any(|c| c == "-data-evaluate-expression \"x\""));
}

#[test]
fn breakpoints_are_updated_incrementally() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    let path = json!({"path": "/fw/main.c"});
    let first = h.request(
        "setBreakpoints",
        json!({"source": path, "breakpoints": [{"line": 5}, {"line": 7}]}),
    );
    let inserted = h.script.count("-break-insert");
    let second = h.request(
        "setBreakpoints",
        json!({"source": path, "breakpoints": [{"line": 7}, {"line": 9}]}),
    );
    assert_eq!(
        h.script.count("-break-insert"),
        inserted + 1,
        "line 7 is kept"
    );
    assert_eq!(
        first["breakpoints"][1]["id"],
        second["breakpoints"][0]["id"]
    );
    assert_eq!(h.script.count("-break-delete"), 1, "line 5 is deleted");
    h.request("setBreakpoints", json!({"source": path, "breakpoints": []}));
    assert_eq!(h.script.count("-break-delete"), 3);
}

#[test]
fn breakpoint_errors_are_reported_per_breakpoint() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.script.on(|cmd| {
        (cmd.contains("--line 99"))
            .then(|| vec!["^error,msg=\"No line 99 in file \\\"main.c\\\".\"".to_string()])
    });
    let bps = h.request(
        "setBreakpoints",
        json!({"source": {"path": "/fw/main.c"}, "breakpoints": [{"line": 99}, {"line": 3, "hitCondition": "sometimes"}]}),
    );
    assert_eq!(bps["breakpoints"][0]["verified"], false);
    assert!(bps["breakpoints"][0]["message"]
        .as_str()
        .unwrap()
        .contains("No line 99"));
    assert_eq!(bps["breakpoints"][1]["verified"], false);
    assert!(bps["breakpoints"][1]["message"]
        .as_str()
        .unwrap()
        .contains("hit condition"));
}

#[test]
fn breakpoints_set_while_running_pause_and_resume() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request("continue", json!({"threadId": 1}));
    let before = h.script.count("-exec-continue");
    h.request(
        "setBreakpoints",
        json!({"source": {"path": "/fw/main.c"}, "breakpoints": [{"line": 4}]}),
    );
    assert_eq!(h.script.count("-exec-interrupt"), 1);
    assert_eq!(h.script.count("-exec-continue"), before + 1);
    h.no_event("stopped");
}

#[test]
fn stops_caused_by_evaluation_are_ignored() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request("stackTrace", json!({"threadId": 1}));
    // An inferior function call: GDB runs and stops the target before answering.
    h.script.on(|cmd| {
        cmd.starts_with("-var-create").then(|| {
            vec![
                "*running,thread-id=\"all\"".to_string(),
                "*stopped,reason=\"function-finished\",thread-id=\"1\"".to_string(),
                "^done,name=\"var9\",numchild=\"0\",value=\"7\",type=\"int\"".to_string(),
            ]
        })
    });
    let result = h.request(
        "evaluate",
        json!({"expression": "compute()", "context": "watch"}),
    );
    assert_eq!(result["result"], "7");
    h.no_event("stopped");
    h.no_event("continued");
}

#[test]
fn pause_reports_pause() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request("continue", json!({"threadId": 1}));
    h.request("pause", json!({"threadId": 1}));
    assert_eq!(h.event("stopped").unwrap()["reason"], "pause");
}

#[test]
fn instruction_granularity_uses_instruction_steps_and_disassembly() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.script
        .push_stop("*stopped,reason=\"end-stepping-range\",thread-id=\"1\"");
    h.request("next", json!({"threadId": 1, "granularity": "instruction"}));
    h.event("stopped").unwrap();
    assert_eq!(h.script.count("-exec-next-instruction --thread 1"), 1);
    let st = h.request("stackTrace", json!({"threadId": 1}));
    let path = st["stackFrames"][0]["source"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(path.ends_with(".s"), "{st}");
    let text = std::fs::read_to_string(&path).unwrap();
    let line = st["stackFrames"][0]["line"].as_u64().unwrap() as usize;
    assert!(text.lines().nth(line - 1).unwrap().contains("0x08000110"));
    // The source of the fake frame does not exist, so line steps also go by
    // instruction until a frame with source is reached.
    h.script
        .push_stop("*stopped,reason=\"end-stepping-range\",thread-id=\"1\"");
    h.request("stepIn", json!({"threadId": 1}));
    h.event("stopped").unwrap();
    assert_eq!(h.script.count("-exec-step-instruction --thread 1"), 1);
}

#[test]
fn console_commands() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request("stackTrace", json!({"threadId": 1}));
    h.script.on(|cmd| {
        cmd.starts_with("-interpreter-exec console \"info registers\"")
            .then(|| vec!["~\"r0 0x1 1\\n\"".to_string(), "^done".to_string()])
    });
    let out = h.request(
        "evaluate",
        json!({"expression": "info registers", "context": "repl"}),
    );
    assert_eq!(out["result"], "r0 0x1 1");
    let out = h.request(
        "evaluate",
        json!({"expression": ":help", "context": "repl"}),
    );
    assert!(out["result"].as_str().unwrap().contains(":asm"));
    let out = h.request(
        "evaluate",
        json!({"expression": "g_counter", "context": "repl"}),
    );
    assert_eq!(out["result"], "42");
    let resp = h.request_raw(
        "evaluate",
        json!({"expression": ":bogus", "context": "repl"}),
    );
    assert_eq!(resp["success"], false);
}

#[test]
fn data_breakpoints_use_watchpoints() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    let info = h.request("dataBreakpointInfo", json!({"name": "g_counter"}));
    assert_eq!(info["accessTypes"], json!(["write", "read", "readWrite"]));
    let wp = h.request(
        "setDataBreakpoints",
        json!({"breakpoints": [{"dataId": "g_counter", "accessType": "readWrite", "condition": "g_counter > 5"}]}),
    );
    assert_eq!(wp["breakpoints"][0]["verified"], true);
    assert!(h
        .script
        .commands()
        .iter()
        .any(|c| c == "-break-watch -a \"g_counter\""));
    assert!(h
        .script
        .commands()
        .iter()
        .any(|c| c.starts_with("-break-condition") && c.ends_with("g_counter > 5")));
    let addr = h.request(
        "dataBreakpointInfo",
        json!({"name": "0x20000000", "asAddress": true, "bytes": 2}),
    );
    assert_eq!(addr["dataId"], "*(unsigned short *) 0x20000000");
}

#[test]
fn connection_is_retried_while_the_server_starts() {
    let mut h = Harness::new();
    let attempts = Arc::new(Mutex::new(0));
    let a = attempts.clone();
    h.script.on(move |cmd| {
        if !cmd.starts_with("-target-select") {
            return None;
        }
        let mut n = a.lock().unwrap();
        *n += 1;
        (*n < 3).then(|| vec!["^error,msg=\"localhost:3333: Connection refused.\"".to_string()])
    });
    h.launch(json!({}));
    assert_eq!(*attempts.lock().unwrap(), 3);
}

#[test]
fn launch_errors_are_reported() {
    let mut h = Harness::new();
    h.request("initialize", json!({}));
    let resp = h.request_raw(
        "launch",
        json!({"serverType": "openocd", "executable": h.elf.clone()}),
    );
    assert_eq!(resp["success"], false);
    assert!(resp["message"].as_str().unwrap().contains("configFiles"));

    let mut h = Harness::new();
    h.request("initialize", json!({}));
    h.script.on(|cmd| {
        cmd.starts_with("-target-select").then(|| {
            vec![
                "^error,msg=\"Remote replied unexpectedly to 'vMustReplyEmpty': timeout\""
                    .to_string(),
            ]
        })
    });
    let resp = h.request_raw(
        "launch",
        json!({"serverType": "external", "gdbTarget": "1.2.3.4:5", "executable": h.elf.clone(), "gdbPath": "fake"}),
    );
    assert_eq!(resp["success"], false);
    assert!(
        resp["message"].as_str().unwrap().contains("cannot connect"),
        "{resp}"
    );
}

#[test]
fn unknown_keys_produce_a_warning() {
    let mut h = Harness::new();
    h.launch(json!({"svdfile": "x.svd"}));
    assert!(
        h.output
            .contains("unknown configuration keys ignored: svdfile"),
        "{}",
        h.output
    );
}

#[cfg(unix)]
#[test]
fn server_start_failures_include_its_output() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::new();
    let script = h._elf.path().join("fake-openocd.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'Error: open failed' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    h.request("initialize", json!({}));
    let resp = h.request_raw(
        "launch",
        json!({
            "serverType": "openocd",
            "serverPath": script,
            "configFiles": ["dummy.cfg"],
            "executable": h.elf.clone(),
            "gdbPath": "fake",
        }),
    );
    assert_eq!(resp["success"], false);
    let message = resp["message"].as_str().unwrap();
    assert!(
        message.contains("OpenOCD failed to start (exit code 1)"),
        "{message}"
    );
    assert!(message.contains("probe was not found"), "{message}");
    assert!(message.contains("Error: open failed"), "{message}");
}

#[test]
fn restart_resets_and_runs_to_main_again() {
    let mut h = Harness::new();
    h.launch(json!({"reloadOnRestart": true}));
    h.start_at_main();
    let loads = h.script.count("-target-download");
    let n = h.script.count("-break-insert") + 1;
    h.script.push_stop(&format!(
        "*stopped,reason=\"breakpoint-hit\",disp=\"del\",bkptno=\"{n}\",thread-id=\"1\""
    ));
    h.request("restart", json!({}));
    assert_eq!(h.event("stopped").unwrap()["reason"], "entry");
    assert_eq!(h.script.count("-target-download"), loads + 1);
    assert!(h.script.count("monitor reset halt") >= 3);
}

#[test]
fn disconnect_shuts_gdb_down() {
    let mut h = Harness::new();
    h.launch(json!({}));
    h.start_at_main();
    h.request("disconnect", json!({}));
    assert!(h
        .script
        .commands()
        .iter()
        .any(|c| c == "-target-disconnect"));
    assert!(h.script.wait_command("-gdb-exit"));
}

#[test]
fn flashing_prints_stages_while_it_runs() {
    let mut h = Harness::new();
    h.script.on(|cmd| {
        if cmd != "-target-download" {
            return None;
        }
        // The server erases the flash before GDB reports anything.
        std::thread::sleep(Duration::from_millis(600));
        Some(vec![
            "+download,{section=\".text\",section-size=\"996\",total-size=\"23660\"}".into(),
            "@\"semihosting output\\n\"".into(),
            "^done,address=\"0x08000040\",load-size=\"996\",transferred-bits=\"7968\"".into(),
        ])
    });
    h.launch(json!({}));
    let out = h.output.clone();
    let pos = |needle: &str| {
        out.find(needle)
            .unwrap_or_else(|| panic!("`{needle}` missing in:\n{out}"))
    };
    assert!(pos("Flashing fw.elf") < pos("Flashing: erasing flash..."));
    assert!(pos("Flashing: erasing flash...") < pos("Flashed 996 B in"));
    assert!(pos("semihosting output") < pos("Flashed 996 B in"));
    assert_eq!(h.script.count("compare-sections"), 0);
}

#[test]
fn verify_after_load_compares_sections() {
    let mut h = Harness::new();
    h.launch(json!({"verifyAfterLoad": true}));
    let cmds = h.script.commands();
    let pos = |needle: &str| cmds.iter().position(|c| c.contains(needle)).unwrap();
    assert!(pos("-target-download") < pos("compare-sections"));
    assert!(h.output.contains("Flashed and verified"), "{}", h.output);

    let mut h = Harness::new();
    h.script.on(|cmd| {
        cmd.contains("compare-sections").then(|| {
            vec![
                "~\"Section .text, range 0x8000040 -- 0x80003d8: MIS-MATCHED!\\n\"".into(),
                "^error,msg=\"One or more sections of the target image does not match the loaded file\""
                    .into(),
            ]
        })
    });
    h.request("initialize", json!({}));
    let resp = h.request_raw(
        "launch",
        json!({"serverType": "external", "gdbTarget": "localhost:3333",
               "executable": h.elf.clone(), "gdbPath": "fake", "verifyAfterLoad": true}),
    );
    assert_eq!(resp["success"], false);
    let message = resp["message"].as_str().unwrap();
    assert!(message.contains("flash verification failed"), "{message}");
    assert!(message.contains("does not match"), "{message}");
}

#[test]
fn terminate_during_flashing_stops_the_session() {
    let mut h = Harness::new();
    // GDB never finishes the download.
    h.script
        .on(|cmd| (cmd == "-target-download").then(Vec::new));
    h.request("initialize", json!({}));
    let send = |h: &mut Harness, command: &str, args: Value| {
        let seq = h.seq;
        h.seq += 1;
        h.tx.send(Event::Dap(
            json!({"seq": seq, "type": "request", "command": command, "arguments": args}),
        ))
        .unwrap();
        seq
    };
    let config = json!({"serverType": "external", "gdbTarget": "localhost:3333",
                        "executable": h.elf.clone(), "gdbPath": "fake"});
    let launch = send(&mut h, "launch", config);
    assert!(h.script.wait_command("-target-download"));
    let terminate = send(&mut h, "terminate", json!({}));
    let started = Instant::now();
    let mut launch_resp = None;
    let mut terminate_resp = None;
    while launch_resp.is_none() || terminate_resp.is_none() {
        let msg = h
            .next_message(WAIT)
            .expect("responses to launch and terminate");
        if msg["type"] == "response" && msg["request_seq"] == launch {
            launch_resp = Some(msg);
        } else if msg["type"] == "response" && msg["request_seq"] == terminate {
            terminate_resp = Some(msg);
        } else {
            h.queue.push_back(msg);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    let launch_resp = launch_resp.unwrap();
    assert_eq!(launch_resp["success"], false);
    assert!(
        launch_resp["message"]
            .as_str()
            .unwrap()
            .contains("stopped by the user"),
        "{launch_resp}"
    );
    assert_eq!(terminate_resp.unwrap()["success"], true);
    assert!(h.event("terminated").is_some());
}
