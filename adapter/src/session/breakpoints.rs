//! Breakpoint requests: source, function, instruction, data (watchpoints) and
//! the "faults" exception filter. Conditions are evaluated by GDB; hit counts
//! and log points by the adapter.

use super::{body, HandlerResult, Session, TargetState};
use crate::breakpoints::{format_log_message, parse_log_message, HitCondition, LogPart};
use crate::dap::{Category, Request};
use crate::mi::Value as Mi;
use crate::util::{leading_address, mi_quote, parse_u64};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BpClass {
    Source,
    Function,
    Instruction,
    Data,
    Fault,
}

#[derive(Debug, Clone)]
pub struct Bp {
    pub id: i64,
    pub class: BpClass,
    /// Source path for source breakpoints, empty otherwise.
    pub group: String,
    /// Identity used to keep unchanged breakpoints (and their hit counts).
    pub key: String,
    pub gdb: Option<String>,
    pub hit_condition: Option<HitCondition>,
    pub log: Option<Vec<LogPart>>,
    pub hits: u64,
    pub verified: bool,
    pub line: Option<i64>,
    pub path: Option<String>,
    pub address: Option<u64>,
    pub message: Option<String>,
}

pub enum HitAction {
    /// Not a stop for the user (log point, unmet hit condition).
    Resume,
    Stop {
        id: i64,
        kind: BpClass,
    },
    /// Not one of our breakpoints.
    Unknown,
}

/// Breakpoint request entry, common to all kinds.
struct Spec {
    condition: Option<String>,
    hit_condition: Option<String>,
    log_message: Option<String>,
}

impl Spec {
    fn from(v: &Value) -> Self {
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            condition: s("condition"),
            hit_condition: s("hitCondition"),
            log_message: s("logMessage"),
        }
    }

    fn key(&self, location: &str) -> String {
        format!(
            "{location}|{}|{}|{}",
            self.condition.as_deref().unwrap_or(""),
            self.hit_condition.as_deref().unwrap_or(""),
            self.log_message.as_deref().unwrap_or("")
        )
    }
}

/// Reads the resolved location from a `bkpt` tuple.
fn bkpt_location(bkpt: &Mi) -> (bool, Option<i64>, Option<String>, Option<u64>) {
    let first_location = bkpt.list("locations").first();
    let pending = bkpt.get("pending").is_some() || bkpt.str("addr") == Some("<PENDING>");
    let line = bkpt
        .i64("line")
        .or_else(|| first_location.and_then(|l| l.i64("line")));
    let file = bkpt
        .str("fullname")
        .or_else(|| first_location.and_then(|l| l.str("fullname")))
        .map(str::to_string);
    let addr = bkpt
        .u64("addr")
        .or_else(|| first_location.and_then(|l| l.u64("addr")));
    (!pending, line, file, addr)
}

impl Session {
    fn bp_json(&self, bp: &Bp) -> Value {
        let mut v = json!({"id": bp.id, "verified": bp.verified});
        if let Some(line) = bp.line {
            v["line"] = json!(self.client_line(line));
        }
        if let Some(path) = &bp.path {
            v["source"] = json!({"path": path});
        }
        if let Some(addr) = bp.address {
            v["instructionReference"] = json!(format!("0x{addr:08x}"));
        }
        if let Some(m) = &bp.message {
            v["message"] = json!(m);
        }
        v
    }

    fn new_bp(&mut self, class: BpClass, group: &str, key: String) -> Bp {
        let id = self.next_bp_id;
        self.next_bp_id += 1;
        Bp {
            id,
            class,
            group: group.to_string(),
            key,
            gdb: None,
            hit_condition: None,
            log: None,
            hits: 0,
            verified: false,
            line: None,
            path: None,
            address: None,
            message: None,
        }
    }

    /// Runs `f` with the target halted (pausing and resuming a running target).
    fn with_halted<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let resume = self.halt_for_internal_op()?;
        let result = f(self);
        if resume {
            self.resume_silently();
        }
        result
    }

    fn delete_gdb_bp(&mut self, bp: &Bp) {
        if let Some(n) = &bp.gdb {
            if let Err(e) = self.mi(&format!("-break-delete {n}")) {
                self.warn(&format!("cannot delete breakpoint {n}: {e}"));
            }
        }
    }

    /// Inserts a GDB breakpoint and fills in `bp` from the result.
    fn insert_bp(&mut self, bp: &mut Bp, location: &str, spec: &Spec) {
        match spec
            .hit_condition
            .as_deref()
            .map(HitCondition::parse)
            .transpose()
        {
            Ok(hc) => bp.hit_condition = hc.flatten(),
            Err(e) => {
                bp.message = Some(e.to_string());
                return;
            }
        }
        bp.log = spec.log_message.as_deref().map(parse_log_message);
        let mut cmd = String::from("-break-insert -f");
        if self.config.force_hardware_breakpoints {
            cmd.push_str(" -h");
        }
        if let Some(c) = &spec.condition {
            cmd.push_str(&format!(" -c {}", mi_quote(c)));
        }
        cmd.push(' ');
        cmd.push_str(location);
        match self.mi(&cmd) {
            Ok(resp) => {
                let Some(bkpt) = resp.body.get("bkpt") else {
                    bp.message = Some("GDB returned no breakpoint".into());
                    return;
                };
                bp.gdb = bkpt.str("number").map(str::to_string);
                let (verified, line, file, addr) = bkpt_location(bkpt);
                bp.verified = verified;
                if bp.class == BpClass::Source {
                    // GDB may move the breakpoint to the next line with code.
                    if line.is_some() {
                        bp.line = line;
                    }
                } else {
                    bp.line = line;
                    bp.path = file;
                }
                if bp.address.is_none() {
                    bp.address = addr;
                }
                if !verified {
                    bp.message = Some("Pending: the location is not in the loaded program".into());
                }
            }
            Err(e) => bp.message = Some(e.to_string()),
        }
    }

    pub(super) fn set_breakpoints(&mut self, req: &Request) -> HandlerResult {
        let path = req
            .arg("source")
            .and_then(|s| s.get("path"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("setBreakpoints needs source.path"))?
            .to_string();
        let specs: Vec<(i64, Spec)> = match req.arg("breakpoints").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .map(|b| {
                    (
                        b.get("line").and_then(Value::as_i64).unwrap_or(1),
                        Spec::from(b),
                    )
                })
                .collect(),
            None => req
                .arg("lines")
                .and_then(Value::as_array)
                .map(|l| {
                    l.iter()
                        .filter_map(Value::as_i64)
                        .map(|line| (line, Spec::from(&Value::Null)))
                        .collect()
                })
                .unwrap_or_default(),
        };
        if self.gdb.is_none() {
            let results: Vec<Value> = specs
                .iter()
                .map(|(line, _)| json!({"verified": false, "line": line, "message": "The session is not started"}))
                .collect();
            return body(json!({"breakpoints": results}));
        }
        let results = self.with_halted(|s| Ok(s.apply_source_breakpoints(&path, specs)))?;
        body(json!({"breakpoints": results}))
    }

    fn apply_source_breakpoints(&mut self, path: &str, specs: Vec<(i64, Spec)>) -> Vec<Value> {
        let is_asm = self.disasm.is_asm_path(Path::new(path));
        let (mut old, rest): (Vec<Bp>, Vec<Bp>) = std::mem::take(&mut self.bps)
            .into_iter()
            .partition(|b| b.class == BpClass::Source && b.group == path);
        self.bps = rest;
        let mut kept = Vec::new();
        let mut results = Vec::new();
        for (client_line, spec) in specs {
            let line = self.gdb_line(client_line);
            let key = spec.key(&line.to_string());
            if let Some(pos) = old.iter().position(|b| b.key == key) {
                let bp = old.remove(pos);
                results.push(self.bp_json(&bp));
                kept.push(bp);
                continue;
            }
            let mut bp = self.new_bp(BpClass::Source, path, key);
            bp.path = Some(path.to_string());
            bp.line = Some(line);
            if is_asm {
                match self.disasm.address_for_line(Path::new(path), line as u32) {
                    Some((addr, snapped)) => {
                        bp.line = Some(i64::from(snapped));
                        bp.address = Some(addr);
                        self.insert_bp(&mut bp, &format!("*0x{addr:x}"), &spec);
                        bp.line = Some(i64::from(snapped));
                        bp.path = Some(path.to_string());
                    }
                    None => bp.message = Some("No instruction on this line".into()),
                }
            } else {
                let gdb_path = path.replace('\\', "/");
                let location = format!("--source {} --line {line}", mi_quote(&gdb_path));
                self.insert_bp(&mut bp, &location, &spec);
            }
            results.push(self.bp_json(&bp));
            kept.push(bp);
        }
        for bp in old {
            self.delete_gdb_bp(&bp);
        }
        self.bps.extend(kept);
        results
    }

    /// Replaces all breakpoints of `class` with new ones built by `locate`.
    fn replace_class(&mut self, class: BpClass, items: Vec<(String, Spec)>) -> Vec<Value> {
        let (mut old, rest): (Vec<Bp>, Vec<Bp>) = std::mem::take(&mut self.bps)
            .into_iter()
            .partition(|b| b.class == class);
        self.bps = rest;
        let mut results = Vec::new();
        let mut kept = Vec::new();
        for (location, spec) in items {
            let key = spec.key(&location);
            if let Some(pos) = old.iter().position(|b| b.key == key) {
                let bp = old.remove(pos);
                results.push(self.bp_json(&bp));
                kept.push(bp);
                continue;
            }
            let mut bp = self.new_bp(class, "", key);
            if class == BpClass::Instruction {
                bp.address = location.strip_prefix('*').and_then(parse_u64);
            }
            self.insert_bp(&mut bp, &location, &spec);
            results.push(self.bp_json(&bp));
            kept.push(bp);
        }
        for bp in old {
            self.delete_gdb_bp(&bp);
        }
        self.bps.extend(kept);
        results
    }

    pub(super) fn set_function_breakpoints(&mut self, req: &Request) -> HandlerResult {
        let items: Vec<(String, Spec)> = req
            .arg("breakpoints")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|b| {
                        let name = b.get("name")?.as_str()?.trim();
                        (!name.is_empty()).then(|| (mi_quote(name), Spec::from(b)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.gdb.is_none() {
            return body(
                json!({"breakpoints": items.iter().map(|_| json!({"verified": false})).collect::<Vec<_>>()}),
            );
        }
        let results = self.with_halted(|s| Ok(s.replace_class(BpClass::Function, items)))?;
        body(json!({"breakpoints": results}))
    }

    pub(super) fn set_instruction_breakpoints(&mut self, req: &Request) -> HandlerResult {
        let items: Vec<(String, Spec)> = req
            .arg("breakpoints")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|b| {
                        let base = b
                            .get("instructionReference")
                            .and_then(Value::as_str)
                            .and_then(|r| parse_u64(r).or_else(|| leading_address(r)))?;
                        let offset = b.get("offset").and_then(Value::as_i64).unwrap_or(0);
                        let addr = (base as i64).wrapping_add(offset) as u64;
                        Some((format!("*0x{addr:x}"), Spec::from(b)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.gdb.is_none() {
            return body(
                json!({"breakpoints": items.iter().map(|_| json!({"verified": false})).collect::<Vec<_>>()}),
            );
        }
        let results = self.with_halted(|s| Ok(s.replace_class(BpClass::Instruction, items)))?;
        body(json!({"breakpoints": results}))
    }

    pub(super) fn set_exception_breakpoints(&mut self, req: &Request) -> HandlerResult {
        let mut filters: Vec<String> = req
            .arg("filters")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|f| f.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(opts) = req.arg("filterOptions").and_then(Value::as_array) {
            filters.extend(opts.iter().filter_map(|o| {
                o.get("filterId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }));
        }
        let enabled = filters.iter().any(|f| f == "faults");
        if self.gdb.is_none() {
            return body(
                json!({"breakpoints": filters.iter().map(|_| json!({"verified": false})).collect::<Vec<_>>()}),
            );
        }
        let verified = self.with_halted(|s| s.apply_fault_filter(enabled))?;
        let results: Vec<Value> = filters
            .iter()
            .map(|f| json!({"verified": f != "faults" || verified}))
            .collect();
        body(json!({"breakpoints": results}))
    }

    fn apply_fault_filter(&mut self, enabled: bool) -> Result<bool> {
        let (old, rest): (Vec<Bp>, Vec<Bp>) = std::mem::take(&mut self.bps)
            .into_iter()
            .partition(|b| b.class == BpClass::Fault);
        self.bps = rest;
        for bp in &old {
            self.delete_gdb_bp(bp);
        }
        self.fault_filter = enabled;
        if !enabled {
            return Ok(true);
        }
        // Handlers are often aliases of one Default_Handler: one breakpoint
        // per address saves the few hardware comparators Cortex-M has.
        let mut seen = Vec::new();
        let mut any = false;
        for handler in crate::cortex::FAULT_HANDLERS {
            let Ok(resp) = self.mi(&format!(
                "-data-evaluate-expression {}",
                mi_quote(&format!("&{handler}"))
            )) else {
                continue;
            };
            let Some(addr) = resp.body.str("value").and_then(leading_address) else {
                continue;
            };
            if seen.contains(&addr) {
                continue;
            }
            seen.push(addr);
            let mut bp = self.new_bp(BpClass::Fault, "", handler.to_string());
            let spec = Spec::from(&Value::Null);
            self.insert_bp(&mut bp, &mi_quote(handler), &spec);
            any |= bp.verified;
            self.bps.push(bp);
        }
        Ok(any || seen.is_empty())
    }

    pub(super) fn data_breakpoint_info(&mut self, req: &Request) -> HandlerResult {
        let name = req.arg_str("name").unwrap_or("").to_string();
        let reference = req.arg_i64("variablesReference").unwrap_or(0);
        let as_address = req.arg_bool("asAddress").unwrap_or(false);
        let expression = if as_address {
            let addr = parse_u64(&name).or_else(|| leading_address(&name));
            let bytes = req.arg_i64("bytes").unwrap_or(4);
            let ty = match bytes {
                1 => "unsigned char",
                2 => "unsigned short",
                8 => "unsigned long long",
                _ => "unsigned int",
            };
            addr.map(|a| format!("*({ty} *) 0x{a:x}"))
        } else if reference > 0 {
            self.insp
                .data_targets
                .get(&(reference, name.clone()))
                .cloned()
        } else if !name.trim().is_empty() {
            Some(name.trim().to_string())
        } else {
            None
        };
        match expression {
            Some(expr) => body(json!({
                "dataId": expr,
                "description": expr,
                "accessTypes": ["write", "read", "readWrite"],
                "canPersist": false,
            })),
            None => body(json!({
                "dataId": Value::Null,
                "description": format!("`{name}` cannot be watched"),
            })),
        }
    }

    pub(super) fn set_data_breakpoints(&mut self, req: &Request) -> HandlerResult {
        let items: Vec<(String, String, Spec)> = req
            .arg("breakpoints")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|b| {
                        let id = b.get("dataId")?.as_str()?.to_string();
                        let access = b
                            .get("accessType")
                            .and_then(Value::as_str)
                            .unwrap_or("write")
                            .to_string();
                        Some((id, access, Spec::from(b)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.gdb.is_none() {
            return body(
                json!({"breakpoints": items.iter().map(|_| json!({"verified": false})).collect::<Vec<_>>()}),
            );
        }
        let results = self.with_halted(|s| Ok(s.apply_watchpoints(items)))?;
        body(json!({"breakpoints": results}))
    }

    fn apply_watchpoints(&mut self, items: Vec<(String, String, Spec)>) -> Vec<Value> {
        let (mut old, rest): (Vec<Bp>, Vec<Bp>) = std::mem::take(&mut self.bps)
            .into_iter()
            .partition(|b| b.class == BpClass::Data);
        self.bps = rest;
        let mut results = Vec::new();
        let mut kept = Vec::new();
        for (expr, access, spec) in items {
            let key = spec.key(&format!("{access}:{expr}"));
            if let Some(pos) = old.iter().position(|b| b.key == key) {
                let bp = old.remove(pos);
                results.push(self.bp_json(&bp));
                kept.push(bp);
                continue;
            }
            let mut bp = self.new_bp(BpClass::Data, "", key);
            match spec
                .hit_condition
                .as_deref()
                .map(HitCondition::parse)
                .transpose()
            {
                Ok(hc) => bp.hit_condition = hc.flatten(),
                Err(e) => bp.message = Some(e.to_string()),
            }
            if bp.message.is_none() {
                let flag = match access.as_str() {
                    "read" => " -r",
                    "readWrite" => " -a",
                    _ => "",
                };
                match self.mi(&format!("-break-watch{flag} {}", mi_quote(&expr))) {
                    Ok(resp) => {
                        let number = ["wpt", "hw-rwpt", "hw-awpt"]
                            .iter()
                            .find_map(|k| resp.body.get(k).and_then(|w| w.str("number")))
                            .map(str::to_string);
                        bp.verified = number.is_some();
                        bp.gdb = number;
                        if let (Some(n), Some(c)) = (&bp.gdb, &spec.condition) {
                            if let Err(e) = self.mi(&format!("-break-condition {n} {c}")) {
                                bp.message = Some(format!("condition rejected: {e}"));
                            }
                        }
                    }
                    Err(e) => bp.message = Some(e.to_string()),
                }
            }
            results.push(self.bp_json(&bp));
            kept.push(bp);
        }
        for bp in old {
            self.delete_gdb_bp(&bp);
        }
        self.bps.extend(kept);
        results
    }

    /// Decides what to do when GDB reports breakpoint `number`.
    pub(super) fn on_breakpoint_hit(&mut self, number: &str, _body: &Mi) -> HitAction {
        let major = number.split('.').next().unwrap_or(number);
        let Some(idx) = self
            .bps
            .iter()
            .position(|b| b.gdb.as_deref() == Some(major))
        else {
            return HitAction::Unknown;
        };
        self.bps[idx].hits += 1;
        let bp = self.bps[idx].clone();
        if let Some(hc) = bp.hit_condition {
            if !hc.matches(bp.hits) {
                return HitAction::Resume;
            }
        }
        if let Some(parts) = &bp.log {
            let message = format_log_message(parts, |expr| {
                self.evaluate_for_log(expr)
                    .unwrap_or_else(|e| format!("<{e}>"))
            });
            self.out.output(Category::Console, &format!("{message}\n"));
            return HitAction::Resume;
        }
        HitAction::Stop {
            id: bp.id,
            kind: bp.class,
        }
    }

    fn evaluate_for_log(&mut self, expr: &str) -> Result<String> {
        let resp = self.mi(&format!("-data-evaluate-expression {}", mi_quote(expr)))?;
        if self.state == TargetState::Stopped {
            self.note_side_effects(&resp);
        }
        Ok(resp.body.str("value").unwrap_or("").to_string())
    }
}
