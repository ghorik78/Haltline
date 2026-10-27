//! Launch/attach configuration (the adapter-specific part of `.zed/debug.json`).

use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestKind {
    #[default]
    Launch,
    Attach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerType {
    #[default]
    Openocd,
    /// ST-LINK_gdbserver from STM32CubeIDE / STM32CubeCLT.
    #[serde(alias = "st-link", alias = "stlink-gdbserver")]
    Stlink,
    /// `st-util` from the open source stlink tools.
    #[serde(alias = "st-util")]
    Stutil,
    Pyocd,
    #[serde(alias = "j-link", alias = "segger")]
    Jlink,
    Qemu,
    /// An already running GDB server (`gdbTarget`).
    External,
}

impl ServerType {
    pub fn name(self) -> &'static str {
        match self {
            ServerType::Openocd => "OpenOCD",
            ServerType::Stlink => "ST-LINK GDB server",
            ServerType::Stutil => "st-util",
            ServerType::Pyocd => "pyOCD",
            ServerType::Jlink => "J-Link GDB server",
            ServerType::Qemu => "QEMU",
            ServerType::External => "external GDB server",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interface {
    #[default]
    Swd,
    Jtag,
}

/// When the top stack frame is shown as disassembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisassemblyMode {
    /// When stepping by instruction or when no source is available.
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub request: RequestKind,
    /// ELF file with debug information.
    #[serde(alias = "program", alias = "elf")]
    pub executable: Option<String>,
    pub cwd: Option<String>,

    #[serde(alias = "servertype")]
    pub server_type: ServerType,
    pub server_path: Option<String>,
    pub server_args: Vec<String>,
    pub server_env: HashMap<String, String>,
    /// Text printed by the server once it accepts GDB connections.
    pub server_ready_text: Option<String>,
    /// Milliseconds to wait for the GDB server to start.
    pub server_timeout: Option<u64>,
    /// `host:port` of the GDB server (required for `external`).
    pub gdb_target: Option<String>,
    /// GDB port to use for servers started by the adapter (default: a free port).
    pub port: Option<u16>,

    pub gdb_path: Option<String>,
    pub gdb_args: Vec<String>,

    // OpenOCD
    pub config_files: Vec<String>,
    pub search_dir: Vec<String>,
    pub openocd_commands: Vec<String>,
    /// Debug probe used to pick the OpenOCD interface file.
    pub probe: Option<String>,
    pub interface: Interface,
    /// Adapter clock in kHz.
    pub speed: Option<u32>,

    /// Device name, e.g. `STM32F407VG`.
    pub device: Option<String>,
    pub serial_number: Option<String>,
    /// pyOCD target name (defaults to the lower-cased device).
    pub target_id: Option<String>,
    /// QEMU machine (`-M`).
    pub machine: Option<String>,
    /// Path to STM32CubeProgrammer's `bin` directory (ST-LINK GDB server).
    #[serde(alias = "stm32CubeProgrammer", alias = "cubeProgrammerPath")]
    pub stm32cubeprogrammer: Option<String>,
    /// RTOS awareness for OpenOCD (`auto`, `FreeRTOS`, `Zephyr`, ...).
    pub rtos: Option<String>,
    pub semihosting: bool,

    pub svd_file: Option<String>,

    /// Function to run to after launch/restart; `""` disables. Default: `main`.
    pub run_to_entry_point: Option<String>,
    pub stop_on_entry: bool,
    /// Flash the executable on launch (default: true, except for QEMU).
    #[serde(alias = "flash")]
    pub load_image: Option<bool>,
    /// Milliseconds allowed for flashing.
    pub load_timeout: Option<u64>,
    pub continue_on_attach: bool,

    pub pre_launch_commands: Vec<String>,
    pub post_launch_commands: Vec<String>,
    pub override_launch_commands: Option<Vec<String>>,
    pub pre_attach_commands: Vec<String>,
    pub post_attach_commands: Vec<String>,
    pub override_attach_commands: Option<Vec<String>>,
    pub pre_restart_commands: Vec<String>,
    pub post_restart_commands: Vec<String>,
    pub override_restart_commands: Option<Vec<String>>,
    /// Commands that reset and halt the target (default depends on the server).
    pub reset_commands: Option<Vec<String>>,
    /// Re-flash the executable when the session is restarted.
    pub reload_on_restart: bool,

    pub force_hardware_breakpoints: bool,
    pub show_disassembly: DisassemblyMode,
    pub hex_values: bool,
    /// Show the Globals and Static scopes.
    pub show_globals: Option<bool>,
    /// Log every GDB/MI command and response to the debug console.
    #[serde(alias = "showDevDebugOutput")]
    pub debug_output: bool,
    /// Rewrite source paths: `{ "/build/dir": "/local/dir" }`.
    #[serde(alias = "sourceMap")]
    pub source_file_map: BTreeMap<String, String>,

    #[serde(flatten)]
    pub unknown: HashMap<String, Value>,
}

/// Keys Zed (or other clients) put into the configuration.
const CLIENT_KEYS: &[&str] = &[
    "label",
    "adapter",
    "type",
    "name",
    "build",
    "tcp_connection",
    "__restart",
    "__sessionId",
    "adapterPath",
    "env",
    "args",
];

impl Config {
    pub fn from_json(value: &Value) -> Result<Self> {
        let config: Config = serde_json::from_value(value.clone())
            .map_err(|e| anyhow::anyhow!("invalid debug configuration: {e}"))?;
        Ok(config)
    }

    /// Keys that are neither ours nor known client keys (likely typos).
    pub fn unknown_keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self
            .unknown
            .keys()
            .map(String::as_str)
            .filter(|k| !CLIENT_KEYS.contains(k))
            .collect();
        keys.sort_unstable();
        keys
    }

    pub fn base_dir(&self) -> PathBuf {
        self.cwd
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// Resolves a path relative to `cwd`.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = expand_home(path);
        if p.is_absolute() {
            p
        } else {
            self.base_dir().join(p)
        }
    }

    pub fn executable_path(&self) -> Option<PathBuf> {
        self.executable
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|p| self.resolve(p))
    }

    pub fn svd_path(&self) -> Option<PathBuf> {
        self.svd_file
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|p| self.resolve(p))
    }

    pub fn entry_point(&self) -> Option<&str> {
        match self.run_to_entry_point.as_deref() {
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s.trim()),
            None if self.request == RequestKind::Launch => Some("main"),
            None => None,
        }
    }

    pub fn should_load(&self) -> bool {
        self.request == RequestKind::Launch
            && self
                .load_image
                .unwrap_or(self.server_type != ServerType::Qemu)
    }

    pub fn globals_enabled(&self) -> bool {
        self.show_globals.unwrap_or(true)
    }

    /// Commands that reset the target and leave it halted.
    pub fn reset_commands(&self) -> Vec<String> {
        if let Some(cmds) = &self.reset_commands {
            return cmds.clone();
        }
        let cmds: &[&str] = match self.server_type {
            ServerType::Openocd | ServerType::Pyocd | ServerType::External => {
                &["monitor reset halt"]
            }
            ServerType::Stlink | ServerType::Stutil => &["monitor reset"],
            ServerType::Jlink => &["monitor reset", "monitor halt"],
            ServerType::Qemu => &["monitor system_reset"],
        };
        cmds.iter().map(|s| s.to_string()).collect()
    }

    /// Checks the configuration before anything is started.
    /// Unknown keys are not an error here; the session reports them as a warning.
    pub fn validate(&self) -> Result<()> {
        match self.executable_path() {
            Some(p) if !p.exists() => bail!("executable `{}` does not exist", p.display()),
            None if self.request == RequestKind::Launch => {
                bail!("`executable` is required: set it to the ELF file of your firmware")
            }
            _ => {}
        }
        if self.server_type == ServerType::Qemu && self.executable_path().is_none() {
            bail!("QEMU needs `executable` to know what to run");
        }
        if self.server_type == ServerType::External && self.gdb_target.is_none() {
            bail!("`gdbTarget` (host:port) is required when `serverType` is `external`");
        }
        if self.server_type == ServerType::Openocd
            && self.config_files.is_empty()
            && self
                .device
                .as_deref()
                .and_then(openocd_target_for_device)
                .is_none()
        {
            bail!(
                "OpenOCD needs `configFiles` (e.g. [\"interface/stlink.cfg\", \"target/stm32f4x.cfg\"]) \
                 or a known STM32 `device` (e.g. \"STM32F407VG\")"
            );
        }
        if self.server_type == ServerType::Jlink && self.device.is_none() {
            bail!("J-Link needs `device` (e.g. \"STM32F407VG\")");
        }
        if let Some(svd) = self.svd_path() {
            if !svd.exists() {
                bail!("SVD file `{}` does not exist", svd.display());
            }
        }
        Ok(())
    }
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// OpenOCD target configuration for an STM32 part number.
pub fn openocd_target_for_device(device: &str) -> Option<&'static str> {
    let d = device.trim().to_ascii_uppercase();
    let family = d.strip_prefix("STM32")?;
    const TABLE: &[(&str, &str)] = &[
        ("MP15", "target/stm32mp15x.cfg"),
        ("MP13", "target/stm32mp13x.cfg"),
        ("F0", "target/stm32f0x.cfg"),
        ("F1", "target/stm32f1x.cfg"),
        ("F2", "target/stm32f2x.cfg"),
        ("F3", "target/stm32f3x.cfg"),
        ("F4", "target/stm32f4x.cfg"),
        ("F7", "target/stm32f7x.cfg"),
        ("G0", "target/stm32g0x.cfg"),
        ("G4", "target/stm32g4x.cfg"),
        ("H7", "target/stm32h7x.cfg"),
        ("H5", "target/stm32h5x.cfg"),
        ("L0", "target/stm32l0.cfg"),
        ("L1", "target/stm32l1.cfg"),
        ("L4", "target/stm32l4x.cfg"),
        ("L5", "target/stm32l5x.cfg"),
        ("U5", "target/stm32u5x.cfg"),
        ("U0", "target/stm32u0x.cfg"),
        ("C0", "target/stm32c0x.cfg"),
        ("WB", "target/stm32wbx.cfg"),
        ("WL", "target/stm32wlx.cfg"),
    ];
    TABLE
        .iter()
        .find(|(prefix, _)| family.starts_with(prefix))
        .map(|(_, cfg)| *cfg)
}

/// OpenOCD interface configuration for a probe name.
pub fn openocd_interface_for_probe(probe: Option<&str>) -> &'static str {
    match probe.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        Some("jlink" | "j-link") => "interface/jlink.cfg",
        Some("cmsis-dap" | "cmsisdap" | "daplink") => "interface/cmsis-dap.cfg",
        Some("stlink-dap") => "interface/stlink-dap.cfg",
        Some("picoprobe") => "interface/cmsis-dap.cfg",
        _ => "interface/stlink.cfg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_minimal_launch_config() {
        let c = Config::from_json(&json!({
            "request": "launch",
            "executable": "build/fw.elf",
            "cwd": "/project",
            "device": "STM32F407VG",
            "label": "Debug", "adapter": "Haltline"
        }))
        .unwrap();
        assert_eq!(c.request, RequestKind::Launch);
        assert_eq!(c.server_type, ServerType::Openocd);
        assert_eq!(
            c.executable_path().unwrap(),
            Path::new("/project/build/fw.elf")
        );
        assert_eq!(c.entry_point(), Some("main"));
        assert!(c.should_load());
        assert!(c.unknown_keys().is_empty());
        assert_eq!(c.reset_commands(), ["monitor reset halt"]);
    }

    #[test]
    fn accepts_aliases_and_reports_unknown_keys() {
        let c = Config::from_json(&json!({
            "request": "attach",
            "program": "/fw.elf",
            "servertype": "st-util",
            "showDevDebugOutput": true,
            "svdfile": "typo.svd"
        }))
        .unwrap();
        assert_eq!(c.server_type, ServerType::Stutil);
        assert_eq!(c.executable.as_deref(), Some("/fw.elf"));
        assert!(c.debug_output);
        assert_eq!(c.unknown_keys(), ["svdfile"]);
        assert_eq!(c.entry_point(), None);
        assert!(!c.should_load());
    }

    #[test]
    fn entry_point_and_load_defaults() {
        let c = Config::from_json(&json!({"runToEntryPoint": "", "serverType": "qemu"})).unwrap();
        assert_eq!(c.entry_point(), None);
        assert!(!c.should_load());
        assert_eq!(c.reset_commands(), ["monitor system_reset"]);
        let c = Config::from_json(&json!({"serverType": "qemu", "loadImage": true})).unwrap();
        assert!(c.should_load());
    }

    #[test]
    fn rejects_bad_types() {
        assert!(Config::from_json(&json!({"serverType": "magic"})).is_err());
        assert!(Config::from_json(&json!({"port": "x"})).is_err());
    }

    #[test]
    fn validates_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("fw.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();
        let elf = elf.to_string_lossy().to_string();

        let err = Config::from_json(&json!({"request": "launch"}))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("executable"));

        let err = Config::from_json(&json!({"executable": "/nope/fw.elf"}))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"));

        let err = Config::from_json(&json!({"executable": elf}))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("configFiles"));

        Config::from_json(&json!({"executable": elf, "device": "stm32f103c8"}))
            .unwrap()
            .validate()
            .unwrap();

        let err = Config::from_json(&json!({"executable": elf, "serverType": "external"}))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("gdbTarget"));

        let err = Config::from_json(&json!({"executable": elf, "serverType": "jlink"}))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("device"));

        let err = Config::from_json(
            &json!({"executable": elf, "device": "STM32F4", "svdFile": "missing.svd", "cwd": "/"}),
        )
        .unwrap()
        .validate()
        .unwrap_err();
        assert!(err.to_string().contains("SVD"));

        // Attach without an ELF is allowed (disassembly-only debugging).
        Config::from_json(
            &json!({"request": "attach", "serverType": "external", "gdbTarget": "localhost:3333"}),
        )
        .unwrap()
        .validate()
        .unwrap();
    }

    #[test]
    fn maps_devices_to_openocd_targets() {
        assert_eq!(
            openocd_target_for_device("STM32F407VGT6"),
            Some("target/stm32f4x.cfg")
        );
        assert_eq!(
            openocd_target_for_device("stm32f103c8"),
            Some("target/stm32f1x.cfg")
        );
        assert_eq!(
            openocd_target_for_device("STM32H743ZI"),
            Some("target/stm32h7x.cfg")
        );
        assert_eq!(
            openocd_target_for_device("STM32MP157"),
            Some("target/stm32mp15x.cfg")
        );
        assert_eq!(
            openocd_target_for_device("STM32WB55"),
            Some("target/stm32wbx.cfg")
        );
        assert_eq!(openocd_target_for_device("nRF52840"), None);
        assert_eq!(openocd_interface_for_probe(None), "interface/stlink.cfg");
        assert_eq!(
            openocd_interface_for_probe(Some("J-Link")),
            "interface/jlink.cfg"
        );
        assert_eq!(
            openocd_interface_for_probe(Some("cmsis-dap")),
            "interface/cmsis-dap.cfg"
        );
    }

    #[test]
    fn resolves_relative_and_home_paths() {
        let c = Config::from_json(&json!({"cwd": "/work"})).unwrap();
        assert_eq!(c.resolve("a/b.elf"), Path::new("/work/a/b.elf"));
        assert_eq!(c.resolve("/abs.elf"), Path::new("/abs.elf"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(c.resolve("~/x.svd"), Path::new(&home).join("x.svd"));
        }
    }
}
