//! GDB/MI output parser.
//!
//! Grammar (from the GDB manual, "GDB/MI Output Syntax"):
//! ```text
//! result-record  → [token] "^" result-class ("," result)*
//! async-record   → [token] ("*" | "+" | "=") async-class ("," result)*
//! stream-record  → ("~" | "@" | "&") c-string
//! result         → variable "=" value
//! value          → c-string | tuple | list
//! ```

pub mod client;

use std::fmt;

/// A GDB/MI value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Const(String),
    /// `{a="1",b="2"}` — keys may repeat, so this is an ordered list of pairs.
    Tuple(Vec<(String, Value)>),
    /// `[...]` — names of `name=value` items (e.g. `frame=`) are dropped.
    List(Vec<Value>),
}

static EMPTY_LIST: Vec<Value> = Vec::new();

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Tuple(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// All values with the given key (for tuples with repeated keys).
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        let items: &[(String, Value)] = match self {
            Value::Tuple(items) => items,
            _ => &[],
        };
        items.iter().filter(move |(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Const(s) => Some(s),
            _ => None,
        }
    }

    /// String value of a key; `None` when absent or not a constant.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// List value of a key; an empty slice when absent. A tuple is treated as a
    /// one-element list (GDB sometimes emits either form).
    pub fn list(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::List(items)) => items,
            Some(v @ Value::Tuple(_)) => std::slice::from_ref(v),
            _ => &EMPTY_LIST,
        }
    }

    pub fn as_list(&self) -> &[Value] {
        match self {
            Value::List(items) => items,
            _ => &EMPTY_LIST,
        }
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.str(key).and_then(crate::util::parse_u64)
    }

    pub fn i64(&self, key: &str) -> Option<i64> {
        self.str(key).and_then(|s| s.trim().parse().ok())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Const(s) => write!(f, "{}", crate::util::mi_quote(s)),
            Value::Tuple(items) => {
                write!(f, "{{")?;
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{k}={v}")?;
                }
                write!(f, "}}")
            }
            Value::List(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ",")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncKind {
    /// `*` — execution state changes (`running`, `stopped`).
    Exec,
    /// `+` — progress of long operations (e.g. `download`).
    Status,
    /// `=` — notifications (`thread-created`, `breakpoint-modified`, ...).
    Notify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// `~` — CLI console output.
    Console,
    /// `@` — output of the program being debugged (e.g. semihosting via GDB).
    Target,
    /// `&` — GDB's internal log (echoed commands, warnings, errors).
    Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Result {
        token: Option<u32>,
        class: String,
        /// Always a [`Value::Tuple`].
        body: Value,
    },
    Async {
        token: Option<u32>,
        kind: AsyncKind,
        class: String,
        body: Value,
    },
    Stream {
        kind: StreamKind,
        text: String,
    },
    /// The `(gdb)` prompt that terminates each output batch.
    Prompt,
    /// A line that is not MI (e.g. stray output written by a child process).
    Other(String),
}

/// Parses a single line of GDB/MI output (without the trailing newline).
pub fn parse_line(line: &str) -> Record {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim_end() == "(gdb)" {
        return Record::Prompt;
    }
    let mut p = Parser::new(line);
    match p.record() {
        Some(rec) => rec,
        None => Record::Other(line.to_string()),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s: s.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn record(&mut self) -> Option<Record> {
        let token_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let token = if self.pos > token_start {
            std::str::from_utf8(&self.s[token_start..self.pos])
                .ok()?
                .parse()
                .ok()
        } else {
            None
        };
        let marker = self.peek()?;
        self.pos += 1;
        match marker {
            b'^' | b'*' | b'+' | b'=' => {
                let class = self.identifier()?;
                let body = Value::Tuple(self.results_tail()?);
                if self.pos != self.s.len() {
                    return None;
                }
                Some(match marker {
                    b'^' => Record::Result { token, class, body },
                    b'*' => Record::Async {
                        token,
                        kind: AsyncKind::Exec,
                        class,
                        body,
                    },
                    b'+' => Record::Async {
                        token,
                        kind: AsyncKind::Status,
                        class,
                        body,
                    },
                    _ => Record::Async {
                        token,
                        kind: AsyncKind::Notify,
                        class,
                        body,
                    },
                })
            }
            b'~' | b'@' | b'&' if token.is_none() => {
                let text = self.c_string()?;
                let kind = match marker {
                    b'~' => StreamKind::Console,
                    b'@' => StreamKind::Target,
                    _ => StreamKind::Log,
                };
                Some(Record::Stream { kind, text })
            }
            _ => None,
        }
    }

    /// Parses `("," result)*` until the end of input.
    fn results_tail(&mut self) -> Option<Vec<(String, Value)>> {
        let mut items = Vec::new();
        while self.eat(b',') {
            items.push(self.result()?);
        }
        Some(items)
    }

    fn identifier(&mut self) -> Option<String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.pos]).into_owned())
    }

    /// `variable "=" value`. GDB occasionally emits bare values in result
    /// position (e.g. MI2 multi-location breakpoints); those get an empty name.
    fn result(&mut self) -> Option<(String, Value)> {
        if matches!(self.peek(), Some(b'{' | b'[' | b'"')) {
            return Some((String::new(), self.value()?));
        }
        let name = self.identifier()?;
        if !self.eat(b'=') {
            return None;
        }
        Some((name, self.value()?))
    }

    fn value(&mut self) -> Option<Value> {
        match self.peek()? {
            b'"' => Some(Value::Const(self.c_string()?)),
            b'{' => {
                self.pos += 1;
                let mut items = Vec::new();
                if self.eat(b'}') {
                    return Some(Value::Tuple(items));
                }
                loop {
                    items.push(self.result()?);
                    if self.eat(b'}') {
                        return Some(Value::Tuple(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Value::List(items));
                }
                loop {
                    let item = match self.peek()? {
                        b'"' | b'{' | b'[' => self.value()?,
                        _ => self.result()?.1,
                    };
                    items.push(item);
                    if self.eat(b']') {
                        return Some(Value::List(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            _ => None,
        }
    }

    fn c_string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut bytes = Vec::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => bytes.push(b'\n'),
                        b't' => bytes.push(b'\t'),
                        b'r' => bytes.push(b'\r'),
                        b'a' => bytes.push(0x07),
                        b'b' => bytes.push(0x08),
                        b'f' => bytes.push(0x0c),
                        b'v' => bytes.push(0x0b),
                        b'e' => bytes.push(0x1b),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            bytes.push(v as u8);
                        }
                        other => bytes.push(other),
                    }
                }
                other => bytes.push(other),
            }
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(rec: &Record) -> &Value {
        match rec {
            Record::Result { body, .. } | Record::Async { body, .. } => body,
            other => panic!("unexpected record {other:?}"),
        }
    }

    #[test]
    fn parses_result_with_token() {
        let rec = parse_line("12^done,value=\"42\"");
        match &rec {
            Record::Result { token, class, .. } => {
                assert_eq!(*token, Some(12));
                assert_eq!(class, "done");
            }
            _ => panic!(),
        }
        assert_eq!(body(&rec).str("value"), Some("42"));
    }

    #[test]
    fn parses_error_messages_with_escapes() {
        let rec = parse_line(r#"7^error,msg="No symbol \"foo\" in current context.""#);
        assert_eq!(
            body(&rec).str("msg"),
            Some(r#"No symbol "foo" in current context."#)
        );
    }

    #[test]
    fn parses_stopped_event() {
        let line = r#"*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x080001a4",func="main",args=[],file="main.c",fullname="/home/u/fw/main.c",line="42",arch="armv7e-m"},thread-id="1",stopped-threads="all""#;
        let rec = parse_line(line);
        let Record::Async {
            kind, class, body, ..
        } = &rec
        else {
            panic!()
        };
        assert_eq!(*kind, AsyncKind::Exec);
        assert_eq!(class, "stopped");
        assert_eq!(body.str("reason"), Some("breakpoint-hit"));
        let frame = body.get("frame").unwrap();
        assert_eq!(frame.u64("addr"), Some(0x0800_01a4));
        assert_eq!(frame.i64("line"), Some(42));
        assert!(frame.list("args").is_empty());
    }

    #[test]
    fn parses_named_lists() {
        let line = r#"^done,stack=[frame={level="0",addr="0x1",func="a"},frame={level="1",addr="0x2",func="b"}]"#;
        let rec = parse_line(line);
        let stack = body(&rec).list("stack");
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[1].str("func"), Some("b"));
    }

    #[test]
    fn parses_value_lists_and_nested_tuples() {
        let line = r#"^done,register-names=["r0","r1","","pc"],x={a={b=["1","2"]}}"#;
        let rec = parse_line(line);
        let names: Vec<_> = body(&rec)
            .list("register-names")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, ["r0", "r1", "", "pc"]);
        let inner = body(&rec).get("x").unwrap().get("a").unwrap();
        assert_eq!(inner.list("b").len(), 2);
    }

    #[test]
    fn parses_empty_containers() {
        let rec = parse_line("^done,a={},b=[]");
        assert_eq!(body(&rec).get("a"), Some(&Value::Tuple(vec![])));
        assert_eq!(body(&rec).get("b"), Some(&Value::List(vec![])));
    }

    #[test]
    fn parses_stream_records_with_octal_utf8() {
        // "°C" encoded by GDB as octal escapes of its UTF-8 bytes.
        let rec = parse_line(r#"~"Temp: 25\302\260C\n""#);
        assert_eq!(
            rec,
            Record::Stream {
                kind: StreamKind::Console,
                text: "Temp: 25°C\n".into()
            }
        );
        let rec = parse_line(r#"@"hello""#);
        assert!(matches!(
            rec,
            Record::Stream {
                kind: StreamKind::Target,
                ..
            }
        ));
        let rec = parse_line(r#"&"warning: x\n""#);
        assert!(matches!(
            rec,
            Record::Stream {
                kind: StreamKind::Log,
                ..
            }
        ));
    }

    #[test]
    fn parses_prompt_and_garbage() {
        assert_eq!(parse_line("(gdb) "), Record::Prompt);
        assert_eq!(parse_line("(gdb)"), Record::Prompt);
        assert_eq!(
            parse_line("Info : hello"),
            Record::Other("Info : hello".into())
        );
        assert!(matches!(parse_line("^done,x="), Record::Other(_)));
        assert!(matches!(
            parse_line("^done,x=\"unterminated"),
            Record::Other(_)
        ));
    }

    #[test]
    fn parses_notify_and_status_records() {
        let rec = parse_line(r#"=thread-group-started,id="i1",pid="42000""#);
        assert!(matches!(
            rec,
            Record::Async {
                kind: AsyncKind::Notify,
                ..
            }
        ));
        let rec =
            parse_line(r#"+download,{section=".text",section-size="1024",total-size="2048"}"#);
        let Record::Async {
            kind, class, body, ..
        } = &rec
        else {
            panic!()
        };
        assert_eq!(*kind, AsyncKind::Status);
        assert_eq!(class, "download");
        assert_eq!(body.get("").unwrap().str("section"), Some(".text"));
    }

    #[test]
    fn tolerates_mi2_multi_location_breakpoints() {
        let line = r#"^done,bkpt={number="1",type="breakpoint",addr="<MULTIPLE>"},{number="1.1",addr="0x1"},{number="1.2",addr="0x2"}"#;
        let rec = parse_line(line);
        let b = body(&rec);
        assert_eq!(b.get("bkpt").unwrap().str("number"), Some("1"));
        assert_eq!(b.get_all("").count(), 2);
    }

    #[test]
    fn display_round_trips() {
        let line = r#"^done,a="x\"y",b={c=["1",{d="2"}]}"#;
        let rec = parse_line(line);
        let text = format!("^done,{}", {
            let Value::Tuple(items) = body(&rec) else {
                panic!()
            };
            items
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        });
        assert_eq!(parse_line(&text), rec);
    }

    #[test]
    fn disassembly_with_source_lines() {
        let line = r#"^done,asm_insns=[src_and_asm_line={line="5",file="m.c",fullname="/m.c",line_asm_insn=[{address="0x08000100",func-name="main",offset="0",opcodes="b580",inst="push\t{r7, lr}"}]}]"#;
        let rec = parse_line(line);
        let lines = body(&rec).list("asm_insns");
        assert_eq!(lines.len(), 1);
        let insn = &lines[0].list("line_asm_insn")[0];
        assert_eq!(insn.str("inst"), Some("push\t{r7, lr}"));
        assert_eq!(insn.str("func-name"), Some("main"));
    }
}
