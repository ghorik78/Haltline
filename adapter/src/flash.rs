//! Flashing progress for the debug console.
//!
//! Zed ignores DAP progress events, so progress is printed as console lines.
//! GDB reports little while it loads an image: nothing while the server
//! erases the flash, `+download` records (at most every 0.5 s) while it sends
//! the data, and nothing while the server writes the flash at the end
//! (OpenOCD, J-Link, st-util and pyOCD buffer the data until then). The
//! stages are inferred from these records and from silence between them.

use crate::mi::Value as Mi;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Erasing,
    Writing,
    Programming,
    Verifying,
    /// A `load` with arguments: GDB reports no progress for it.
    Loading,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Erasing => "erasing flash",
            Stage::Writing => "writing",
            Stage::Programming => "programming flash",
            Stage::Verifying => "verifying",
            Stage::Loading => "loading",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Stage::Erasing => "erase",
            Stage::Writing => "write",
            Stage::Programming => "program",
            Stage::Verifying => "verify",
            Stage::Loading => "load",
        }
    }
}

/// A stage is announced only once it lasts this long, so stages that do not
/// happen on a server (no erase on QEMU) produce no output.
const ANNOUNCE_AFTER: Duration = Duration::from_millis(300);
/// While a stage lasts, its elapsed time is repeated this often.
const HEARTBEAT: Duration = Duration::from_secs(3);
/// No `+download` record for this long after the last section started means
/// GDB has sent everything and waits for the server to program the flash.
const TRANSFER_IDLE: Duration = Duration::from_millis(1500);
/// Minimum spacing of percentage lines.
const PERCENT_EVERY: Duration = Duration::from_secs(1);
/// Shorter stages are left out of the summary.
const SUMMARY_MIN: Duration = Duration::from_millis(50);

const PREFIX: &str = "Flashing: ";

pub struct FlashProgress {
    has_flash: bool,
    /// Bytes GDB downloads (from the ELF); 0 when unknown.
    total: u64,
    started: Instant,
    stage: Stage,
    stage_started: Instant,
    announced: bool,
    last_line: Instant,
    last_record: Instant,
    /// Bytes sent so far, as reported by GDB.
    sent: u64,
    /// Total size of the sections whose transfer has started.
    started_bytes: u64,
    reported_percent: Option<u64>,
    durations: Vec<(Stage, Duration)>,
}

impl FlashProgress {
    /// Progress of `-target-download`. `total` is the number of bytes to
    /// load (0 if unknown); `has_flash` is false for targets without flash
    /// (QEMU), where nothing is erased or programmed.
    pub fn new(total: u64, has_flash: bool, now: Instant) -> Self {
        let stage = if has_flash {
            Stage::Erasing
        } else {
            Stage::Writing
        };
        Self::with_stage(stage, total, has_flash, now)
    }

    /// Progress of a command that reports nothing until it is done.
    pub fn untracked(has_flash: bool, now: Instant) -> Self {
        Self::with_stage(Stage::Loading, 0, has_flash, now)
    }

    fn with_stage(stage: Stage, total: u64, has_flash: bool, now: Instant) -> Self {
        Self {
            has_flash,
            total,
            started: now,
            stage,
            stage_started: now,
            announced: false,
            last_line: now,
            last_record: now,
            sent: 0,
            started_bytes: 0,
            reported_percent: None,
            durations: Vec::new(),
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    fn enter(&mut self, stage: Stage, at: Instant) {
        if stage == self.stage {
            return;
        }
        self.close_stage(at);
        self.stage = stage;
        self.stage_started = at;
        self.announced = false;
    }

    fn close_stage(&mut self, at: Instant) {
        let took = at.saturating_duration_since(self.stage_started);
        self.durations.push((self.stage, took));
        self.stage_started = at;
    }

    fn percent(&self) -> Option<u64> {
        (self.total > 0).then(|| (self.sent * 100 / self.total).min(100))
    }

    fn line(&mut self, text: String, now: Instant) -> Vec<String> {
        self.last_line = now;
        vec![format!("{PREFIX}{text}")]
    }

    /// Handles a `+download` status record.
    pub fn on_download(&mut self, body: &Mi, now: Instant) -> Vec<String> {
        // GDB prints the fields as an unnamed tuple: `+download,{section=...}`.
        let record = match body.get("") {
            Some(inner @ Mi::Tuple(_)) => inner,
            _ => body,
        };
        let num = |key: &str| record.str(key).and_then(|v| v.parse::<u64>().ok());
        self.last_record = now;
        if self.stage == Stage::Erasing {
            self.enter(Stage::Writing, now);
        }
        match num("total-sent") {
            Some(sent) => self.sent = self.sent.max(sent),
            // The first record of a section: its transfer is starting.
            None => self.started_bytes += num("section-size").unwrap_or(0),
        }
        if self.stage != Stage::Writing {
            return Vec::new();
        }
        if self.has_flash && self.total > 0 && self.sent >= self.total {
            self.enter(Stage::Programming, now);
            return Vec::new();
        }
        match self.percent() {
            Some(p)
                if self.announced
                    && self.reported_percent != Some(p)
                    && now.saturating_duration_since(self.last_line) >= PERCENT_EVERY =>
            {
                self.reported_percent = Some(p);
                self.line(format!("writing {p}%"), now)
            }
            _ => Vec::new(),
        }
    }

    /// Called regularly while the command runs: announces stages that last
    /// and repeats their elapsed time.
    pub fn tick(&mut self, now: Instant) -> Vec<String> {
        if self.stage == Stage::Writing
            && self.has_flash
            && (self.total == 0 || self.started_bytes >= self.total)
            && now.saturating_duration_since(self.last_record) >= TRANSFER_IDLE
        {
            let at = self.last_record;
            self.enter(Stage::Programming, at);
        }
        let elapsed = now.saturating_duration_since(self.stage_started);
        if !self.announced {
            if elapsed < ANNOUNCE_AFTER {
                return Vec::new();
            }
            self.announced = true;
            let text = match (self.stage, self.total) {
                (Stage::Writing, total) if total > 0 => {
                    let percent = self.percent().unwrap_or(0);
                    self.reported_percent = Some(percent);
                    format!("writing {}... {percent}%", format_size(total))
                }
                (stage, _) => format!("{}...", stage.label()),
            };
            return self.line(text, now);
        }
        if now.saturating_duration_since(self.last_line) < HEARTBEAT {
            return Vec::new();
        }
        let text = match self.percent() {
            Some(p) if self.stage == Stage::Writing => {
                self.reported_percent = Some(p);
                format!("writing {p}%, {}", format_secs(elapsed))
            }
            _ => format!("{}... {}", self.stage.label(), format_secs(elapsed)),
        };
        self.line(text, now)
    }

    /// The load command finished. `body` is its result (`load-size`).
    pub fn load_done(&mut self, body: &Mi, now: Instant) {
        if let Some(size) = body.str("load-size").and_then(|v| v.parse().ok()) {
            self.total = size;
        }
        self.close_stage(now);
    }

    pub fn begin_verify(&mut self, now: Instant) {
        self.stage = Stage::Verifying;
        self.stage_started = now;
        self.announced = false;
    }

    pub fn verify_done(&mut self, now: Instant) {
        self.close_stage(now);
    }

    /// The final line, e.g. `Flashed 12.3 KiB in 4.1 s (erase 1.0 s, ...)`.
    pub fn summary(&self, now: Instant) -> String {
        let verified = self.durations.iter().any(|(s, _)| *s == Stage::Verifying);
        let verb = match (self.has_flash, verified) {
            (true, true) => "Flashed and verified",
            (true, false) => "Flashed",
            (false, true) => "Loaded and verified",
            (false, false) => "Loaded",
        };
        let size = if self.total > 0 {
            format!(" {}", format_size(self.total))
        } else {
            String::new()
        };
        let mut parts: Vec<(Stage, Duration)> = Vec::new();
        for (stage, took) in &self.durations {
            match parts.iter_mut().find(|(s, _)| s == stage) {
                Some((_, sum)) => *sum += *took,
                None => parts.push((*stage, *took)),
            }
        }
        let parts: Vec<String> = parts
            .iter()
            .filter(|(_, took)| *took >= SUMMARY_MIN)
            .map(|(stage, took)| format!("{} {}", stage.short(), format_secs(*took)))
            .collect();
        let total = format_secs(now.saturating_duration_since(self.started));
        if parts.len() > 1 {
            format!("{verb}{size} in {total} ({})", parts.join(", "))
        } else {
            format!("{verb}{size} in {total}")
        }
    }
}

fn format_secs(d: Duration) -> String {
    format!("{:.1} s", d.as_secs_f64())
}

pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{:.2} MiB", b / (KIB * KIB))
    }
}

/// Bytes `load` writes for this ELF file: the allocated sections that have
/// contents (what GDB/BFD calls `SEC_LOAD`). `None` if it is not an ELF.
pub fn loadable_size(path: &Path) -> Option<u64> {
    elf_loadable_size(&std::fs::read(path).ok()?)
}

fn elf_loadable_size(data: &[u8]) -> Option<u64> {
    if data.get(..4)? != b"\x7fELF" {
        return None;
    }
    let is64 = match data.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let le = match data.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let read = |off: usize, len: usize| -> Option<u64> {
        let bytes = data.get(off..off.checked_add(len)?)?;
        let mut v = 0u64;
        for i in 0..len {
            let b = if le { bytes[len - 1 - i] } else { bytes[i] };
            v = (v << 8) | u64::from(b);
        }
        Some(v)
    };
    let (shoff, shentsize, shnum) = if is64 {
        (read(0x28, 8)?, read(0x3a, 2)?, read(0x3c, 2)?)
    } else {
        (read(0x20, 4)?, read(0x2e, 2)?, read(0x30, 2)?)
    };
    const SHF_ALLOC: u64 = 0x2;
    const SHT_NOBITS: u64 = 8;
    let mut total = 0u64;
    for i in 0..shnum {
        let base = usize::try_from(shoff + i * shentsize).ok()?;
        let kind = read(base + 4, 4)?;
        let (flags, size) = if is64 {
            (read(base + 8, 8)?, read(base + 32, 8)?)
        } else {
            (read(base + 8, 4)?, read(base + 20, 4)?)
        };
        if flags & SHF_ALLOC != 0 && kind != SHT_NOBITS {
            total += size;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::{parse_line, Record};

    fn download(line: &str) -> Mi {
        match parse_line(line) {
            Record::Async { body, .. } => body,
            other => panic!("not an async record: {other:?}"),
        }
    }

    fn done(line: &str) -> Mi {
        match parse_line(line) {
            Record::Result { body, .. } => body,
            other => panic!("not a result record: {other:?}"),
        }
    }

    fn start(section: &str, size: u64, gdb_total: u64) -> Mi {
        download(&format!(
            "3+download,{{section=\"{section}\",section-size=\"{size}\",total-size=\"{gdb_total}\"}}"
        ))
    }

    fn sent(section: &str, size: u64, total_sent: u64, gdb_total: u64) -> Mi {
        download(&format!(
            "3+download,{{section=\"{section}\",section-sent=\"{size}\",section-size=\"{size}\",total-sent=\"{total_sent}\",total-size=\"{gdb_total}\"}}"
        ))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// OpenOCD: a long erase, a quick transfer into its buffer, then a long
    /// flash write after GDB has sent everything.
    #[test]
    fn reports_erase_transfer_and_programming() {
        let t0 = Instant::now();
        let mut p = FlashProgress::new(4096, true, t0);
        assert!(p.tick(t0 + ms(100)).is_empty());
        assert_eq!(p.tick(t0 + ms(400)), ["Flashing: erasing flash..."]);
        assert!(p.tick(t0 + ms(2000)).is_empty());
        assert_eq!(p.tick(t0 + ms(3500)), ["Flashing: erasing flash... 3.5 s"]);

        // GDB's total-size counts debug sections too; it is ignored.
        assert!(p
            .on_download(&start(".text", 4096, 99999), t0 + ms(4000))
            .is_empty());
        assert_eq!(p.stage(), Stage::Writing);
        assert!(p.tick(t0 + ms(4100)).is_empty());
        assert_eq!(p.tick(t0 + ms(4300)), ["Flashing: writing 4.0 KiB... 0%"]);
        assert_eq!(
            p.on_download(&sent(".text", 1024, 1024, 99999), t0 + ms(5400)),
            ["Flashing: writing 25%"]
        );
        // Silence after the last section started: the server is programming.
        assert!(p.tick(t0 + ms(6000)).is_empty());
        assert_eq!(p.tick(t0 + ms(6950)), ["Flashing: programming flash..."]);
        assert_eq!(p.stage(), Stage::Programming);
        assert_eq!(
            p.tick(t0 + ms(10000)),
            ["Flashing: programming flash... 4.6 s"]
        );

        let done = done("3^done,address=\"0x08000040\",load-size=\"4096\"");
        p.load_done(&done, t0 + ms(11000));
        p.begin_verify(t0 + ms(11000));
        p.verify_done(t0 + ms(11500));
        assert_eq!(
            p.summary(t0 + ms(11500)),
            "Flashed and verified 4.0 KiB in 11.5 s (erase 4.0 s, write 1.4 s, program 5.6 s, verify 0.5 s)"
        );
    }

    #[test]
    fn complete_transfer_switches_to_programming_at_once() {
        let t0 = Instant::now();
        let mut p = FlashProgress::new(100, true, t0);
        p.on_download(&start(".text", 100, 500), t0);
        p.on_download(&sent(".text", 100, 100, 500), t0 + ms(10));
        assert_eq!(p.stage(), Stage::Programming);
    }

    /// QEMU has no flash: no erase or programming stage, and a quick load
    /// prints nothing but the summary.
    #[test]
    fn quick_load_without_flash() {
        let t0 = Instant::now();
        let mut p = FlashProgress::new(996, false, t0);
        assert_eq!(p.stage(), Stage::Writing);
        p.on_download(&start(".isr_vector", 64, 23660), t0 + ms(1));
        p.on_download(&sent(".isr_vector", 64, 64, 23660), t0 + ms(2));
        p.on_download(&start(".text", 932, 23660), t0 + ms(3));
        assert!(p.tick(t0 + ms(5)).is_empty());
        assert_eq!(p.stage(), Stage::Writing);
        let done = done("3^done,address=\"0x08000040\",load-size=\"996\"");
        p.load_done(&done, t0 + ms(20));
        assert_eq!(p.summary(t0 + ms(20)), "Loaded 996 B in 0.0 s");
    }

    #[test]
    fn untracked_loads_only_show_elapsed_time() {
        let t0 = Instant::now();
        let mut p = FlashProgress::untracked(true, t0);
        assert_eq!(p.tick(t0 + ms(500)), ["Flashing: loading..."]);
        assert_eq!(p.tick(t0 + ms(3600)), ["Flashing: loading... 3.6 s"]);
        p.load_done(&Mi::Tuple(Vec::new()), t0 + ms(4000));
        assert_eq!(p.summary(t0 + ms(4000)), "Flashed in 4.0 s");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(996), "996 B");
        assert_eq!(format_size(12_595), "12.3 KiB");
        assert_eq!(format_size(3 * 1024 * 1024 / 2), "1.50 MiB");
    }

    /// A minimal ELF32 with a .text (alloc, progbits), a .bss (alloc, nobits)
    /// and a debug section (not alloc).
    fn elf32(big_endian: bool) -> Vec<u8> {
        let put = |buf: &mut Vec<u8>, off: usize, len: usize, v: u64| {
            for i in 0..len {
                let shift = if big_endian { (len - 1 - i) * 8 } else { i * 8 };
                buf[off + i] = (v >> shift) as u8;
            }
        };
        let shoff = 0x40;
        let mut buf = vec![0u8; shoff + 4 * 40];
        buf[..4].copy_from_slice(b"\x7fELF");
        buf[4] = 1;
        buf[5] = if big_endian { 2 } else { 1 };
        put(&mut buf, 0x20, 4, shoff as u64);
        put(&mut buf, 0x2e, 2, 40);
        put(&mut buf, 0x30, 2, 4);
        // (type, flags, size); section 0 is the null section.
        let sections = [(1u64, 0x6u64, 920u64), (8, 0x3, 4096), (1, 0x0, 5000)];
        for (i, (kind, flags, size)) in sections.iter().enumerate() {
            let base = shoff + (i + 1) * 40;
            put(&mut buf, base + 4, 4, *kind);
            put(&mut buf, base + 8, 4, *flags);
            put(&mut buf, base + 20, 4, *size);
        }
        buf
    }

    #[test]
    fn counts_loadable_elf_sections() {
        assert_eq!(elf_loadable_size(&elf32(false)), Some(920));
        assert_eq!(elf_loadable_size(&elf32(true)), Some(920));
        assert_eq!(elf_loadable_size(b"\x7fELF"), None);
        assert_eq!(elf_loadable_size(b"not an elf"), None);
        let mut truncated = elf32(false);
        truncated.truncate(0x60);
        assert_eq!(elf_loadable_size(&truncated), None);
    }
}
