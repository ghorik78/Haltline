//! Debug Adapter Protocol framing (`Content-Length` headers + JSON) and a
//! thread-safe writer for responses and events.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::Mutex;

/// Reads one DAP message. Returns `Ok(None)` on a clean end of stream.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return if content_length.is_none() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended inside DAP headers",
                ))
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length")
                })?);
            }
        }
    }
    let mut body = vec![0u8; content_length.unwrap_or(0)];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message<W: Write>(writer: &mut W, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

/// An incoming request.
#[derive(Debug, Clone)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    pub arguments: Value,
}

impl Request {
    pub fn from_value(v: &Value) -> Option<Self> {
        if v.get("type")?.as_str()? != "request" {
            return None;
        }
        Some(Self {
            seq: v.get("seq")?.as_i64()?,
            command: v.get("command")?.as_str()?.to_string(),
            arguments: v.get("arguments").cloned().unwrap_or(Value::Null),
        })
    }

    pub fn arg(&self, key: &str) -> Option<&Value> {
        self.arguments.get(key)
    }

    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.arguments.get(key).and_then(Value::as_str)
    }

    pub fn arg_i64(&self, key: &str) -> Option<i64> {
        self.arguments.get(key).and_then(Value::as_i64)
    }

    pub fn arg_bool(&self, key: &str) -> Option<bool> {
        self.arguments.get(key).and_then(Value::as_bool)
    }
}

/// Output categories understood by DAP clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Console,
    Stdout,
    Stderr,
    Important,
}

impl Category {
    fn as_str(self) -> &'static str {
        match self {
            Category::Console => "console",
            Category::Stdout => "stdout",
            Category::Stderr => "stderr",
            Category::Important => "important",
        }
    }
}

struct WriterState {
    out: Box<dyn Write + Send>,
    seq: i64,
}

/// Serialises messages to the client; safe to share between threads.
pub struct DapWriter {
    state: Mutex<WriterState>,
}

impl DapWriter {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            state: Mutex::new(WriterState { out, seq: 1 }),
        }
    }

    fn send(&self, mut message: Value) {
        let mut state = self.state.lock().unwrap();
        message["seq"] = json!(state.seq);
        state.seq += 1;
        // A broken pipe means the client is gone; the main loop notices EOF.
        let _ = write_message(&mut state.out, &message);
    }

    pub fn respond(&self, req: &Request, body: Value) {
        let mut msg = json!({
            "type": "response",
            "request_seq": req.seq,
            "success": true,
            "command": req.command,
        });
        if !body.is_null() {
            msg["body"] = body;
        }
        self.send(msg);
    }

    pub fn respond_error(&self, req: &Request, message: &str) {
        self.send(json!({
            "type": "response",
            "request_seq": req.seq,
            "success": false,
            "command": req.command,
            "message": message,
            "body": { "error": { "id": 1, "format": message, "showUser": true } },
        }));
    }

    pub fn event(&self, event: &str, body: Value) {
        let mut msg = json!({ "type": "event", "event": event });
        if !body.is_null() {
            msg["body"] = body;
        }
        self.send(msg);
    }

    pub fn output(&self, category: Category, text: &str) {
        if text.is_empty() {
            return;
        }
        self.event(
            "output",
            json!({ "category": category.as_str(), "output": text }),
        );
    }

    /// Console output that always ends with a newline.
    pub fn console(&self, text: &str) {
        if text.ends_with('\n') {
            self.output(Category::Console, text);
        } else {
            self.output(Category::Console, &format!("{text}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    #[test]
    fn reads_multiple_messages() {
        let a =
            r#"{"seq":1,"type":"request","command":"initialize","arguments":{"adapterID":"x"}}"#;
        let b = r#"{"seq":2,"type":"request","command":"launch"}"#;
        let raw = format!(
            "Content-Length: {}\r\n\r\n{a}Content-Length: {}\r\n\r\n{b}",
            a.len(),
            b.len()
        );
        let mut cursor = Cursor::new(raw.into_bytes());
        let first = read_message(&mut cursor).unwrap().unwrap();
        let req = Request::from_value(&first).unwrap();
        assert_eq!(req.command, "initialize");
        assert_eq!(req.arg_str("adapterID"), Some("x"));
        let second = Request::from_value(&read_message(&mut cursor).unwrap().unwrap()).unwrap();
        assert_eq!(second.seq, 2);
        assert!(second.arguments.is_null());
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn handles_utf8_lengths_and_extra_headers() {
        let body =
            r#"{"seq":1,"type":"request","command":"evaluate","arguments":{"expression":"ж"}}"#;
        let raw = format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let msg = read_message(&mut Cursor::new(raw.into_bytes()))
            .unwrap()
            .unwrap();
        assert_eq!(msg["arguments"]["expression"], "ж");
    }

    #[test]
    fn rejects_truncated_messages() {
        let raw = b"Content-Length: 50\r\n\r\n{\"seq\":1}".to_vec();
        assert!(read_message(&mut Cursor::new(raw)).is_err());
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_assigns_sequence_numbers() {
        let sink = Sink::default();
        let writer = DapWriter::new(Box::new(sink.clone()));
        let req = Request {
            seq: 5,
            command: "threads".into(),
            arguments: Value::Null,
        };
        writer.respond(&req, json!({"threads": []}));
        writer.respond_error(&req, "boom");
        writer.console("hello");
        let data = sink.0.lock().unwrap().clone();
        let mut cursor = Cursor::new(data);
        let r1 = read_message(&mut cursor).unwrap().unwrap();
        let r2 = read_message(&mut cursor).unwrap().unwrap();
        let r3 = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(r1["seq"], 1);
        assert_eq!(r1["request_seq"], 5);
        assert_eq!(r1["success"], true);
        assert_eq!(r2["seq"], 2);
        assert_eq!(r2["success"], false);
        assert_eq!(r2["message"], "boom");
        assert_eq!(r3["event"], "output");
        assert_eq!(r3["body"]["output"], "hello\n");
    }
}
