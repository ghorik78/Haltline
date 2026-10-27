//! Inspecting a halted target: threads, call stack, scopes, variables,
//! registers, SVD peripherals, memory, disassembly and the debug console.

use super::{body, HandlerResult, Session, TargetState};
use crate::cortex::{self, FaultStatus, Group};
use crate::dap::Request;
use crate::disasm::parse_disassembly;
use crate::mi::client::MiClient;
use crate::mi::Value as Mi;
use crate::svd::{read_plan, RegisterNode};
use crate::util::{
    base64_decode, base64_encode, hex, hex_decode, hex_encode, le_to_u64, leading_address,
    mi_quote, parse_int_bits, parse_u64, u64_to_le,
};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Arrays with more children than this are split into ranges.
const CHUNK: i64 = 100;
const MAX_GLOBALS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct FrameRef {
    pub thread: i64,
    pub level: i64,
}

impl FrameRef {
    fn opts(self) -> String {
        format!("--thread {} --frame {}", self.thread, self.level)
    }
}

#[derive(Debug, Clone)]
struct FrameInfo {
    frame: FrameRef,
    fullname: Option<String>,
}

#[derive(Debug, Clone)]
enum Handle {
    Locals(FrameRef),
    Globals,
    Statics(String),
    Registers(FrameRef),
    RegisterGroup(FrameRef, Group),
    RegisterBits {
        name: String,
        value: u64,
    },
    FaultStatus,
    FaultBits {
        which: &'static str,
        value: u64,
    },
    Peripherals,
    Peripheral(usize),
    PeripheralNode(usize, Vec<usize>),
    VarObj {
        name: String,
        expr: String,
        ty: String,
        numchild: i64,
    },
    VarRange {
        name: String,
        expr: String,
        ty: String,
        from: i64,
        to: i64,
    },
}

#[derive(Debug, Clone)]
struct GlobalSym {
    name: String,
    file: String,
    fullname: String,
    is_static: bool,
}

#[derive(Default)]
pub(super) struct Inspection {
    handles: HashMap<i64, Handle>,
    next_handle: i64,
    frames: HashMap<i64, FrameInfo>,
    next_frame: i64,
    varobjs: Vec<String>,
    /// (container reference, variable name) → expression, for data breakpoints.
    pub data_targets: HashMap<(i64, String), String>,
    registers: HashMap<FrameRef, Vec<(String, String)>>,
    peripheral_values: HashMap<u64, Option<u64>>,
    top_without_source: HashMap<i64, bool>,
    // Kept for the whole session:
    register_names: Option<Vec<String>>,
    globals: Option<Result<Vec<GlobalSym>, String>>,
    file_exists: HashMap<String, bool>,
}

impl Inspection {
    /// Forgets everything that belongs to the current stop.
    pub fn reset(&mut self, gdb: Option<&MiClient>) {
        if let Some(gdb) = gdb {
            for name in self.varobjs.drain(..) {
                let _ = gdb.cmd_timeout(&format!("-var-delete {name}"), Duration::from_secs(2));
            }
        }
        self.varobjs.clear();
        self.handles.clear();
        self.frames.clear();
        self.data_targets.clear();
        self.registers.clear();
        self.peripheral_values.clear();
    }

    pub fn top_frame_without_source(&self, thread: i64) -> bool {
        self.top_without_source
            .get(&thread)
            .copied()
            .unwrap_or(false)
    }

    fn handle(&mut self, h: Handle) -> i64 {
        self.next_handle += 1;
        self.handles.insert(self.next_handle, h);
        self.next_handle
    }

    fn source_exists(&mut self, path: &str) -> bool {
        *self
            .file_exists
            .entry(path.to_string())
            .or_insert_with(|| Path::new(path).is_file())
    }
}

fn var_json(name: &str, value: &str, ty: &str, reference: i64) -> Value {
    let mut v = json!({
        "name": name,
        "value": value,
        "variablesReference": reference,
    });
    if !ty.is_empty() {
        v["type"] = json!(ty);
    }
    v
}

fn read_only(mut v: Value) -> Value {
    v["presentationHint"] = json!({"attributes": ["readOnly"]});
    v
}

/// Expression for a child of a GDB variable object.
fn child_expression(parent: &str, parent_type: &str, exp: &str) -> String {
    let simple = parent
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '>' | '-' | '$'))
        && !parent.starts_with('-');
    let wrapped = if simple {
        parent.to_string()
    } else {
        format!("({parent})")
    };
    if exp.chars().all(|c| c.is_ascii_digit()) {
        format!("{wrapped}[{exp}]")
    } else if exp.starts_with('*') {
        format!("*{wrapped}")
    } else if parent_type.trim_end().ends_with('*') {
        format!("{wrapped}->{exp}")
    } else {
        format!("{wrapped}.{exp}")
    }
}

fn is_access_specifier(exp: &str) -> bool {
    matches!(exp, "public" | "private" | "protected")
}

/// CLI words that make console input a GDB command rather than an expression.
const CLI_WORDS: &[&str] = &[
    "monitor",
    "mon",
    "info",
    "x",
    "p",
    "print",
    "output",
    "set",
    "show",
    "load",
    "break",
    "tbreak",
    "hbreak",
    "delete",
    "disassemble",
    "bt",
    "backtrace",
    "where",
    "frame",
    "up",
    "down",
    "finish",
    "maintenance",
    "maint",
    "flushregs",
    "list",
    "ptype",
    "whatis",
    "display",
    "undisplay",
    "dump",
    "restore",
    "call",
    "continue",
    "next",
    "step",
    "stepi",
    "nexti",
    "until",
    "advance",
    "jump",
    "watch",
    "rwatch",
    "awatch",
    "source",
    "echo",
    "printf",
    "compare-sections",
    "thread",
    "help",
    "file",
    "symbol-file",
    "add-symbol-file",
    "target",
    "detach",
    "disconnect",
    "kill",
    "shell",
];
const CLI_STANDALONE: &[&str] = &[
    "bt",
    "backtrace",
    "where",
    "finish",
    "up",
    "down",
    "flushregs",
    "continue",
    "next",
    "step",
    "stepi",
    "nexti",
    "help",
    "info",
    "load",
    "detach",
    "disconnect",
];

fn looks_like_cli(input: &str) -> bool {
    let first = input.split_whitespace().next().unwrap_or("");
    let word = first.split('/').next().unwrap_or(first);
    if !CLI_WORDS.contains(&word) {
        return false;
    }
    first.contains('/') || input.split_whitespace().count() > 1 || CLI_STANDALONE.contains(&word)
}

const HELP: &str = "\
Haltline debug console
  <expression>          evaluate a C expression (e.g. g_counter, *ptr, buf[3])
  <gdb command>         run a GDB command (e.g. info registers, x/8xw 0x20000000, monitor reset halt)
  > <gdb command>       force a GDB command
  :mi <command>         run a raw GDB/MI command
  :asm [on|off|auto]    show the top frame as disassembly (instruction stepping)
  :hex [on|off]         show variables in hexadecimal
  :reset [run]          reset the target (and run to the entry point with `run`)
  :fault                explain the current fault (CFSR/HFSR and the stacked frame)
  :svd [PERIPH[.REG]]   list peripherals or show register values from the SVD file
  :help                 this text";

impl Session {
    // ----- threads and frames ---------------------------------------------

    pub(super) fn threads(&mut self, _req: &Request) -> HandlerResult {
        if self.state != TargetState::Stopped || self.gdb.is_none() {
            let cached = if self.threads_cache.is_empty() {
                vec![json!({"id": 1, "name": "Core"})]
            } else {
                self.threads_cache.clone()
            };
            return body(json!({"threads": cached}));
        }
        let resp = self.mi("-thread-info")?;
        let mut threads = Vec::new();
        for t in resp.body.list("threads") {
            let Some(id) = t.i64("id") else { continue };
            let name = t
                .str("name")
                .or_else(|| t.str("details"))
                .map(str::to_string)
                .or_else(|| t.str("target-id").map(str::to_string))
                .unwrap_or_else(|| format!("Thread {id}"));
            let name = if name == "Remote target" {
                "Core".to_string()
            } else {
                name
            };
            threads.push(json!({"id": id, "name": name}));
        }
        if threads.is_empty() {
            threads.push(json!({"id": 1, "name": "Core"}));
        }
        self.threads_cache = threads.clone();
        body(json!({"threads": threads}))
    }

    pub(super) fn stack_trace(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let thread = req.arg_i64("threadId").unwrap_or(1);
        let start = req.arg_i64("startFrame").unwrap_or(0).max(0);
        let levels = req.arg_i64("levels").unwrap_or(0);
        let depth = self
            .mi(&format!("-stack-info-depth --thread {thread} 256"))
            .ok()
            .and_then(|r| r.body.i64("depth"))
            .unwrap_or(1)
            .max(1);
        let end = if levels > 0 {
            (start + levels).min(depth)
        } else {
            depth
        };
        if start >= end {
            return body(json!({"stackFrames": [], "totalFrames": depth}));
        }
        let resp = self.mi(&format!(
            "-stack-list-frames --thread {thread} {start} {}",
            end - 1
        ))?;
        let mut frames = Vec::new();
        for f in resp.body.list("stack") {
            let level = f.i64("level").unwrap_or(0);
            let addr = f.u64("addr").unwrap_or(0);
            let func = f.str("func").unwrap_or("??").to_string();
            let fullname = f
                .str("fullname")
                .map(str::to_string)
                .filter(|p| self.insp.source_exists(p));
            let line = f.i64("line");
            self.insp.next_frame += 1;
            let id = self.insp.next_frame;
            self.insp.frames.insert(
                id,
                FrameInfo {
                    frame: FrameRef { thread, level },
                    fullname: fullname.clone(),
                },
            );
            if level == 0 {
                self.insp
                    .top_without_source
                    .insert(thread, fullname.is_none());
            }
            let mut frame = json!({
                "id": id,
                "name": func,
                "line": 0,
                "column": 0,
                "instructionPointerReference": format!("0x{addr:08x}"),
            });
            if func == "<signal handler called>" {
                frame["name"] = json!("<exception entry>");
                frame["presentationHint"] = json!("label");
                frames.push(frame);
                continue;
            }
            let use_asm = fullname.is_none() || (level == 0 && self.asm_view_active());
            let asm = if use_asm
                && self.config.show_disassembly != crate::config::DisassemblyMode::Never
            {
                self.asm_location(addr)
            } else {
                None
            };
            if let Some((path, asm_line)) = asm {
                frame["source"] = json!({
                    "name": format!("{func} (disassembly)"),
                    "path": path,
                    "presentationHint": "deemphasize",
                });
                frame["line"] = json!(self.client_line(i64::from(asm_line)));
                frame["column"] = json!(1);
            } else if let (Some(path), Some(line)) = (&fullname, line) {
                let name = Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                frame["source"] = json!({"name": name, "path": path});
                frame["line"] = json!(self.client_line(line));
                frame["column"] = json!(1);
            } else {
                frame["presentationHint"] = json!("subtle");
            }
            frames.push(frame);
        }
        body(json!({"stackFrames": frames, "totalFrames": depth}))
    }

    /// Path and line of the disassembly file containing `addr`.
    fn asm_location(&mut self, addr: u64) -> Option<(String, u32)> {
        if let Some(f) = self.disasm.find(addr) {
            return f
                .line_of(addr)
                .map(|l| (f.path.to_string_lossy().into_owned(), l));
        }
        let mut insns = self
            .mi_timeout(
                &format!("-data-disassemble -a 0x{addr:x} -- 5"),
                Duration::from_secs(20),
            )
            .map(|r| parse_disassembly(&r.body))
            .unwrap_or_default();
        if !insns.iter().any(|i| i.address == addr) {
            // No symbol around the address: disassemble a window from it.
            insns = self
                .mi(&format!(
                    "-data-disassemble -s 0x{addr:x} -e 0x{:x} -- 5",
                    addr + 0x80
                ))
                .map(|r| parse_disassembly(&r.body))
                .ok()?;
        }
        let title = insns
            .first()
            .and_then(|i| i.func.clone())
            .filter(|f| insns.iter().all(|i| i.func.as_ref() == Some(f)))
            .unwrap_or_else(|| format!("0x{addr:08x}"));
        let file = match self.disasm.insert(&title, &insns) {
            Ok(f) => f,
            Err(e) => {
                self.warn(&format!("cannot write disassembly file: {e}"));
                return None;
            }
        };
        file.line_of(addr)
            .map(|l| (file.path.to_string_lossy().into_owned(), l))
    }

    fn frame_for(&self, frame_id: Option<i64>) -> Option<FrameRef> {
        frame_id
            .and_then(|id| self.insp.frames.get(&id))
            .map(|f| f.frame)
    }

    // ----- scopes and variables -------------------------------------------

    pub(super) fn scopes(&mut self, req: &Request) -> HandlerResult {
        let id = req.arg_i64("frameId").unwrap_or(0);
        let info = self
            .insp
            .frames
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown frame {id}"))?;
        let mut scopes = Vec::new();
        let locals = self.insp.handle(Handle::Locals(info.frame));
        scopes.push(json!({"name": "Locals", "presentationHint": "locals", "variablesReference": locals, "expensive": false}));
        if self.config.globals_enabled() {
            if let Some(file) = &info.fullname {
                let statics = self.insp.handle(Handle::Statics(file.clone()));
                scopes.push(
                    json!({"name": "Static", "variablesReference": statics, "expensive": true}),
                );
            }
            let globals = self.insp.handle(Handle::Globals);
            scopes.push(json!({"name": "Globals", "presentationHint": "globals", "variablesReference": globals, "expensive": true}));
        }
        let regs = self.insp.handle(Handle::Registers(info.frame));
        scopes.push(json!({"name": "Registers", "presentationHint": "registers", "variablesReference": regs, "expensive": false}));
        if self.svd.is_some() {
            let p = self.insp.handle(Handle::Peripherals);
            scopes.push(json!({"name": "Peripherals", "variablesReference": p, "expensive": true}));
        }
        body(json!({"scopes": scopes}))
    }

    pub(super) fn variables(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let reference = req.arg_i64("variablesReference").unwrap_or(0);
        let hex_request = req
            .arg("format")
            .and_then(|f| f.get("hex"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let handle = self
            .insp
            .handles
            .get(&reference)
            .cloned()
            .ok_or_else(|| anyhow!("unknown variables reference {reference}"))?;
        let vars = match handle {
            Handle::Locals(frame) => self.locals(reference, frame, hex_request)?,
            Handle::Globals => self.globals(reference, None, hex_request)?,
            Handle::Statics(file) => self.globals(reference, Some(&file), hex_request)?,
            Handle::VarObj {
                name,
                expr,
                ty,
                numchild,
            } => {
                if numchild > CHUNK {
                    self.ranges(&name, &expr, &ty, 0, numchild)
                } else {
                    self.varobj_children(reference, &name, &expr, &ty, None, hex_request)?
                }
            }
            Handle::VarRange {
                name,
                expr,
                ty,
                from,
                to,
            } => {
                if to - from > CHUNK {
                    self.ranges(&name, &expr, &ty, from, to)
                } else {
                    self.varobj_children(
                        reference,
                        &name,
                        &expr,
                        &ty,
                        Some((from, to)),
                        hex_request,
                    )?
                }
            }
            Handle::Registers(frame) => self.register_scope(frame)?,
            Handle::RegisterGroup(frame, group) => self.register_group(frame, group)?,
            Handle::RegisterBits { name, value } => cortex::fields_for(&name)
                .unwrap_or(&[])
                .iter()
                .map(|f| {
                    let range = if f.width == 1 {
                        format!("[{}]", f.lsb)
                    } else {
                        format!("[{}:{}]", f.lsb + f.width - 1, f.lsb)
                    };
                    read_only(var_json(
                        f.name,
                        &cortex::format_field(&name, f, f.extract(value)),
                        &format!("{range} {}", f.description),
                        0,
                    ))
                })
                .collect(),
            Handle::FaultStatus => self.fault_scope()?,
            Handle::FaultBits { which, value } => {
                let fields = if which == "HFSR" {
                    cortex::HFSR
                } else {
                    cortex::CFSR
                };
                fields
                    .iter()
                    .map(|f| {
                        read_only(var_json(
                            f.name,
                            &f.extract(value).to_string(),
                            f.description,
                            0,
                        ))
                    })
                    .collect()
            }
            Handle::Peripherals => self.peripheral_list(),
            Handle::Peripheral(idx) => self.peripheral_children(reference, idx, &[])?,
            Handle::PeripheralNode(idx, path) => self.peripheral_children(reference, idx, &path)?,
        };
        let start = req.arg_i64("start").unwrap_or(0).max(0) as usize;
        let count = req.arg_i64("count").unwrap_or(0).max(0) as usize;
        let vars: Vec<Value> = if count > 0 {
            vars.into_iter().skip(start).take(count).collect()
        } else {
            vars
        };
        body(json!({"variables": vars}))
    }

    /// Creates a variable object; returns (name, value, type, numchild).
    fn create_varobj(
        &mut self,
        expr: &str,
        frame: Option<FrameRef>,
        hex_request: bool,
    ) -> Result<(String, String, String, i64)> {
        let opts = frame.map(|f| format!("{} ", f.opts())).unwrap_or_default();
        let resp = self.mi(&format!("-var-create {opts}- * {}", mi_quote(expr)))?;
        self.note_side_effects(&resp);
        let name = resp
            .body
            .str("name")
            .ok_or_else(|| anyhow!("GDB did not create a variable object"))?
            .to_string();
        self.insp.varobjs.push(name.clone());
        let mut value = resp.body.str("value").unwrap_or("").to_string();
        let ty = resp.body.str("type").unwrap_or("").to_string();
        let numchild = resp.body.i64("numchild").unwrap_or(0)
            + i64::from(
                resp.body.str("dynamic") == Some("1") && resp.body.str("has_more") == Some("1"),
            );
        if (self.hex || hex_request) && numchild == 0 {
            if let Ok(r) = self.mi(&format!("-var-set-format {name} hexadecimal")) {
                if let Some(v) = r.body.str("value") {
                    value = v.to_string();
                }
            }
        }
        Ok((name, value, ty, numchild))
    }

    #[allow(clippy::too_many_arguments)]
    fn variable_entry(
        &mut self,
        container: i64,
        display: &str,
        varobj: &str,
        expr: &str,
        value: &str,
        ty: &str,
        numchild: i64,
    ) -> Value {
        let reference = if numchild > 0 {
            self.insp.handle(Handle::VarObj {
                name: varobj.to_string(),
                expr: expr.to_string(),
                ty: ty.to_string(),
                numchild,
            })
        } else {
            0
        };
        self.insp
            .data_targets
            .insert((container, display.to_string()), expr.to_string());
        let mut v = var_json(display, value, ty, reference);
        v["evaluateName"] = json!(expr);
        if numchild > 0 {
            v["indexedVariables"] = Value::Null;
        }
        if let Some(addr) = leading_address(value) {
            v["memoryReference"] = json!(format!("0x{addr:x}"));
        }
        if let Some(obj) = v.as_object_mut() {
            obj.retain(|_, val| !val.is_null());
        }
        v
    }

    fn locals(&mut self, container: i64, frame: FrameRef, hex_request: bool) -> Result<Vec<Value>> {
        let resp = self.mi(&format!(
            "-stack-list-variables {} --no-values",
            frame.opts()
        ))?;
        let mut names: Vec<(String, bool)> = resp
            .body
            .list("variables")
            .iter()
            .filter_map(|v| Some((v.str("name")?.to_string(), v.str("arg") == Some("1"))))
            .collect();
        // Arguments first, then locals; keep GDB order within each group.
        names.sort_by_key(|(_, is_arg)| !*is_arg);
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (name, _) in names {
            if !seen.insert(name.clone()) {
                continue; // shadowed variable of an outer block
            }
            match self.create_varobj(&name, Some(frame), hex_request) {
                Ok((obj, value, ty, numchild)) => out.push(
                    self.variable_entry(container, &name, &obj, &name, &value, &ty, numchild),
                ),
                Err(e) => out.push(read_only(var_json(&name, &format!("<{e}>"), "", 0))),
            }
        }
        Ok(out)
    }

    fn load_globals(&mut self) -> Result<Vec<GlobalSym>, String> {
        if let Some(g) = &self.insp.globals {
            return g.clone();
        }
        let result = self
            .mi_timeout("-symbol-info-variables", Duration::from_secs(60))
            .map_err(|e| format!("{e:#} (GDB 10 or newer is required)"))
            .map(|resp| {
                let mut all = Vec::new();
                if let Some(symbols) = resp.body.get("symbols") {
                    for file in symbols.list("debug") {
                        let filename = file.str("filename").unwrap_or("").to_string();
                        let fullname = file.str("fullname").unwrap_or(&filename).to_string();
                        for s in file.list("symbols") {
                            let Some(name) = s.str("name") else { continue };
                            let description = s.str("description").unwrap_or("");
                            all.push(GlobalSym {
                                name: name.to_string(),
                                file: filename.clone(),
                                fullname: fullname.clone(),
                                is_static: description.starts_with("static "),
                            });
                        }
                    }
                }
                // Prefer the project's own sources over library internals.
                let local: Vec<GlobalSym> = all
                    .iter()
                    .filter(|g| Path::new(&g.fullname).is_file())
                    .cloned()
                    .collect();
                let mut chosen = if local.is_empty() { all } else { local };
                chosen.sort_by(|a, b| a.name.cmp(&b.name).then(a.file.cmp(&b.file)));
                chosen
            });
        self.insp.globals = Some(result.clone());
        result
    }

    fn globals(
        &mut self,
        container: i64,
        statics_of: Option<&str>,
        hex_request: bool,
    ) -> Result<Vec<Value>> {
        let symbols = match self.load_globals() {
            Ok(s) => s,
            Err(e) => return Ok(vec![read_only(var_json("<unavailable>", &e, "", 0))]),
        };
        let selected: Vec<GlobalSym> = symbols
            .into_iter()
            .filter(|g| match statics_of {
                Some(file) => g.is_static && g.fullname == file,
                None => !g.is_static,
            })
            .collect();
        let mut out = Vec::new();
        for g in selected.iter().take(MAX_GLOBALS) {
            let basename = Path::new(&g.file)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| g.file.clone());
            let expr = if basename.is_empty() {
                g.name.clone()
            } else {
                format!("'{basename}'::{}", g.name)
            };
            match self.create_varobj(&expr, None, hex_request) {
                Ok((obj, value, ty, numchild)) => {
                    let mut v = self
                        .variable_entry(container, &g.name, &obj, &g.name, &value, &ty, numchild);
                    v["evaluateName"] = json!(expr);
                    out.push(v);
                }
                Err(e) => out.push(read_only(var_json(&g.name, &format!("<{e}>"), "", 0))),
            }
        }
        if selected.len() > MAX_GLOBALS {
            out.push(read_only(var_json(
                "...",
                &format!("{} more not shown", selected.len() - MAX_GLOBALS),
                "",
                0,
            )));
        }
        Ok(out)
    }

    fn ranges(&mut self, name: &str, expr: &str, ty: &str, from: i64, to: i64) -> Vec<Value> {
        let span = to - from;
        let mut step = CHUNK;
        while span / step > CHUNK {
            step *= CHUNK;
        }
        let mut out = Vec::new();
        let mut start = from;
        while start < to {
            let end = (start + step).min(to);
            let reference = self.insp.handle(Handle::VarRange {
                name: name.to_string(),
                expr: expr.to_string(),
                ty: ty.to_string(),
                from: start,
                to: end,
            });
            out.push(var_json(
                &format!("[{start}..{}]", end - 1),
                "",
                "",
                reference,
            ));
            start = end;
        }
        out
    }

    fn varobj_children(
        &mut self,
        container: i64,
        name: &str,
        expr: &str,
        ty: &str,
        range: Option<(i64, i64)>,
        hex_request: bool,
    ) -> Result<Vec<Value>> {
        let range_args = range.map(|(a, b)| format!(" {a} {b}")).unwrap_or_default();
        let resp = self.mi(&format!(
            "-var-list-children --all-values {name}{range_args}"
        ))?;
        let mut out = Vec::new();
        for child in resp.body.list("children") {
            let cname = child.str("name").unwrap_or("").to_string();
            let exp = child.str("exp").unwrap_or("").to_string();
            let cty = child.str("type").unwrap_or("").to_string();
            let numchild = child.i64("numchild").unwrap_or(0);
            if is_access_specifier(&exp) && cty.is_empty() {
                out.extend(self.varobj_children(container, &cname, expr, ty, None, hex_request)?);
                continue;
            }
            let mut value = child.str("value").unwrap_or("").to_string();
            if (self.hex || hex_request) && numchild == 0 {
                if let Ok(r) = self.mi(&format!("-var-set-format {cname} hexadecimal")) {
                    if let Some(v) = r.body.str("value") {
                        value = v.to_string();
                    }
                }
            }
            let display = if exp.chars().all(|c| c.is_ascii_digit()) && !exp.is_empty() {
                format!("[{exp}]")
            } else {
                exp.clone()
            };
            let child_expr = child_expression(expr, ty, &exp);
            out.push(self.variable_entry(
                container,
                &display,
                &cname,
                &child_expr,
                &value,
                &cty,
                numchild,
            ));
        }
        Ok(out)
    }

    // ----- registers --------------------------------------------------------

    fn register_names(&mut self) -> Result<Vec<String>> {
        if let Some(n) = &self.insp.register_names {
            return Ok(n.clone());
        }
        let resp = self.mi("-data-list-register-names")?;
        let names: Vec<String> = resp
            .body
            .list("register-names")
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        self.insp.register_names = Some(names.clone());
        Ok(names)
    }

    /// (name, value) of all available registers in `frame`.
    fn registers(&mut self, frame: FrameRef) -> Result<Vec<(String, String)>> {
        if let Some(r) = self.insp.registers.get(&frame) {
            return Ok(r.clone());
        }
        let names = self.register_names()?;
        let resp = self.mi(&format!(
            "-data-list-register-values {} --skip-unavailable x",
            frame.opts()
        ))?;
        let mut values: HashMap<usize, String> = HashMap::new();
        for v in resp.body.list("register-values") {
            if let (Some(n), Some(val)) = (v.i64("number"), v.str("value")) {
                values.insert(n as usize, val.to_string());
            }
        }
        let float_numbers: Vec<String> = names
            .iter()
            .enumerate()
            .filter(|(i, n)| cortex::is_float_register(n) && values.contains_key(i))
            .map(|(i, _)| i.to_string())
            .collect();
        if !float_numbers.is_empty() {
            if let Ok(resp) = self.mi(&format!(
                "-data-list-register-values {} --skip-unavailable N {}",
                frame.opts(),
                float_numbers.join(" ")
            )) {
                for v in resp.body.list("register-values") {
                    if let (Some(n), Some(val)) = (v.i64("number"), v.str("value")) {
                        values.insert(n as usize, val.to_string());
                    }
                }
            }
        }
        let regs: Vec<(String, String)> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty())
            .filter_map(|(i, n)| values.get(&i).map(|v| (n.clone(), v.clone())))
            .collect();
        self.insp.registers.insert(frame, regs.clone());
        Ok(regs)
    }

    fn register_value(&mut self, name: &str) -> Option<u64> {
        let frame = FrameRef {
            thread: self.last_stop.as_ref().map_or(1, |s| s.thread),
            level: 0,
        };
        self.registers(frame)
            .ok()?
            .into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| parse_u64(&v))
    }

    fn register_entry(&mut self, name: &str, value: &str) -> Value {
        let reference = match (cortex::fields_for(name), parse_u64(value)) {
            (Some(_), Some(v)) => self.insp.handle(Handle::RegisterBits {
                name: name.to_string(),
                value: v,
            }),
            _ => 0,
        };
        let mut shown = value.to_string();
        if name.eq_ignore_ascii_case("xpsr") {
            if let Some(v) = parse_u64(value) {
                let isr = v & 0x1FF;
                if isr != 0 {
                    shown = format!("{value} ({})", cortex::exception_name(isr));
                }
            }
        }
        let mut v = var_json(name, &shown, "", reference);
        v["evaluateName"] = json!(format!("${name}"));
        v
    }

    fn register_scope(&mut self, frame: FrameRef) -> Result<Vec<Value>> {
        let regs = self.registers(frame)?;
        let mut out = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        for (name, value) in &regs {
            let g = cortex::classify(name);
            if g == Group::Core {
                out.push(self.register_entry(name, value));
            } else if !groups.contains(&g) {
                groups.push(g);
            }
        }
        groups.sort();
        for g in groups {
            let reference = self.insp.handle(Handle::RegisterGroup(frame, g));
            out.push(var_json(g.label(), "", "", reference));
        }
        if frame.level == 0 {
            let reference = self.insp.handle(Handle::FaultStatus);
            out.push(var_json("Fault status (SCB)", "", "", reference));
        }
        Ok(out)
    }

    fn register_group(&mut self, frame: FrameRef, group: Group) -> Result<Vec<Value>> {
        let regs = self.registers(frame)?;
        Ok(regs
            .iter()
            .filter(|(n, _)| cortex::classify(n) == group)
            .map(|(n, v)| self.register_entry(n, v))
            .collect())
    }

    fn read_fault_status(&mut self) -> Result<FaultStatus> {
        let bytes = self.read_bytes(cortex::SCB_CFSR, cortex::FAULT_BLOCK_LEN)?;
        FaultStatus::from_block(&bytes)
            .ok_or_else(|| anyhow!("cannot read the SCB fault registers"))
    }

    fn fault_scope(&mut self) -> Result<Vec<Value>> {
        let status = match self.read_fault_status() {
            Ok(s) => s,
            Err(e) => {
                return Ok(vec![read_only(var_json(
                    "<unavailable>",
                    &format!("{e:#}"),
                    "",
                    0,
                ))])
            }
        };
        let cfsr_ref = self.insp.handle(Handle::FaultBits {
            which: "CFSR",
            value: status.cfsr,
        });
        let hfsr_ref = self.insp.handle(Handle::FaultBits {
            which: "HFSR",
            value: status.hfsr,
        });
        Ok(vec![
            read_only(var_json(
                "CFSR",
                &format!(
                    "{}{}",
                    hex(status.cfsr, 32),
                    FaultStatus::summary(cortex::CFSR, status.cfsr)
                ),
                "Configurable Fault Status (0xE000ED28)",
                cfsr_ref,
            )),
            read_only(var_json(
                "HFSR",
                &format!(
                    "{}{}",
                    hex(status.hfsr, 32),
                    FaultStatus::summary(cortex::HFSR, status.hfsr)
                ),
                "HardFault Status (0xE000ED2C)",
                hfsr_ref,
            )),
            read_only(var_json(
                "MMFAR",
                &hex(status.mmfar, 32),
                "MemManage Fault Address",
                0,
            )),
            read_only(var_json(
                "BFAR",
                &hex(status.bfar, 32),
                "BusFault Address",
                0,
            )),
        ])
    }

    pub(super) fn current_exception_name(&mut self) -> Option<String> {
        let xpsr = self.register_value("xpsr")?;
        let isr = xpsr & 0x1FF;
        (isr != 0).then(|| cortex::exception_name(isr))
    }

    /// Explains a fault: status registers and the stacked exception frame.
    pub(super) fn fault_report(&mut self) -> Vec<String> {
        let Ok(status) = self.read_fault_status() else {
            return Vec::new();
        };
        let mut lines = status.describe();
        let lr = self.register_value("lr");
        if let Some(uses_psp) = lr.and_then(cortex::exc_return_uses_psp) {
            let sp_name = if uses_psp { "psp" } else { "msp" };
            if let Some(sp) = self
                .register_value(sp_name)
                .or_else(|| self.register_value("sp"))
            {
                if let Ok(bytes) = self.read_bytes(sp, 32) {
                    let words: Vec<u64> = bytes.chunks(4).map(le_to_u64).collect();
                    let stacked: Vec<String> = cortex::STACKED
                        .iter()
                        .zip(&words)
                        .map(|(n, v)| format!("{n}={}", hex(*v, 32)))
                        .collect();
                    lines.push(format!(
                        "  Stacked frame on {} at {}:",
                        sp_name.to_uppercase(),
                        hex(sp, 32)
                    ));
                    lines.push(format!("    {}", stacked.join(" ")));
                    if let Some(pc) = words.get(6) {
                        let location = self
                            .cli(&format!("info line *0x{pc:x}"), Duration::from_secs(5))
                            .unwrap_or_default();
                        let location = location.lines().next().unwrap_or("").trim().to_string();
                        lines.push(format!(
                            "  The fault happened at PC {}{}",
                            hex(*pc, 32),
                            if location.is_empty() {
                                String::new()
                            } else {
                                format!(": {location}")
                            }
                        ));
                    }
                }
            }
        }
        lines
    }

    // ----- peripherals -----------------------------------------------------

    fn peripheral_list(&mut self) -> Vec<Value> {
        let Some(svd) = self.svd.clone() else {
            return Vec::new();
        };
        svd.peripherals
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let reference = self.insp.handle(Handle::Peripheral(i));
                let mut v = var_json(
                    &p.name,
                    &format!("@ {}", hex(p.base_address, 32)),
                    &p.description,
                    reference,
                );
                v["presentationHint"] = json!({"kind": "class"});
                v
            })
            .collect()
    }

    fn read_peripheral(&mut self, idx: usize) {
        let Some(svd) = self.svd.clone() else { return };
        let Some(p) = svd.peripherals.get(idx) else {
            return;
        };
        let regs = p.registers();
        if regs
            .iter()
            .all(|r| self.insp.peripheral_values.contains_key(&r.address))
        {
            return;
        }
        let mut memory: HashMap<u64, u8> = HashMap::new();
        for (start, len) in read_plan(&regs) {
            if let Ok(bytes) = self.read_bytes(start, len) {
                for (i, b) in bytes.iter().enumerate() {
                    memory.insert(start + i as u64, *b);
                }
            }
        }
        for r in regs {
            let bytes: Option<Vec<u8>> = (0..r.bytes())
                .map(|i| memory.get(&(r.address + i)).copied())
                .collect();
            self.insp
                .peripheral_values
                .insert(r.address, bytes.map(|b| le_to_u64(&b)));
        }
    }

    fn peripheral_children(
        &mut self,
        container: i64,
        idx: usize,
        path: &[usize],
    ) -> Result<Vec<Value>> {
        let svd = self.svd.clone().ok_or_else(|| anyhow!("no SVD file"))?;
        let p = svd
            .peripherals
            .get(idx)
            .ok_or_else(|| anyhow!("unknown peripheral"))?;
        self.read_peripheral(idx);
        // Walk to the node.
        let mut nodes: &[RegisterNode] = &p.children;
        let mut register = None;
        for &i in path {
            match nodes.get(i) {
                Some(RegisterNode::Cluster(c)) => nodes = &c.children,
                Some(RegisterNode::Register(r)) => {
                    register = Some(r);
                    break;
                }
                None => bail!("invalid peripheral path"),
            }
        }
        if let Some(r) = register {
            let value = self
                .insp
                .peripheral_values
                .get(&r.address)
                .copied()
                .flatten();
            return Ok(r
                .fields
                .iter()
                .map(|f| {
                    let shown = match value {
                        Some(v) if r.auto_readable() => f.format(f.extract(v)),
                        _ => "?".into(),
                    };
                    let ty = format!("{} {}", f.bit_range(), f.access.short());
                    let mut v = var_json(&f.name, &shown, &ty, 0);
                    if !f.access.writable() || !r.access.writable() {
                        v = read_only(v);
                    }
                    v
                })
                .collect());
        }
        let mut out = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            let mut child_path = path.to_vec();
            child_path.push(i);
            match node {
                RegisterNode::Cluster(c) => {
                    let reference = self.insp.handle(Handle::PeripheralNode(idx, child_path));
                    out.push(var_json(
                        &c.name,
                        &format!("@ {}", hex(c.address, 32)),
                        &c.description,
                        reference,
                    ));
                }
                RegisterNode::Register(r) => {
                    let value = self
                        .insp
                        .peripheral_values
                        .get(&r.address)
                        .copied()
                        .flatten();
                    let shown = if !r.access.readable() {
                        "<write-only>".to_string()
                    } else if r.read_side_effects {
                        "<not read: reading has side effects; use :svd>".to_string()
                    } else {
                        value
                            .map(|v| r.format(v))
                            .unwrap_or_else(|| "<unreadable>".into())
                    };
                    let reference = if r.fields.is_empty() {
                        0
                    } else {
                        self.insp.handle(Handle::PeripheralNode(idx, child_path))
                    };
                    let ty = format!(
                        "{} {} {}",
                        hex(r.address, 32),
                        r.access.short(),
                        r.description
                    );
                    let width_type = match r.size {
                        8 => "unsigned char",
                        16 => "unsigned short",
                        _ => "unsigned int",
                    };
                    self.insp.data_targets.insert(
                        (container, r.name.clone()),
                        format!("*({width_type} *) 0x{:x}", r.address),
                    );
                    let mut v = var_json(&r.name, &shown, ty.trim(), reference);
                    v["memoryReference"] = json!(format!("0x{:x}", r.address));
                    if !r.access.writable() {
                        v = read_only(v);
                    }
                    out.push(v);
                }
            }
        }
        Ok(out)
    }

    /// Writes a peripheral register or field; returns the new display value.
    fn set_peripheral(
        &mut self,
        idx: usize,
        path: &[usize],
        name: &str,
        input: &str,
    ) -> Result<String> {
        let svd = self.svd.clone().ok_or_else(|| anyhow!("no SVD file"))?;
        let p = &svd.peripherals[idx];
        let mut nodes: &[RegisterNode] = &p.children;
        let mut register = None;
        for &i in path {
            match nodes.get(i) {
                Some(RegisterNode::Cluster(c)) => nodes = &c.children,
                Some(RegisterNode::Register(r)) => {
                    register = Some(r);
                    break;
                }
                None => bail!("invalid peripheral path"),
            }
        }
        let (reg, field) = match register {
            Some(r) => (
                r,
                Some(
                    r.fields
                        .iter()
                        .find(|f| f.name == name)
                        .ok_or_else(|| anyhow!("no field {name}"))?,
                ),
            ),
            None => {
                let r = nodes
                    .iter()
                    .find_map(|n| match n {
                        RegisterNode::Register(r) if r.name == name => Some(r),
                        _ => None,
                    })
                    .ok_or_else(|| anyhow!("no register {name}"))?;
                (r, None)
            }
        };
        if !reg.access.writable() {
            bail!("{} is read-only", reg.name);
        }
        let new_value = match field {
            Some(f) => {
                if !f.access.writable() {
                    bail!("{} is read-only", f.name);
                }
                let fv = f.parse_input(input)?;
                let current = if reg.auto_readable() {
                    le_to_u64(&self.read_bytes(reg.address, reg.bytes())?)
                } else {
                    reg.reset_value
                };
                f.insert(current, fv)
            }
            None => parse_int_bits(input, reg.size)
                .ok_or_else(|| anyhow!("`{input}` is not a valid {}-bit value", reg.size))?,
        };
        self.write_bytes(reg.address, &u64_to_le(new_value, reg.bytes() as usize))?;
        self.insp.peripheral_values.clear();
        let readback = if reg.auto_readable() {
            le_to_u64(&self.read_bytes(reg.address, reg.bytes())?)
        } else {
            new_value
        };
        Ok(match field {
            Some(f) => f.format(f.extract(readback)),
            None => reg.format(readback),
        })
    }

    // ----- memory ------------------------------------------------------------

    /// Reads exactly `len` bytes or fails.
    fn read_bytes(&mut self, addr: u64, len: u64) -> Result<Vec<u8>> {
        let (data, _) = self.read_memory_partial(addr, len)?;
        if (data.len() as u64) < len {
            bail!("cannot read {len} bytes at {}", hex(addr, 32));
        }
        Ok(data)
    }

    /// Reads up to `len` bytes; returns the contiguous readable prefix.
    fn read_memory_partial(&mut self, addr: u64, len: u64) -> Result<(Vec<u8>, u64)> {
        let resp = self.mi(&format!("-data-read-memory-bytes 0x{addr:x} {len}"))?;
        let mut data = Vec::new();
        let mut chunks: Vec<(u64, Vec<u8>)> = resp
            .body
            .list("memory")
            .iter()
            .filter_map(|m| {
                let begin = m.u64("begin")?;
                let offset = m.u64("offset").unwrap_or(0);
                Some((begin + offset, hex_decode(m.str("contents")?)?))
            })
            .collect();
        chunks.sort_by_key(|(a, _)| *a);
        let mut next = addr;
        for (start, bytes) in chunks {
            if start != next {
                break;
            }
            next += bytes.len() as u64;
            data.extend(bytes);
        }
        data.truncate(len as usize);
        Ok((data, addr))
    }

    fn write_bytes(&mut self, addr: u64, data: &[u8]) -> Result<()> {
        self.mi(&format!(
            "-data-write-memory-bytes 0x{addr:x} {}",
            hex_encode(data)
        ))?;
        Ok(())
    }

    pub(super) fn read_memory(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let reference = req.arg_str("memoryReference").unwrap_or("");
        let base = parse_u64(reference)
            .or_else(|| leading_address(reference))
            .ok_or_else(|| anyhow!("invalid memory reference `{reference}`"))?;
        let addr = (base as i64 + req.arg_i64("offset").unwrap_or(0)) as u64;
        let count = req.arg_i64("count").unwrap_or(0).max(0) as u64;
        if count == 0 {
            return body(json!({"address": format!("0x{addr:x}"), "data": ""}));
        }
        let data = self
            .read_memory_partial(addr, count)
            .map(|(d, _)| d)
            .unwrap_or_default();
        let unreadable = count - data.len() as u64;
        let mut b = json!({"address": format!("0x{addr:x}"), "data": base64_encode(&data)});
        if unreadable > 0 {
            b["unreadableBytes"] = json!(unreadable);
        }
        body(b)
    }

    pub(super) fn write_memory(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let reference = req.arg_str("memoryReference").unwrap_or("");
        let base = parse_u64(reference)
            .or_else(|| leading_address(reference))
            .ok_or_else(|| anyhow!("invalid memory reference `{reference}`"))?;
        let addr = (base as i64 + req.arg_i64("offset").unwrap_or(0)) as u64;
        let data = base64_decode(req.arg_str("data").unwrap_or(""))
            .ok_or_else(|| anyhow!("invalid base64 data"))?;
        self.write_bytes(addr, &data)?;
        self.insp.peripheral_values.clear();
        self.out.event(
            "memory",
            json!({"memoryReference": format!("0x{addr:x}"), "offset": 0, "count": data.len()}),
        );
        body(json!({"bytesWritten": data.len()}))
    }

    pub(super) fn disassemble(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let reference = req.arg_str("memoryReference").unwrap_or("");
        let base = parse_u64(reference)
            .or_else(|| leading_address(reference))
            .ok_or_else(|| anyhow!("invalid memory reference `{reference}`"))?;
        let base = (base as i64 + req.arg_i64("offset").unwrap_or(0)) as u64 & !1;
        let insn_offset = req.arg_i64("instructionOffset").unwrap_or(0);
        let count = req.arg_i64("instructionCount").unwrap_or(0).max(0);
        // Thumb instructions are 2 or 4 bytes: read generously around `base`.
        let before = (-insn_offset).max(0) as u64 * 4 + 16;
        let after = (insn_offset + count).max(0) as u64 * 4 + 16;
        let mut insns = Vec::new();
        for shift in [0u64, 2] {
            let start = base.saturating_sub(before + shift);
            let candidate = self
                .mi(&format!(
                    "-data-disassemble -s 0x{start:x} -e 0x{:x} -- 5",
                    base + after
                ))
                .map(|r| parse_disassembly(&r.body))
                .unwrap_or_default();
            if candidate.iter().any(|i| i.address == base) {
                insns = candidate;
                break;
            }
        }
        let anchor = insns.iter().position(|i| i.address == base);
        let mut out = Vec::new();
        for k in 0..count {
            let index = anchor.map(|a| a as i64 + insn_offset + k);
            match index
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| insns.get(i))
            {
                Some(i) => {
                    let mut v = json!({
                        "address": format!("0x{:08x}", i.address),
                        "instructionBytes": i.opcodes,
                        "instruction": i.pretty_text(),
                    });
                    if let Some(s) = i.symbol() {
                        v["symbol"] = json!(s);
                    }
                    if let Some(src) = &i.source {
                        if let Some(full) = &src.fullname {
                            v["location"] = json!({"path": full, "name": src.file});
                            v["line"] = json!(self.client_line(i64::from(src.line)));
                        }
                    }
                    out.push(v);
                }
                None => {
                    let addr = (base as i64 + (insn_offset + k) * 2).max(0);
                    out.push(json!({"address": format!("0x{addr:08x}"), "instruction": "??", "presentationHint": "invalid"}));
                }
            }
        }
        body(json!({"instructions": out}))
    }

    // ----- writing values -----------------------------------------------------

    pub(super) fn set_variable(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let reference = req.arg_i64("variablesReference").unwrap_or(0);
        let name = req.arg_str("name").unwrap_or("").to_string();
        let value = req.arg_str("value").unwrap_or("").trim().to_string();
        let handle = self
            .insp
            .handles
            .get(&reference)
            .cloned()
            .ok_or_else(|| anyhow!("unknown variables reference {reference}"))?;
        match handle {
            Handle::Registers(frame) | Handle::RegisterGroup(frame, _) => {
                let resp = self.mi(&format!(
                    "-data-evaluate-expression {} {}",
                    frame.opts(),
                    mi_quote(&format!("${name} = {value}"))
                ))?;
                self.insp.registers.clear();
                let regs = self.registers(frame)?;
                let shown = regs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| resp.body.str("value").unwrap_or("").to_string());
                body(json!({"value": shown}))
            }
            Handle::Peripheral(idx) => {
                body(json!({"value": self.set_peripheral(idx, &[], &name, &value)?}))
            }
            Handle::PeripheralNode(idx, path) => {
                body(json!({"value": self.set_peripheral(idx, &path, &name, &value)?}))
            }
            Handle::Locals(_)
            | Handle::Globals
            | Handle::Statics(_)
            | Handle::VarObj { .. }
            | Handle::VarRange { .. } => {
                let frame = match &handle {
                    Handle::Locals(f) => Some(*f),
                    _ => None,
                };
                let known = self
                    .insp
                    .data_targets
                    .get(&(reference, name.clone()))
                    .cloned();
                let expr = match (&handle, known) {
                    (_, Some(e)) => e,
                    // Top-level names are valid expressions on their own.
                    (Handle::Locals(_) | Handle::Globals | Handle::Statics(_), None) => {
                        name.clone()
                    }
                    _ => bail!("unknown variable {name}"),
                };
                let new_value = self.assign(&expr, &value, frame)?;
                body(json!({"value": new_value}))
            }
            _ => Err(anyhow!("{name} cannot be changed")),
        }
    }

    fn assign(&mut self, expr: &str, value: &str, frame: Option<FrameRef>) -> Result<String> {
        let opts = frame.map(|f| format!("{} ", f.opts())).unwrap_or_default();
        let resp = self.mi(&format!(
            "-data-evaluate-expression {opts}{}",
            mi_quote(&format!("{expr} = {value}"))
        ))?;
        self.note_side_effects(&resp);
        self.insp.peripheral_values.clear();
        let shown = resp.body.str("value").unwrap_or("").to_string();
        if self.hex {
            if let Ok(v) = shown.trim().parse::<i64>() {
                return Ok(format!("0x{:x}", v));
            }
        }
        Ok(shown)
    }

    pub(super) fn set_expression(&mut self, req: &Request) -> HandlerResult {
        self.require_stopped()?;
        let expr = req.arg_str("expression").unwrap_or("").to_string();
        let value = req.arg_str("value").unwrap_or("").to_string();
        let frame = self.frame_for(req.arg_i64("frameId"));
        let shown = self.assign(&expr, &value, frame)?;
        body(json!({"value": shown}))
    }

    // ----- evaluation and the console -----------------------------------------

    pub(super) fn evaluate(&mut self, req: &Request) -> HandlerResult {
        let expression = req.arg_str("expression").unwrap_or("").to_string();
        let frame = self.frame_for(req.arg_i64("frameId"));
        let hex_request = req
            .arg("format")
            .and_then(|f| f.get("hex"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match req.arg_str("context").unwrap_or("repl") {
            "repl" => self.repl(&expression, frame),
            context => {
                let mut result = self.evaluate_expression(&expression, frame, hex_request, true)?;
                if context == "hover" {
                    if let Some(obj) = result.as_object_mut() {
                        obj.remove("memoryReference");
                    }
                }
                body(result)
            }
        }
    }

    fn evaluate_expression(
        &mut self,
        expression: &str,
        frame: Option<FrameRef>,
        hex_request: bool,
        with_address: bool,
    ) -> Result<Value> {
        self.require_stopped()?;
        let frame = frame.or_else(|| {
            self.last_stop.as_ref().map(|s| FrameRef {
                thread: s.thread,
                level: 0,
            })
        });
        let (name, value, ty, numchild) = self.create_varobj(expression, frame, hex_request)?;
        let reference = if numchild > 0 {
            self.insp.handle(Handle::VarObj {
                name,
                expr: expression.to_string(),
                ty: ty.clone(),
                numchild,
            })
        } else {
            0
        };
        let mut result = json!({"result": value, "variablesReference": reference});
        if !ty.is_empty() {
            result["type"] = json!(ty);
        }
        if with_address {
            let address = if let Some(a) = leading_address(&value) {
                Some(a)
            } else if let Some(n) = parse_u64(expression.trim()) {
                Some(n)
            } else {
                let opts = frame.map(|f| format!("{} ", f.opts())).unwrap_or_default();
                self.mi(&format!(
                    "-data-evaluate-expression {opts}{}",
                    mi_quote(&format!("&({expression})"))
                ))
                .ok()
                .and_then(|r| r.body.str("value").and_then(leading_address))
            };
            if let Some(a) = address {
                result["memoryReference"] = json!(format!("0x{a:x}"));
            }
        }
        Ok(result)
    }

    fn console_result(text: &str) -> HandlerResult {
        body(json!({"result": text.trim_end(), "variablesReference": 0}))
    }

    fn repl(&mut self, input: &str, frame: Option<FrameRef>) -> HandlerResult {
        let t = input.trim();
        if t.is_empty() {
            return Self::console_result("");
        }
        if let Some(cmd) = t.strip_prefix(':') {
            return self.haltline_command(cmd.trim());
        }
        if let Some(cmd) = t.strip_prefix('>').or_else(|| t.strip_prefix("-exec ")) {
            let out = self.repl_cli(cmd.trim(), frame)?;
            return Self::console_result(&out);
        }
        if looks_like_cli(t) {
            let out = self.repl_cli(t, frame)?;
            return Self::console_result(&out);
        }
        match self.evaluate_expression(t, frame, false, true) {
            Ok(v) => body(v),
            Err(expr_err) => match self.repl_cli(t, frame) {
                Ok(out) => Self::console_result(&out),
                Err(_) => Err(expr_err),
            },
        }
    }

    fn repl_cli(&mut self, cmd: &str, frame: Option<FrameRef>) -> Result<String> {
        let opts = match (self.state, frame) {
            (TargetState::Stopped, Some(f)) => format!("{} ", f.opts()),
            _ => String::new(),
        };
        let was_stopped = self.state == TargetState::Stopped;
        let resp = self.mi_timeout(
            &format!("-interpreter-exec {opts}console {}", mi_quote(cmd)),
            Duration::from_secs(60),
        )?;
        if was_stopped && resp.class != "running" {
            self.note_side_effects(&resp);
        }
        if resp.class == "running" {
            self.state = TargetState::Running;
            self.on_external_resume();
        } else {
            self.insp.registers.clear();
            self.insp.peripheral_values.clear();
        }
        Ok(resp.console)
    }

    fn haltline_command(&mut self, cmd: &str) -> HandlerResult {
        let mut parts = cmd.split_whitespace();
        let name = parts.next().unwrap_or("help");
        let arg = parts.collect::<Vec<_>>().join(" ");
        match name {
            "help" | "?" => Self::console_result(HELP),
            "asm" => {
                self.asm_override = match arg.as_str() {
                    "on" => Some(true),
                    "off" => Some(false),
                    "auto" => None,
                    "" => Some(!self.asm_view_active()),
                    other => bail!("usage: :asm [on|off|auto] (got `{other}`)"),
                };
                self.refresh_client();
                Self::console_result(&format!(
                    "Disassembly view: {}",
                    match self.asm_override {
                        Some(true) => "on (steps go by instruction)",
                        Some(false) => "off",
                        None => "auto",
                    }
                ))
            }
            "hex" => {
                self.hex = match arg.as_str() {
                    "on" => true,
                    "off" => false,
                    "" => !self.hex,
                    other => bail!("usage: :hex [on|off] (got `{other}`)"),
                };
                self.refresh_client();
                Self::console_result(&format!(
                    "Hexadecimal values: {}",
                    if self.hex { "on" } else { "off" }
                ))
            }
            "reset" => {
                self.reset_target()?;
                if arg == "run" {
                    self.start_execution()?;
                } else {
                    self.report_stop("entry", Some("Reset".into()), 1, None);
                }
                Self::console_result("Target reset")
            }
            "fault" => {
                self.require_stopped()?;
                let lines = self.fault_report();
                if lines.is_empty() {
                    Self::console_result("Cannot read the fault status registers")
                } else {
                    let exception = self
                        .current_exception_name()
                        .map(|e| format!("Active exception: {e}\n"))
                        .unwrap_or_default();
                    Self::console_result(&format!("{exception}{}", lines.join("\n")))
                }
            }
            "svd" => {
                self.require_stopped()?;
                let text = self.svd_command(&arg)?;
                Self::console_result(&text)
            }
            "mi" => {
                if arg.is_empty() {
                    bail!("usage: :mi -command [args]");
                }
                let resp = self.mi_timeout(&arg, Duration::from_secs(60))?;
                let mut text = format!("^{}", resp.class);
                if let Mi::Tuple(items) = &resp.body {
                    for (k, v) in items {
                        text.push_str(&format!(",{k}={v}"));
                    }
                }
                Self::console_result(&format!("{}{text}", resp.console))
            }
            other => bail!("unknown command `:{other}` (try :help)"),
        }
    }

    fn svd_command(&mut self, arg: &str) -> Result<String> {
        let svd = self
            .svd
            .clone()
            .ok_or_else(|| anyhow!("no SVD file is configured (`svdFile`)"))?;
        if arg.is_empty() {
            let names: Vec<String> = svd
                .peripherals
                .iter()
                .map(|p| format!("{:<14} {}", p.name, hex(p.base_address, 32)))
                .collect();
            return Ok(names.join("\n"));
        }
        let (pname, rname) = match arg.split_once('.') {
            Some((p, r)) => (p, Some(r)),
            None => (arg, None),
        };
        let p = svd
            .find_peripheral(pname)
            .ok_or_else(|| anyhow!("no peripheral `{pname}` in the SVD file"))?;
        let regs: Vec<_> = p
            .registers()
            .into_iter()
            .filter(|r| rname.is_none_or(|n| r.name.eq_ignore_ascii_case(n)))
            .collect();
        if regs.is_empty() {
            bail!("no register `{}` in {}", rname.unwrap_or(""), p.name);
        }
        let mut lines = Vec::new();
        for r in regs {
            // An explicit request may read registers with read side effects.
            let explicit = rname.is_some();
            let value = if r.access.readable() && (explicit || !r.read_side_effects) {
                self.read_bytes(r.address, r.bytes())
                    .ok()
                    .map(|b| le_to_u64(&b))
            } else {
                None
            };
            lines.push(format!(
                "{}.{} @ {} = {}",
                p.name,
                r.name,
                hex(r.address, 32),
                value
                    .map(|v| r.format(v))
                    .unwrap_or_else(|| "<not read>".into())
            ));
            if let Some(v) = value {
                for f in &r.fields {
                    lines.push(format!(
                        "    {:<12} {:<8} {}",
                        f.name,
                        f.bit_range(),
                        f.format(f.extract(v))
                    ));
                }
            }
        }
        Ok(lines.join("\n"))
    }

    pub(super) fn exception_info(&mut self, _req: &Request) -> HandlerResult {
        let info = self
            .last_stop
            .clone()
            .filter(|s| s.reason == "exception")
            .ok_or_else(|| anyhow!("the target did not stop on an exception"))?;
        body(json!({
            "exceptionId": info.description.clone().unwrap_or_else(|| "Fault".into()),
            "description": info.text.clone().unwrap_or_default(),
            "breakMode": "always",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_child_expressions() {
        assert_eq!(child_expression("p", "struct point", "x"), "p.x");
        assert_eq!(child_expression("p", "struct point *", "x"), "p->x");
        assert_eq!(child_expression("buf", "uint8_t [16]", "3"), "buf[3]");
        assert_eq!(child_expression("ptr", "int *", "*ptr"), "*ptr");
        assert_eq!(child_expression("a + b", "struct s", "x"), "(a + b).x");
        assert_eq!(
            child_expression("'main.c'::g", "struct s", "x"),
            "('main.c'::g).x"
        );
        assert_eq!(child_expression("s.arr", "int [4]", "0"), "s.arr[0]");
    }

    #[test]
    fn recognises_cli_input() {
        assert!(looks_like_cli("info registers"));
        assert!(looks_like_cli("x/4xw 0x20000000"));
        assert!(looks_like_cli("monitor reset halt"));
        assert!(looks_like_cli("bt"));
        assert!(looks_like_cli("p/x g_counter"));
        assert!(!looks_like_cli("x"));
        assert!(!looks_like_cli("p"));
        assert!(!looks_like_cli("g_counter + 1"));
        assert!(!looks_like_cli("counter"));
    }
}
