//! A small DAP client that drives the `haltline-dap` binary over stdio.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(60);

pub struct DapClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    seq: i64,
    queue: VecDeque<Value>,
    output: String,
}

impl DapClient {
    pub fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_haltline-dap"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start haltline-dap");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = haltline_dap::dap::read_message(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            rx,
            seq: 1,
            queue: VecDeque::new(),
            output: String::new(),
        }
    }

    fn recv(&mut self, deadline: Instant) -> Option<Value> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = self.rx.recv_timeout(remaining).ok()?;
        if msg["type"] == "event" && msg["event"] == "output" {
            let text = msg["body"]["output"].as_str().unwrap_or("");
            self.output.push_str(text);
            if std::env::var_os("HALTLINE_TEST_VERBOSE").is_some() {
                eprint!("[adapter] {text}");
            }
        }
        Some(msg)
    }

    pub fn send(&mut self, command: &str, arguments: Value) -> i64 {
        let seq = self.seq;
        self.seq += 1;
        let msg =
            json!({"seq": seq, "type": "request", "command": command, "arguments": arguments});
        haltline_dap::dap::write_message(&mut self.stdin, &msg).expect("write request");
        self.stdin.flush().unwrap();
        seq
    }

    /// Sends a request and returns the whole response message.
    pub fn request_raw(&mut self, command: &str, arguments: Value) -> Value {
        let seq = self.send(command, arguments);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let msg = self
                .recv(deadline)
                .unwrap_or_else(|| panic!("no response to `{command}`\noutput:\n{}", self.output));
            if msg["type"] == "response" && msg["request_seq"] == seq {
                return msg;
            }
            if msg["type"] == "event" {
                self.queue.push_back(msg);
            }
        }
    }

    /// Sends a request, asserts success and returns the body.
    pub fn request(&mut self, command: &str, arguments: Value) -> Value {
        let resp = self.request_raw(command, arguments);
        assert_eq!(
            resp["success"], true,
            "`{command}` failed: {}\noutput:\n{}",
            resp["message"], self.output
        );
        resp.get("body").cloned().unwrap_or(Value::Null)
    }

    /// Waits for an event (events received earlier are checked first).
    pub fn wait_event(&mut self, name: &str) -> Value {
        self.wait_event_timeout(name, TIMEOUT)
            .unwrap_or_else(|| panic!("no `{name}` event\noutput:\n{}", self.output))
    }

    pub fn wait_event_timeout(&mut self, name: &str, timeout: Duration) -> Option<Value> {
        if let Some(pos) = self.queue.iter().position(|e| e["event"] == name) {
            return self.queue.remove(pos).map(|e| e["body"].clone());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let msg = self.recv(deadline)?;
            if msg["type"] != "event" {
                continue;
            }
            if msg["event"] == name {
                return Some(msg["body"].clone());
            }
            self.queue.push_back(msg);
        }
    }

    /// Drops queued events with this name.
    pub fn clear_events(&mut self, name: &str) {
        self.queue.retain(|e| e["event"] != name);
    }

    pub fn output(&self) -> &str {
        &self.output
    }

    /// Waits until the console output contains `text`.
    pub fn wait_output(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        while !self.output.contains(text) {
            match self.recv(deadline) {
                Some(msg) if msg["type"] == "event" => self.queue.push_back(msg),
                Some(_) => {}
                None => panic!("output never contained `{text}`:\n{}", self.output),
            }
        }
    }

    pub fn stopped(&mut self) -> Value {
        self.wait_event("stopped")
    }

    pub fn top_frame(&mut self) -> Value {
        let st = self.request(
            "stackTrace",
            json!({"threadId": 1, "startFrame": 0, "levels": 20}),
        );
        st["stackFrames"][0].clone()
    }

    pub fn frames(&mut self) -> Vec<Value> {
        let st = self.request(
            "stackTrace",
            json!({"threadId": 1, "startFrame": 0, "levels": 50}),
        );
        st["stackFrames"].as_array().cloned().unwrap_or_default()
    }

    pub fn scope(&mut self, frame_id: i64, name: &str) -> i64 {
        let scopes = self.request("scopes", json!({"frameId": frame_id}));
        scopes["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("no scope {name}: {scopes}"))["variablesReference"]
            .as_i64()
            .unwrap()
    }

    pub fn variables(&mut self, reference: i64) -> Vec<Value> {
        let vars = self.request("variables", json!({"variablesReference": reference}));
        vars["variables"].as_array().cloned().unwrap_or_default()
    }

    pub fn variable(&mut self, reference: i64, name: &str) -> Value {
        let vars = self.variables(reference);
        vars.iter()
            .find(|v| v["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no variable {name} in {vars:?}"))
    }

    pub fn evaluate(&mut self, expression: &str, context: &str) -> Value {
        let frame = self.top_frame()["id"].clone();
        self.request(
            "evaluate",
            json!({"expression": expression, "frameId": frame, "context": context}),
        )
    }

    pub fn disconnect(mut self) {
        let _ = self.request_raw("disconnect", json!({"terminateDebuggee": true}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("haltline-dap did not exit after disconnect");
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// External tools needed by the end-to-end tests.
pub struct Tools {
    pub qemu: PathBuf,
    pub gdb: PathBuf,
    pub cc: PathBuf,
}

fn which(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for name in names {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn tool(env: &str, names: &[&str]) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| which(names))
}

/// Finds QEMU, an ARM GDB and arm-none-eabi-gcc. Returns `None` (the test is
/// skipped) when they are missing, unless `HALTLINE_E2E=1` demands them.
pub fn tools() -> Option<Tools> {
    let found = (|| {
        Some(Tools {
            qemu: tool("HALTLINE_QEMU", &["qemu-system-arm"])?,
            gdb: tool("HALTLINE_GDB", &["gdb-multiarch", "arm-none-eabi-gdb"])?,
            cc: tool("HALTLINE_CC", &["arm-none-eabi-gcc"])?,
        })
    })();
    if found.is_none() {
        if std::env::var("HALTLINE_E2E").as_deref() == Ok("1") {
            panic!("HALTLINE_E2E=1 but qemu-system-arm, gdb-multiarch/arm-none-eabi-gdb or arm-none-eabi-gcc is missing");
        }
        eprintln!("skipping end-to-end test: QEMU / ARM GDB / arm-none-eabi-gcc not found");
    }
    found
}

pub fn firmware_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/firmware")
}

/// Builds the test firmware once per test binary.
pub fn firmware(tools: &Tools) -> PathBuf {
    static ELF: OnceLock<PathBuf> = OnceLock::new();
    ELF.get_or_init(|| {
        let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("haltline-e2e");
        std::fs::create_dir_all(&out_dir).unwrap();
        let elf = out_dir.join("firmware.elf");
        let dir = firmware_dir();
        let status = Command::new(&tools.cc)
            .current_dir(&dir)
            .args([
                "-mcpu=cortex-m4",
                "-mthumb",
                "-O0",
                "-g3",
                "-ffreestanding",
                "-fno-common",
                "-nostdlib",
                "-T",
                "linker.ld",
                "startup.c",
                "main.c",
                "-lgcc",
                "-o",
            ])
            .arg(&elf)
            .status()
            .expect("run arm-none-eabi-gcc");
        assert!(status.success(), "firmware build failed");
        elf
    })
    .clone()
}

/// Line number (1-based) of the `@tag` marker in a firmware source file.
pub fn line_of(file: &str, tag: &str) -> i64 {
    let text = std::fs::read_to_string(firmware_dir().join(file)).unwrap();
    let marker = format!("@{tag} ");
    text.lines()
        .position(|l| l.contains(&marker) || l.trim_end().ends_with(&format!("@{tag} */")))
        .unwrap_or_else(|| panic!("no @{tag} in {file}")) as i64
        + 1
}

pub fn source(file: &str) -> String {
    firmware_dir().join(file).to_string_lossy().into_owned()
}

/// Starts a QEMU session and stops at `main`.
pub fn launch(tools: &Tools, extra: Value) -> DapClient {
    let elf = firmware(tools);
    let mut client = DapClient::spawn();
    let caps = client.request(
        "initialize",
        json!({"adapterID": "haltline", "linesStartAt1": true, "columnsStartAt1": true}),
    );
    assert_eq!(caps["supportsConfigurationDoneRequest"], true);
    let mut config = json!({
        "request": "launch",
        "executable": elf,
        "serverType": "qemu",
        "serverPath": tools.qemu,
        "gdbPath": tools.gdb,
        "machine": "netduinoplus2",
        "cwd": firmware_dir(),
    });
    if let (Some(c), Some(e)) = (config.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            c.insert(k.clone(), v.clone());
        }
    }
    client.request("launch", config);
    client.wait_event("initialized");
    client
}

pub fn line(frame: &Value) -> i64 {
    frame["line"].as_i64().unwrap_or(-1)
}

pub fn address(text: &Value) -> u64 {
    let s = text.as_str().unwrap_or("");
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}
