//! A synchronous GDB/MI client.
//!
//! Commands are written with a numeric token; a reader thread parses GDB's
//! output and hands each result record to the caller waiting on that token.
//! Everything else (async and stream records) is forwarded to an event sink.

use super::{parse_line, AsyncKind, Record, StreamKind, Value};
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Out-of-band output from GDB.
#[derive(Debug, Clone)]
pub enum MiEvent {
    Async {
        /// Monotonic sequence number shared by all async/stream records.
        seq: u64,
        kind: AsyncKind,
        class: String,
        body: Value,
    },
    Stream {
        seq: u64,
        kind: StreamKind,
        text: String,
    },
    /// A line that is not valid MI.
    Other(String),
    /// GDB closed its stdout.
    Exited,
}

#[derive(Debug, Clone)]
pub struct MiResponse {
    pub class: String,
    /// The result body (a tuple).
    pub body: Value,
    /// Console (`~`) output printed while the command was executing.
    pub console: String,
    /// Range of async-record sequence numbers that arrived while the command
    /// was in flight: `seq_start < seq <= seq_end`.
    pub seq_start: u64,
    pub seq_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiError {
    /// GDB answered `^error`.
    Gdb(String),
    Timeout(String),
    Exited,
    Io(String),
}

impl fmt::Display for MiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MiError::Gdb(msg) => write!(f, "{msg}"),
            MiError::Timeout(cmd) => write!(f, "GDB did not answer `{cmd}` in time"),
            MiError::Exited => write!(f, "GDB has exited"),
            MiError::Io(e) => write!(f, "cannot talk to GDB: {e}"),
        }
    }
}

impl std::error::Error for MiError {}

/// Receives every line sent to and received from GDB.
pub type Trace = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct State {
    pending: HashMap<u32, (mpsc::Sender<MiResponse>, u64)>,
    console: String,
}

struct Shared {
    state: Mutex<State>,
    seq: AtomicU64,
    alive: AtomicBool,
}

pub struct MiClient {
    writer: Mutex<Box<dyn Write + Send>>,
    shared: Arc<Shared>,
    next_token: AtomicU32,
    trace: Option<Trace>,
}

impl MiClient {
    /// Starts the reader thread on `output` and returns a client writing to `input`.
    pub fn start(
        input: Box<dyn Write + Send>,
        output: Box<dyn Read + Send>,
        sink: impl Fn(MiEvent) + Send + 'static,
        trace: Option<Trace>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            seq: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        let reader_shared = shared.clone();
        let reader_trace = trace.clone();
        thread::Builder::new()
            .name("gdb-reader".into())
            .spawn(move || reader_loop(output, reader_shared, sink, reader_trace))
            .expect("spawn gdb reader");
        Self {
            writer: Mutex::new(input),
            shared,
            next_token: AtomicU32::new(1),
            trace,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    /// Current async sequence number.
    pub fn seq(&self) -> u64 {
        self.shared.seq.load(Ordering::SeqCst)
    }

    pub fn cmd(&self, command: &str) -> Result<MiResponse, MiError> {
        self.cmd_timeout(command, DEFAULT_TIMEOUT)
    }

    pub fn cmd_timeout(&self, command: &str, timeout: Duration) -> Result<MiResponse, MiError> {
        if !self.is_alive() {
            return Err(MiError::Exited);
        }
        let token = self.next_token.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let seq_start = {
            let mut state = self.shared.state.lock().unwrap();
            let seq_start = self.shared.seq.load(Ordering::SeqCst);
            state.pending.insert(token, (tx, seq_start));
            state.console.clear();
            seq_start
        };
        let line = format!("{token}{}\n", command.trim_end());
        if let Some(trace) = &self.trace {
            trace(&format!("-> {}", line.trim_end()));
        }
        {
            let mut writer = self.writer.lock().unwrap();
            if let Err(e) = writer
                .write_all(line.as_bytes())
                .and_then(|_| writer.flush())
            {
                self.shared.state.lock().unwrap().pending.remove(&token);
                return Err(MiError::Io(e.to_string()));
            }
        }
        match rx.recv_timeout(timeout) {
            Ok(mut resp) => {
                resp.seq_start = seq_start;
                if resp.class == "error" {
                    let msg = resp.body.str("msg").unwrap_or("unknown GDB error");
                    Err(MiError::Gdb(msg.to_string()))
                } else {
                    Ok(resp)
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared.state.lock().unwrap().pending.remove(&token);
                Err(MiError::Timeout(command.to_string()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(MiError::Exited),
        }
    }

    /// Writes a command without waiting for its result (used for `-gdb-exit`).
    pub fn send_no_wait(&self, command: &str) {
        let mut writer = self.writer.lock().unwrap();
        let _ = writer
            .write_all(format!("{}\n", command.trim_end()).as_bytes())
            .and_then(|_| writer.flush());
    }
}

fn reader_loop(
    output: Box<dyn Read + Send>,
    shared: Arc<Shared>,
    sink: impl Fn(MiEvent),
    trace: Option<Trace>,
) {
    let mut reader = BufReader::new(output);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        if let Some(trace) = &trace {
            trace(&format!("<- {line}"));
        }
        match parse_line(line) {
            Record::Prompt => {}
            Record::Result { token, class, body } => {
                let mut state = shared.state.lock().unwrap();
                if let Some((tx, seq_start)) = token.and_then(|t| state.pending.remove(&t)) {
                    let console = std::mem::take(&mut state.console);
                    let _ = tx.send(MiResponse {
                        class,
                        body,
                        console,
                        seq_start,
                        seq_end: shared.seq.load(Ordering::SeqCst),
                    });
                }
            }
            Record::Async {
                kind, class, body, ..
            } => {
                let seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
                sink(MiEvent::Async {
                    seq,
                    kind,
                    class,
                    body,
                });
            }
            Record::Stream { kind, text } => {
                let captured = {
                    let mut state = shared.state.lock().unwrap();
                    if kind == StreamKind::Console && !state.pending.is_empty() {
                        state.console.push_str(&text);
                        true
                    } else {
                        false
                    }
                };
                if !captured {
                    let seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
                    sink(MiEvent::Stream { seq, kind, text });
                }
            }
            Record::Other(text) => sink(MiEvent::Other(text)),
        }
    }
    shared.alive.store(false, Ordering::SeqCst);
    shared.state.lock().unwrap().pending.clear();
    sink(MiEvent::Exited);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// Spawns a fake GDB thread answering every command via `respond`.
    fn fake(
        respond: impl Fn(u32, &str) -> Vec<String> + Send + 'static,
    ) -> (MiClient, mpsc::Receiver<MiEvent>) {
        let (cmd_reader, cmd_writer) = std::io::pipe().unwrap();
        let (out_reader, mut out_writer) = std::io::pipe().unwrap();
        thread::spawn(move || {
            let reader = BufReader::new(cmd_reader);
            for line in reader.lines() {
                let line = line.unwrap();
                let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
                let token: u32 = line[..digits].parse().unwrap_or(0);
                for out in respond(token, &line[digits..]) {
                    writeln!(out_writer, "{out}").unwrap();
                }
                writeln!(out_writer, "(gdb) ").unwrap();
                out_writer.flush().unwrap();
            }
        });
        let (tx, rx) = mpsc::channel();
        let client = MiClient::start(
            Box::new(cmd_writer),
            Box::new(out_reader),
            move |e| {
                let _ = tx.send(e);
            },
            None,
        );
        (client, rx)
    }

    #[test]
    fn matches_results_by_token_and_captures_console() {
        let (client, _rx) = fake(|token, cmd| {
            if cmd.starts_with("-interpreter-exec") {
                vec![
                    "~\"line one\\n\"".into(),
                    "~\"line two\\n\"".into(),
                    format!("{token}^done"),
                ]
            } else {
                vec![format!("{token}^done,value=\"7\"")]
            }
        });
        let resp = client.cmd("-data-evaluate-expression x").unwrap();
        assert_eq!(resp.body.str("value"), Some("7"));
        let resp = client
            .cmd("-interpreter-exec console \"info registers\"")
            .unwrap();
        assert_eq!(resp.console, "line one\nline two\n");
    }

    #[test]
    fn reports_gdb_errors() {
        let (client, _rx) =
            fake(|token, _| vec![format!("{token}^error,msg=\"No symbol \\\"q\\\".\"")]);
        let err = client.cmd("-data-evaluate-expression q").unwrap_err();
        assert_eq!(err, MiError::Gdb("No symbol \"q\".".into()));
    }

    #[test]
    fn forwards_async_records_with_sequence_numbers() {
        let (client, rx) = fake(|token, _| {
            vec![
                "*running,thread-id=\"all\"".into(),
                format!("{token}^running"),
                "*stopped,reason=\"end-stepping-range\"".into(),
            ]
        });
        let resp = client.cmd("-exec-next").unwrap();
        assert_eq!(resp.class, "running");
        assert_eq!(resp.seq_end - resp.seq_start, 1);
        let mut classes = Vec::new();
        for _ in 0..2 {
            match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
                MiEvent::Async { class, seq, .. } => classes.push((class, seq)),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(classes[0].0, "running");
        assert_eq!(classes[1].0, "stopped");
        assert!(classes[1].1 > classes[0].1);
    }

    #[test]
    fn times_out_and_reports_exit() {
        let (client, rx) = fake(|_, _| vec![]);
        let err = client
            .cmd_timeout("-gdb-version", Duration::from_millis(100))
            .unwrap_err();
        assert!(matches!(err, MiError::Timeout(_)));
        drop(client);
        // The fake exits once the command pipe closes, which closes stdout.
        let exited = (0..50).any(|_| {
            matches!(
                rx.recv_timeout(Duration::from_millis(100)),
                Ok(MiEvent::Exited)
            )
        });
        assert!(exited);
    }
}
