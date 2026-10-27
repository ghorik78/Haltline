//! ARM Cortex-M knowledge: register groups, bit-field decoders for special
//! registers and the System Control Block fault status registers.

/// Register groups shown in the "Registers" scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Core,
    System,
    Fpu,
    Other,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Core => "Core",
            Group::System => "System",
            Group::Fpu => "FPU",
            Group::Other => "Other",
        }
    }
}

pub fn classify(name: &str) -> Group {
    let n = name.to_ascii_lowercase();
    let numbered = |prefix: &str, max: u32| {
        n.strip_prefix(prefix)
            .and_then(|rest| rest.parse::<u32>().ok())
            .is_some_and(|i| i <= max)
    };
    if numbered("r", 12) || matches!(n.as_str(), "sp" | "lr" | "pc" | "xpsr" | "cpsr") {
        return Group::Core;
    }
    const SYSTEM: [&str; 9] = [
        "msp",
        "psp",
        "primask",
        "basepri",
        "faultmask",
        "control",
        "msplim",
        "psplim",
        "sp_",
    ];
    if SYSTEM.iter().any(|p| n.starts_with(p)) {
        return Group::System;
    }
    if numbered("s", 31)
        || numbered("d", 31)
        || numbered("q", 15)
        || matches!(n.as_str(), "fpscr" | "fpsid" | "fpexc" | "vpr" | "p0")
    {
        return Group::Fpu;
    }
    Group::Other
}

/// Whether GDB should print the register in its natural format (floating point)
/// rather than hex.
pub fn is_float_register(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.starts_with('s') || n.starts_with('d')) && n[1..].parse::<u32>().is_ok()
}

#[derive(Debug, Clone, Copy)]
pub struct BitField {
    pub name: &'static str,
    pub lsb: u32,
    pub width: u32,
    pub description: &'static str,
}

const fn bf(name: &'static str, lsb: u32, width: u32, description: &'static str) -> BitField {
    BitField {
        name,
        lsb,
        width,
        description,
    }
}

pub const XPSR: &[BitField] = &[
    bf("N", 31, 1, "Negative"),
    bf("Z", 30, 1, "Zero"),
    bf("C", 29, 1, "Carry"),
    bf("V", 28, 1, "Overflow"),
    bf("Q", 27, 1, "Saturation"),
    bf("T", 24, 1, "Thumb state"),
    bf("GE", 16, 4, "Greater than or equal (DSP)"),
    bf("ISR", 0, 9, "Active exception number"),
];

pub const CONTROL: &[BitField] = &[
    bf("FPCA", 2, 1, "FP context active"),
    bf("SPSEL", 1, 1, "0 = MSP, 1 = PSP in thread mode"),
    bf(
        "nPRIV",
        0,
        1,
        "0 = privileged, 1 = unprivileged thread mode",
    ),
];

pub const PRIMASK: &[BitField] = &[bf("PM", 0, 1, "1 = configurable interrupts masked")];
pub const FAULTMASK: &[BitField] = &[bf("FM", 0, 1, "1 = all exceptions except NMI masked")];
pub const BASEPRI: &[BitField] = &[bf("BASEPRI", 0, 8, "Priority masking threshold")];

pub const FPSCR: &[BitField] = &[
    bf("N", 31, 1, "Negative"),
    bf("Z", 30, 1, "Zero"),
    bf("C", 29, 1, "Carry"),
    bf("V", 28, 1, "Overflow"),
    bf("AHP", 26, 1, "Alternative half-precision"),
    bf("DN", 25, 1, "Default NaN"),
    bf("FZ", 24, 1, "Flush-to-zero"),
    bf("RMode", 22, 2, "Rounding mode"),
    bf("IDC", 7, 1, "Input denormal"),
    bf("IXC", 4, 1, "Inexact"),
    bf("UFC", 3, 1, "Underflow"),
    bf("OFC", 2, 1, "Overflow"),
    bf("DZC", 1, 1, "Division by zero"),
    bf("IOC", 0, 1, "Invalid operation"),
];

/// Bit fields used to decode a special register, if it has any.
pub fn fields_for(register: &str) -> Option<&'static [BitField]> {
    Some(match register.to_ascii_lowercase().as_str() {
        "xpsr" | "cpsr" => XPSR,
        "control" | "control_s" | "control_ns" => CONTROL,
        "primask" | "primask_s" | "primask_ns" => PRIMASK,
        "faultmask" | "faultmask_s" | "faultmask_ns" => FAULTMASK,
        "basepri" | "basepri_s" | "basepri_ns" => BASEPRI,
        "fpscr" => FPSCR,
        _ => return None,
    })
}

impl BitField {
    pub fn extract(&self, value: u64) -> u64 {
        (value >> self.lsb) & crate::util::mask(self.width)
    }
}

/// Name of a Cortex-M exception number (as found in xPSR.ISR / ICSR.VECTACTIVE).
pub fn exception_name(number: u64) -> String {
    match number {
        0 => "Thread mode".into(),
        1 => "Reset".into(),
        2 => "NMI".into(),
        3 => "HardFault".into(),
        4 => "MemManage".into(),
        5 => "BusFault".into(),
        6 => "UsageFault".into(),
        7 => "SecureFault".into(),
        11 => "SVCall".into(),
        12 => "DebugMonitor".into(),
        14 => "PendSV".into(),
        15 => "SysTick".into(),
        n if n >= 16 => format!("IRQ {}", n - 16),
        n => format!("Reserved ({n})"),
    }
}

/// Human readable value of a decoded bit field.
pub fn format_field(register: &str, field: &BitField, value: u64) -> String {
    if field.name == "ISR" && register.eq_ignore_ascii_case("xpsr") {
        return format!("{value} ({})", exception_name(value));
    }
    if field.width == 1 {
        value.to_string()
    } else {
        crate::util::hex(value, field.width)
    }
}

/// System Control Block fault registers.
pub const SCB_CFSR: u64 = 0xE000_ED28;
pub const SCB_HFSR: u64 = 0xE000_ED2C;
pub const SCB_MMFAR: u64 = 0xE000_ED34;
pub const SCB_BFAR: u64 = 0xE000_ED38;
/// Bytes from CFSR to the end of BFAR.
pub const FAULT_BLOCK_LEN: u64 = 0x14;

pub const CFSR: &[BitField] = &[
    bf("IACCVIOL", 0, 1, "MemManage: instruction access violation"),
    bf("DACCVIOL", 1, 1, "MemManage: data access violation"),
    bf(
        "MUNSTKERR",
        3,
        1,
        "MemManage: fault on exception return unstacking",
    ),
    bf(
        "MSTKERR",
        4,
        1,
        "MemManage: fault on exception entry stacking",
    ),
    bf(
        "MLSPERR",
        5,
        1,
        "MemManage: fault during FP lazy state preservation",
    ),
    bf("MMARVALID", 7, 1, "MMFAR holds the faulting address"),
    bf("IBUSERR", 8, 1, "BusFault: instruction bus error"),
    bf("PRECISERR", 9, 1, "BusFault: precise data bus error"),
    bf("IMPRECISERR", 10, 1, "BusFault: imprecise data bus error"),
    bf(
        "UNSTKERR",
        11,
        1,
        "BusFault: fault on exception return unstacking",
    ),
    bf(
        "STKERR",
        12,
        1,
        "BusFault: fault on exception entry stacking",
    ),
    bf(
        "LSPERR",
        13,
        1,
        "BusFault: fault during FP lazy state preservation",
    ),
    bf("BFARVALID", 15, 1, "BFAR holds the faulting address"),
    bf("UNDEFINSTR", 16, 1, "UsageFault: undefined instruction"),
    bf(
        "INVSTATE",
        17,
        1,
        "UsageFault: invalid state (Thumb bit cleared?)",
    ),
    bf(
        "INVPC",
        18,
        1,
        "UsageFault: invalid PC load (bad EXC_RETURN)",
    ),
    bf(
        "NOCP",
        19,
        1,
        "UsageFault: coprocessor access (FPU not enabled?)",
    ),
    bf("STKOF", 20, 1, "UsageFault: stack overflow (ARMv8-M)"),
    bf("UNALIGNED", 24, 1, "UsageFault: unaligned access"),
    bf("DIVBYZERO", 25, 1, "UsageFault: division by zero"),
];

pub const HFSR: &[BitField] = &[
    bf("VECTTBL", 1, 1, "Bus fault on vector table read"),
    bf("FORCED", 30, 1, "Escalated configurable fault (see CFSR)"),
    bf("DEBUGEVT", 31, 1, "Debug event"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FaultStatus {
    pub cfsr: u64,
    pub hfsr: u64,
    pub mmfar: u64,
    pub bfar: u64,
}

impl FaultStatus {
    /// Builds the status from the 20 bytes starting at [`SCB_CFSR`].
    pub fn from_block(bytes: &[u8]) -> Option<Self> {
        if (bytes.len() as u64) < FAULT_BLOCK_LEN {
            return None;
        }
        let word = |off: usize| crate::util::le_to_u64(&bytes[off..off + 4]);
        Some(Self {
            cfsr: word(0),
            hfsr: word(4),
            mmfar: word(12),
            bfar: word(16),
        })
    }

    pub fn active(fields: &'static [BitField], value: u64) -> Vec<&'static BitField> {
        fields.iter().filter(|f| f.extract(value) != 0).collect()
    }

    pub fn is_fault(&self) -> bool {
        self.cfsr != 0 || self.hfsr & ((1 << 1) | (1 << 30)) != 0
    }

    /// Multi-line explanation of the current fault state.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "CFSR = {}  HFSR = {}",
            crate::util::hex(self.cfsr, 32),
            crate::util::hex(self.hfsr, 32)
        )];
        for f in Self::active(HFSR, self.hfsr) {
            lines.push(format!("  HFSR.{}: {}", f.name, f.description));
        }
        for f in Self::active(CFSR, self.cfsr) {
            lines.push(format!("  CFSR.{}: {}", f.name, f.description));
        }
        if self.cfsr & (1 << 7) != 0 {
            lines.push(format!(
                "  Faulting data address (MMFAR): {}",
                crate::util::hex(self.mmfar, 32)
            ));
        }
        if self.cfsr & (1 << 15) != 0 {
            lines.push(format!(
                "  Faulting data address (BFAR): {}",
                crate::util::hex(self.bfar, 32)
            ));
        }
        if !self.is_fault() {
            lines.push("  No fault flags are set.".into());
        }
        lines
    }

    /// Short summary like `PRECISERR, BFARVALID`.
    pub fn summary(fields: &'static [BitField], value: u64) -> String {
        let names: Vec<_> = Self::active(fields, value).iter().map(|f| f.name).collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(" [{}]", names.join(", "))
        }
    }
}

/// Registers stacked by the hardware on exception entry, in memory order.
pub const STACKED: [&str; 8] = ["r0", "r1", "r2", "r3", "r12", "lr", "pc", "xpsr"];

/// Fault handler symbols used by the "Faults" exception breakpoint filter.
pub const FAULT_HANDLERS: [&str; 5] = [
    "HardFault_Handler",
    "MemManage_Handler",
    "BusFault_Handler",
    "UsageFault_Handler",
    "SecureFault_Handler",
];

/// For an EXC_RETURN value in LR: `Some(true)` if the exception frame is on the
/// process stack (PSP), `Some(false)` for MSP, `None` if LR is not EXC_RETURN.
pub fn exc_return_uses_psp(lr: u64) -> Option<bool> {
    if lr & 0xFF00_0000 != 0xFF00_0000 {
        return None;
    }
    Some(lr & (1 << 2) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_registers() {
        for r in ["r0", "R12", "sp", "lr", "pc", "xPSR"] {
            assert_eq!(classify(r), Group::Core, "{r}");
        }
        for r in [
            "msp",
            "psp",
            "primask",
            "basepri",
            "faultmask",
            "control",
            "msp_ns",
        ] {
            assert_eq!(classify(r), Group::System, "{r}");
        }
        for r in ["s0", "s31", "d15", "fpscr"] {
            assert_eq!(classify(r), Group::Fpu, "{r}");
        }
        for r in ["r13", "s32", "foo", "sys"] {
            assert_eq!(classify(r), Group::Other, "{r}");
        }
        assert!(is_float_register("s3"));
        assert!(is_float_register("d0"));
        assert!(!is_float_register("sp"));
        assert!(!is_float_register("fpscr"));
    }

    #[test]
    fn decodes_xpsr() {
        let xpsr = 0x6100_0003u64; // Z, C, T, HardFault
        let fields = fields_for("xPSR").unwrap();
        let get = |n: &str| fields.iter().find(|f| f.name == n).unwrap().extract(xpsr);
        assert_eq!(get("N"), 0);
        assert_eq!(get("Z"), 1);
        assert_eq!(get("C"), 1);
        assert_eq!(get("T"), 1);
        assert_eq!(get("ISR"), 3);
        let isr = fields.iter().find(|f| f.name == "ISR").unwrap();
        assert_eq!(format_field("xpsr", isr, 3), "3 (HardFault)");
    }

    #[test]
    fn names_exceptions() {
        assert_eq!(exception_name(0), "Thread mode");
        assert_eq!(exception_name(15), "SysTick");
        assert_eq!(exception_name(16), "IRQ 0");
        assert_eq!(exception_name(61), "IRQ 45");
        assert_eq!(exception_name(9), "Reserved (9)");
    }

    #[test]
    fn describes_faults() {
        let mut block = vec![0u8; FAULT_BLOCK_LEN as usize];
        block[0..4].copy_from_slice(&0x0000_8200u32.to_le_bytes()); // PRECISERR | BFARVALID
        block[4..8].copy_from_slice(&0x4000_0000u32.to_le_bytes()); // FORCED
        block[16..20].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        let status = FaultStatus::from_block(&block).unwrap();
        assert!(status.is_fault());
        let text = status.describe().join("\n");
        assert!(text.contains("HFSR.FORCED"));
        assert!(text.contains("CFSR.PRECISERR"));
        assert!(text.contains("BFAR): 0xdeadbeef"));
        assert!(!text.contains("MMFAR"));
        assert_eq!(
            FaultStatus::summary(CFSR, status.cfsr),
            " [PRECISERR, BFARVALID]"
        );
        assert!(!FaultStatus::default().is_fault());
        assert!(FaultStatus::from_block(&[0; 4]).is_none());
    }

    #[test]
    fn decodes_exc_return() {
        assert_eq!(exc_return_uses_psp(0xFFFF_FFFD), Some(true));
        assert_eq!(exc_return_uses_psp(0xFFFF_FFF9), Some(false));
        assert_eq!(exc_return_uses_psp(0x0800_0123), None);
    }
}
