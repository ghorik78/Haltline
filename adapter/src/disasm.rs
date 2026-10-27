//! Disassembly support.
//!
//! Zed has no dedicated disassembly view and opens stack frames by file path
//! only, so the adapter renders the disassembly of each function into a real
//! `.s` file and points stack frames at it. Every instruction occupies one line
//! that starts with its address, which lets breakpoints set in these files be
//! mapped back to instruction addresses.

use crate::mi::Value;
use crate::util::{fnv1a, sanitize_file_name};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLine {
    pub file: String,
    pub fullname: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub address: u64,
    pub opcodes: String,
    pub text: String,
    pub func: Option<String>,
    pub offset: Option<u64>,
    /// Source line this instruction belongs to (mixed-mode output only).
    pub source: Option<SourceLine>,
}

impl Instruction {
    pub fn symbol(&self) -> Option<String> {
        self.func.as_ref().map(|f| match self.offset {
            Some(o) => format!("{f}+{o}"),
            None => f.clone(),
        })
    }

    /// Instruction text with the mnemonic padded, e.g. `push    {r7, lr}`.
    pub fn pretty_text(&self) -> String {
        let t = self.text.trim();
        match t.split_once('\t') {
            Some((mnemonic, rest)) => format!("{mnemonic:<8}{}", rest.replace('\t', " ")),
            None => t.to_string(),
        }
    }
}

fn parse_insn(v: &Value, source: Option<&SourceLine>) -> Option<Instruction> {
    Some(Instruction {
        address: v.u64("address")?,
        opcodes: v.str("opcodes").unwrap_or_default().to_string(),
        text: v.str("inst").unwrap_or_default().to_string(),
        func: v.str("func-name").map(str::to_string),
        offset: v.u64("offset"),
        source: source.cloned(),
    })
}

/// Parses the body of a `-data-disassemble` result (any mode).
pub fn parse_disassembly(body: &Value) -> Vec<Instruction> {
    let mut out = Vec::new();
    for item in body.list("asm_insns") {
        if item.get("line_asm_insn").is_some() {
            let source = item.i64("line").map(|line| SourceLine {
                file: item.str("file").unwrap_or_default().to_string(),
                fullname: item.str("fullname").map(str::to_string),
                line: line as u32,
            });
            for insn in item.list("line_asm_insn") {
                if let Some(i) = parse_insn(insn, source.as_ref()) {
                    out.push(i);
                }
            }
        } else if let Some(i) = parse_insn(item, None) {
            out.push(i);
        }
    }
    out.sort_by_key(|i| i.address);
    out.dedup_by_key(|i| i.address);
    out
}

/// A rendered disassembly file.
#[derive(Debug, Clone)]
pub struct AsmFile {
    pub path: PathBuf,
    pub start: u64,
    /// Exclusive end address.
    pub end: u64,
    /// Instruction address → 1-based line number.
    pub lines: BTreeMap<u64, u32>,
}

impl AsmFile {
    pub fn contains(&self, address: u64) -> bool {
        address >= self.start && address < self.end
    }

    pub fn line_of(&self, address: u64) -> Option<u32> {
        self.lines
            .range(..=address)
            .next_back()
            .filter(|_| self.contains(address))
            .map(|(_, l)| *l)
    }
}

/// Renders instructions into the text of a `.s` file and returns the
/// address → line mapping. `source_text(fullname, line)` supplies source lines
/// for the interleaved comments.
pub fn render(
    title: &str,
    insns: &[Instruction],
    mut source_text: impl FnMut(&str, u32) -> Option<String>,
) -> (String, BTreeMap<u64, u32>) {
    let mut text = String::new();
    let mut lines = BTreeMap::new();
    let mut line_no = 0u32;
    let mut push = |text: &mut String, s: &str| {
        text.push_str(s);
        text.push('\n');
        line_no += 1;
        line_no
    };
    let (start, end) = match (insns.first(), insns.last()) {
        (Some(a), Some(b)) => (a.address, b.address),
        _ => (0, 0),
    };
    push(&mut text, &format!("; Haltline disassembly: {title}"));
    push(&mut text, &format!("; Range: 0x{start:08x} .. 0x{end:08x}"));
    push(
        &mut text,
        "; Generated from the ELF file. Set breakpoints on instruction lines.",
    );
    push(&mut text, ";");
    let mut last_source: Option<(String, u32)> = None;
    for insn in insns {
        if let Some(src) = &insn.source {
            let key = (
                src.fullname.clone().unwrap_or_else(|| src.file.clone()),
                src.line,
            );
            if last_source.as_ref() != Some(&key) {
                let name = Path::new(&src.file)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| src.file.clone());
                let code = source_text(&key.0, src.line).unwrap_or_default();
                let comment = format!("; {name}:{}  {}", src.line, code.trim_end());
                push(&mut text, comment.trim_end());
                last_source = Some(key);
            }
        }
        let symbol = insn.symbol().map(|s| format!("<{s}>")).unwrap_or_default();
        let line = format!(
            "  0x{:08x} {:<20} {:<12} {}",
            insn.address,
            symbol,
            insn.opcodes,
            insn.pretty_text()
        );
        let n = push(&mut text, line.trim_end());
        lines.insert(insn.address, n);
    }
    (text, lines)
}

/// Reads the line → address mapping back from a rendered file.
pub fn parse_asm_text(text: &str) -> BTreeMap<u32, u64> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let rest = line.trim_start().strip_prefix("0x")?;
            let end = rest
                .find(|c: char| !c.is_ascii_hexdigit())
                .unwrap_or(rest.len());
            let address = u64::from_str_radix(&rest[..end], 16).ok()?;
            Some((i as u32 + 1, address))
        })
        .collect()
}

fn source_line(
    sources: &mut HashMap<String, Option<Vec<String>>>,
    fullname: &str,
    line: u32,
) -> Option<String> {
    let lines = sources.entry(fullname.to_string()).or_insert_with(|| {
        std::fs::read(fullname).ok().map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(str::to_string)
                .collect()
        })
    });
    lines.as_ref()?.get(line.checked_sub(1)? as usize).cloned()
}

/// Rendered files of one debug session, kept in a per-ELF cache directory.
pub struct DisasmCache {
    dir: PathBuf,
    files: Vec<AsmFile>,
    sources: HashMap<String, Option<Vec<String>>>,
}

impl DisasmCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            files: Vec::new(),
            sources: HashMap::new(),
        }
    }

    /// Cache directory for an ELF: `<tmp>/haltline/<stem>-<hash>`, where the
    /// hash covers the path, size and modification time of the file.
    pub fn dir_for_elf(base: &Path, elf: Option<&Path>) -> PathBuf {
        let (stem, key) = match elf {
            Some(p) => {
                let meta = std::fs::metadata(p).ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                (
                    p.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "elf".into()),
                    format!("{}|{size}|{mtime}", p.display()),
                )
            }
            None => ("noelf".into(), String::new()),
        };
        base.join("haltline").join(format!(
            "{}-{:016x}",
            sanitize_file_name(&stem),
            fnv1a(key.as_bytes())
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `path` is a rendered disassembly file (of this or an earlier session).
    pub fn is_asm_path(&self, path: &Path) -> bool {
        path.starts_with(&self.dir)
            || (path.extension().is_some_and(|e| e == "s")
                && path
                    .parent()
                    .and_then(Path::parent)
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == "haltline")))
    }

    pub fn find(&self, address: u64) -> Option<&AsmFile> {
        self.files.iter().find(|f| f.contains(address))
    }

    /// Renders `insns` into a file (reusing an existing one with the same name).
    pub fn insert(&mut self, title: &str, insns: &[Instruction]) -> io::Result<AsmFile> {
        let Some(first) = insns.first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no instructions",
            ));
        };
        let start = first.address;
        let last = insns.last().unwrap();
        // Thumb instructions are 2 or 4 bytes long.
        let end = last.address
            + if last.opcodes.replace(' ', "").len() >= 8 {
                4
            } else {
                2
            };
        let sources = &mut self.sources;
        let (text, lines) = render(title, insns, |name, line| source_line(sources, name, line));
        std::fs::create_dir_all(&self.dir)?;
        let path = self
            .dir
            .join(format!("{}@{start:08x}.s", sanitize_file_name(title)));
        let unchanged = std::fs::read_to_string(&path).is_ok_and(|old| old == text);
        if !unchanged {
            std::fs::write(&path, &text)?;
        }
        self.files.retain(|f| f.path != path);
        let file = AsmFile {
            path,
            start,
            end,
            lines,
        };
        self.files.push(file.clone());
        Ok(file)
    }

    /// Maps a breakpoint line in a rendered file to an instruction address.
    /// Non-instruction lines snap to the next instruction.
    pub fn address_for_line(&self, path: &Path, line: u32) -> Option<(u64, u32)> {
        let map: BTreeMap<u32, u64> = match self.files.iter().find(|f| f.path == path) {
            Some(f) => f.lines.iter().map(|(a, l)| (*l, *a)).collect(),
            None => parse_asm_text(&std::fs::read_to_string(path).ok()?),
        };
        map.range(line..).next().map(|(l, a)| (*a, *l))
    }

    pub fn line_for_address(&self, path: &Path, address: u64) -> Option<u32> {
        if let Some(f) = self.files.iter().find(|f| f.path == path) {
            return f.lines.get(&address).copied();
        }
        parse_asm_text(&std::fs::read_to_string(path).ok()?)
            .into_iter()
            .find(|(_, a)| *a == address)
            .map(|(l, _)| l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::{parse_line, Record};

    fn body(line: &str) -> Value {
        match parse_line(line) {
            Record::Result { body, .. } => body,
            other => panic!("{other:?}"),
        }
    }

    const MIXED: &str = r#"^done,asm_insns=[src_and_asm_line={line="3",file="main.c",fullname="/fw/main.c",line_asm_insn=[{address="0x08000100",func-name="main",offset="0",opcodes="b580",inst="push\t{r7, lr}"},{address="0x08000102",func-name="main",offset="2",opcodes="af00",inst="add\tr7, sp, #0"}]},src_and_asm_line={line="4",file="main.c",fullname="/fw/main.c",line_asm_insn=[{address="0x08000104",func-name="main",offset="4",opcodes="f000 f802",inst="bl\t0x800010c <foo>"}]}]"#;

    #[test]
    fn parses_mixed_and_flat_output() {
        let insns = parse_disassembly(&body(MIXED));
        assert_eq!(insns.len(), 3);
        assert_eq!(insns[0].address, 0x0800_0100);
        assert_eq!(insns[0].source.as_ref().unwrap().line, 3);
        assert_eq!(insns[2].opcodes, "f000 f802");
        assert_eq!(insns[2].symbol().as_deref(), Some("main+4"));
        assert_eq!(insns[0].pretty_text(), "push    {r7, lr}");

        let flat =
            r#"^done,asm_insns=[{address="0x2",inst="nop"},{address="0x0",inst="movs\tr0, #1"}]"#;
        let insns = parse_disassembly(&body(flat));
        assert_eq!(insns.iter().map(|i| i.address).collect::<Vec<_>>(), [0, 2]);
        assert!(insns[0].source.is_none());
    }

    #[test]
    fn renders_and_reparses_files() {
        let insns = parse_disassembly(&body(MIXED));
        let (text, lines) = render("main", &insns, |name, line| {
            assert_eq!(name, "/fw/main.c");
            Some(format!("source line {line}"))
        });
        let parsed = parse_asm_text(&text);
        assert_eq!(parsed.len(), 3);
        for (addr, line) in &lines {
            assert_eq!(parsed.get(line), Some(addr));
        }
        let text_lines: Vec<&str> = text.lines().collect();
        assert!(text_lines[*lines.get(&0x0800_0100).unwrap() as usize - 2]
            .starts_with("; main.c:3  source line 3"));
        assert!(text.contains("0x08000104 <main+4>"));
        assert!(text.contains("bl      0x800010c <foo>"));
    }

    #[test]
    fn cache_writes_files_and_maps_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DisasmCache::dir_for_elf(tmp.path(), None);
        let mut cache = DisasmCache::new(dir.clone());
        let insns = parse_disassembly(&body(MIXED));
        let file = cache.insert("main", &insns).unwrap();
        assert!(file.path.starts_with(&dir));
        assert!(file.path.exists());
        assert!(cache.is_asm_path(&file.path));
        assert_eq!(file.end, 0x0800_0108);
        assert!(cache.find(0x0800_0106).is_some());
        assert!(cache.find(0x0800_0108).is_none());
        let l = file.line_of(0x0800_0102).unwrap();
        assert_eq!(
            cache.address_for_line(&file.path, l),
            Some((0x0800_0102, l))
        );
        // A comment line snaps to the following instruction.
        let (addr, snapped) = cache.address_for_line(&file.path, 1).unwrap();
        assert_eq!(addr, 0x0800_0100);
        assert!(snapped > 1);
        assert_eq!(
            cache.line_for_address(&file.path, 0x0800_0104),
            file.lines.get(&0x0800_0104).copied()
        );

        // A fresh cache (new session) still maps lines by reading the file.
        let fresh = DisasmCache::new(dir);
        assert_eq!(
            fresh.address_for_line(&file.path, l),
            Some((0x0800_0102, l))
        );
    }

    #[test]
    fn cache_dir_depends_on_elf_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("fw.elf");
        std::fs::write(&elf, b"one").unwrap();
        let a = DisasmCache::dir_for_elf(tmp.path(), Some(&elf));
        std::fs::write(&elf, b"three").unwrap();
        let b = DisasmCache::dir_for_elf(tmp.path(), Some(&elf));
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("fw-"));
    }
}
