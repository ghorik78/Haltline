//! Finding tools and running the GDB server (OpenOCD, ST-LINK, pyOCD, ...).

use crate::config::{
    openocd_interface_for_probe, openocd_target_for_device, Config, Interface, ServerType,
};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How to start a GDB server and how to tell that it is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Case-insensitive texts; any of them in the output means "ready".
    pub ready_texts: Vec<String>,
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Searches `PATH` and then `extra_dirs` for the first matching program.
pub fn find_executable(names: &[&str], extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for name in names {
        let file = exe_name(name);
        for dir in path_dirs.iter().chain(extra_dirs) {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Directories `<parent>/<prefix>*/<suffix>` that exist, newest name first.
pub fn glob_dirs(parent: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        })
        .map(|e| e.path().join(suffix))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Install locations of STM32CubeCLT / STM32CubeIDE bundled tools.
fn st_tool_dirs(suffix: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for parent in [
        "/opt/st",
        "/opt/ST",
        "C:\\ST",
        "/Applications",
        "C:\\Program Files\\STMicroelectronics",
    ] {
        dirs.extend(glob_dirs(Path::new(parent), "stm32cubeclt", suffix));
    }
    if let Some(h) = home() {
        dirs.extend(glob_dirs(&h.join("st"), "stm32cubeclt", suffix));
    }
    dirs
}

pub fn find_gdb(config: &Config) -> Result<PathBuf> {
    if let Some(p) = &config.gdb_path {
        let resolved = if p.contains('/') || p.contains('\\') {
            config.resolve(p)
        } else {
            find_executable(&[p], &[]).unwrap_or_else(|| PathBuf::from(p))
        };
        return Ok(resolved);
    }
    let mut extra = st_tool_dirs("GNU-tools-for-STM32/bin");
    for parent in [
        "/opt",
        "/usr/local",
        "/usr/share",
        "/Applications/ArmGNUToolchain",
    ] {
        extra.extend(glob_dirs(Path::new(parent), "arm-gnu-toolchain", "bin"));
        extra.extend(glob_dirs(Path::new(parent), "gcc-arm-none-eabi", "bin"));
    }
    for parent in [
        "C:\\Program Files (x86)\\Arm GNU Toolchain arm-none-eabi",
        "C:\\Program Files\\Arm GNU Toolchain arm-none-eabi",
    ] {
        extra.extend(glob_dirs(Path::new(parent), "", "bin"));
    }
    find_executable(
        &[
            "arm-none-eabi-gdb",
            "gdb-multiarch",
            "arm-none-eabi-gdb-py3",
        ],
        &extra,
    )
    .ok_or_else(|| {
        anyhow!(
            "cannot find arm-none-eabi-gdb or gdb-multiarch in PATH; install the Arm GNU toolchain \
             or set `gdbPath`"
        )
    })
}

fn find_server_program(config: &Config, names: &[&str], extra: &[PathBuf]) -> Result<PathBuf> {
    if let Some(p) = &config.server_path {
        return Ok(if p.contains('/') || p.contains('\\') {
            config.resolve(p)
        } else {
            find_executable(&[p], &[]).unwrap_or_else(|| PathBuf::from(p))
        });
    }
    find_executable(names, extra).ok_or_else(|| {
        anyhow!(
            "cannot find {} ({}) in PATH; install it or set `serverPath`",
            config.server_type.name(),
            names.join(" / ")
        )
    })
}

fn find_cube_programmer(config: &Config) -> Result<PathBuf> {
    if let Some(p) = &config.stm32cubeprogrammer {
        return Ok(config.resolve(p));
    }
    let mut candidates = st_tool_dirs("STM32CubeProgrammer/bin");
    candidates.push(PathBuf::from(
        "C:\\Program Files\\STMicroelectronics\\STM32Cube\\STM32CubeProgrammer\\bin",
    ));
    candidates.push(PathBuf::from(
        "/Applications/STMicroelectronics/STM32Cube/STM32CubeProgrammer/STM32CubeProgrammer.app/Contents/MacOs/bin",
    ));
    if let Some(h) = home() {
        candidates.push(h.join("STMicroelectronics/STM32Cube/STM32CubeProgrammer/bin"));
    }
    candidates.into_iter().find(|p| p.is_dir()).ok_or_else(|| {
        anyhow!(
            "ST-LINK GDB server needs STM32CubeProgrammer; set `stm32cubeprogrammer` to its `bin` directory"
        )
    })
}

/// Builds the command line of the GDB server. `None` for `external`.
pub fn server_command(config: &Config, port: u16) -> Result<Option<ServerCommand>> {
    let mut args: Vec<String> = Vec::new();
    let c = |s: &str| s.to_string();
    let (program, ready): (PathBuf, &[&str]) = match config.server_type {
        ServerType::External => return Ok(None),
        ServerType::Openocd => {
            for dir in &config.search_dir {
                args.extend([c("-s"), config.resolve(dir).to_string_lossy().into_owned()]);
            }
            args.extend([
                c("-c"),
                format!("gdb_port {port}"),
                c("-c"),
                c("tcl_port disabled"),
                c("-c"),
                c("telnet_port disabled"),
            ]);
            if config.config_files.is_empty() {
                let probe = config.probe.as_deref();
                let interface = openocd_interface_for_probe(probe);
                let target = config
                    .device
                    .as_deref()
                    .and_then(openocd_target_for_device)
                    .ok_or_else(|| anyhow!("unknown device; set `configFiles`"))?;
                args.extend([c("-f"), c(interface)]);
                let transport = match (interface, config.interface) {
                    ("interface/stlink.cfg", Interface::Swd) => None,
                    ("interface/stlink.cfg", Interface::Jtag) => Some("hla_jtag"),
                    ("interface/stlink-dap.cfg", Interface::Swd) => Some("dapdirect_swd"),
                    ("interface/stlink-dap.cfg", Interface::Jtag) => Some("dapdirect_jtag"),
                    (_, Interface::Swd) => Some("swd"),
                    (_, Interface::Jtag) => Some("jtag"),
                };
                if let Some(t) = transport {
                    args.extend([c("-c"), format!("transport select {t}")]);
                }
                if let Some(sn) = &config.serial_number {
                    args.extend([c("-c"), format!("adapter serial {sn}")]);
                }
                args.extend([c("-f"), c(target)]);
            } else {
                for f in &config.config_files {
                    let resolved = config.resolve(f);
                    let value = if resolved.exists() {
                        resolved.to_string_lossy().into_owned()
                    } else {
                        f.clone() // relative to OpenOCD's scripts directory
                    };
                    args.extend([c("-f"), value]);
                }
                if let Some(sn) = &config.serial_number {
                    args.extend([c("-c"), format!("adapter serial {sn}")]);
                }
            }
            if let Some(speed) = config.speed {
                args.extend([c("-c"), format!("adapter speed {speed}")]);
            }
            if let Some(rtos) = &config.rtos {
                args.extend([c("-c"), format!("$_TARGETNAME configure -rtos {rtos}")]);
            }
            for cmd in &config.openocd_commands {
                args.extend([c("-c"), cmd.clone()]);
            }
            (
                find_server_program(config, &["openocd"], &[])?,
                &["listening on port", "for gdb connections"],
            )
        }
        ServerType::Stlink => {
            let program = find_server_program(
                config,
                &["ST-LINK_gdbserver"],
                &st_tool_dirs("STLink-gdb-server/bin"),
            )?;
            args.extend([c("-p"), port.to_string()]);
            args.extend([
                c("-cp"),
                find_cube_programmer(config)?.to_string_lossy().into_owned(),
            ]);
            if config.interface == Interface::Swd {
                args.push(c("-d"));
            }
            if let Some(sn) = &config.serial_number {
                args.extend([c("-i"), sn.clone()]);
            }
            if let Some(speed) = config.speed {
                args.extend([c("--frequency"), speed.to_string()]);
            }
            (
                program,
                &["waiting for debugger connection", "listening at"],
            )
        }
        ServerType::Stutil => {
            args.extend([c("-p"), port.to_string()]);
            if let Some(sn) = &config.serial_number {
                args.push(format!("--serial={sn}"));
            }
            if config.semihosting {
                args.push(c("--semihosting"));
            }
            (
                find_server_program(config, &["st-util"], &[])?,
                &["listening at"],
            )
        }
        ServerType::Pyocd => {
            args.extend([c("gdbserver"), c("--port"), port.to_string()]);
            let target = config
                .target_id
                .clone()
                .or_else(|| config.device.as_ref().map(|d| d.to_ascii_lowercase()));
            if let Some(t) = target {
                args.extend([c("--target"), t]);
            }
            if let Some(sn) = &config.serial_number {
                args.extend([c("--uid"), sn.clone()]);
            }
            if let Some(speed) = config.speed {
                args.extend([c("--frequency"), (u64::from(speed) * 1000).to_string()]);
            }
            if config.semihosting {
                args.push(c("--semihosting"));
            }
            (
                find_server_program(config, &["pyocd"], &[])?,
                &["listening on port", "started on port"],
            )
        }
        ServerType::Jlink => {
            let mut extra = Vec::new();
            for parent in [
                "/opt/SEGGER",
                "/Applications/SEGGER",
                "C:\\Program Files\\SEGGER",
                "C:\\Program Files (x86)\\SEGGER",
            ] {
                extra.extend(glob_dirs(Path::new(parent), "jlink", ""));
            }
            let program = find_server_program(
                config,
                &["JLinkGDBServerCLExe", "JLinkGDBServerCL", "JLinkGDBServer"],
                &extra,
            )?;
            let device = config
                .device
                .clone()
                .ok_or_else(|| anyhow!("J-Link needs `device`"))?;
            args.extend([
                c("-if"),
                c(if config.interface == Interface::Jtag {
                    "JTAG"
                } else {
                    "SWD"
                }),
                c("-device"),
                device,
                c("-port"),
                port.to_string(),
                c("-swoport"),
                (u32::from(port) + 1).to_string(),
                c("-telnetport"),
                (u32::from(port) + 2).to_string(),
                c("-speed"),
                config.speed.map_or(c("auto"), |s| s.to_string()),
                c("-nogui"),
                c("-singlerun"),
                c("-localhostonly"),
            ]);
            if let Some(sn) = &config.serial_number {
                args.extend([c("-select"), format!("USB={sn}")]);
            }
            (program, &["waiting for gdb connection"])
        }
        ServerType::Qemu => {
            let program = find_server_program(config, &["qemu-system-arm"], &[])?;
            let elf = config
                .executable_path()
                .ok_or_else(|| anyhow!("QEMU needs `executable`"))?;
            args.extend([
                c("-M"),
                config.machine.clone().unwrap_or_else(|| c("netduinoplus2")),
                c("-display"),
                c("none"),
                c("-monitor"),
                c("none"),
                c("-serial"),
                c("null"),
                c("-S"),
                c("-gdb"),
                format!("tcp:127.0.0.1:{port}"),
                c("-kernel"),
                elf.to_string_lossy().into_owned(),
            ]);
            if config.semihosting {
                args.extend([c("-semihosting-config"), c("enable=on,target=native")]);
            }
            (program, &[])
        }
    };
    args.extend(config.server_args.iter().cloned());
    let mut ready_texts: Vec<String> = ready.iter().map(|s| s.to_string()).collect();
    if let Some(text) = &config.server_ready_text {
        ready_texts = vec![text.clone()];
    }
    Ok(Some(ServerCommand {
        program,
        args,
        ready_texts,
    }))
}

/// Address GDB should connect to.
pub fn gdb_target(config: &Config, port: u16) -> String {
    match (&config.gdb_target, config.server_type) {
        (Some(t), _) => {
            if t.contains(':') {
                t.clone()
            } else {
                format!("localhost:{t}")
            }
        }
        (None, _) => format!("localhost:{port}"),
    }
}

/// Picks a free TCP port on localhost.
pub fn free_port() -> Result<u16> {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).context("cannot allocate a TCP port")?;
    Ok(listener.local_addr()?.port())
}

/// Output from the server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    Line { text: String, stderr: bool },
    Exited(Option<i32>),
}

/// A running child process whose output is forwarded line by line.
pub struct ManagedProcess {
    child: Arc<Mutex<Child>>,
    recent: Arc<Mutex<VecDeque<String>>>,
    name: String,
}

/// Makes the child die with the adapter on Linux, even if the adapter is killed.
#[cfg(target_os = "linux")]
pub fn tie_to_parent(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl is async-signal-safe and only affects the child.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn tie_to_parent(_cmd: &mut Command) {}

impl ManagedProcess {
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        sink: impl Fn(ServerEvent) + Send + Sync + 'static,
    ) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        tie_to_parent(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start {}", program.display()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));
        let recent = Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::new(sink);
        let spawn_reader = |stream: Box<dyn Read + Send>,
                            stderr: bool,
                            wait_for: Option<thread::JoinHandle<()>>| {
            let sink = sink.clone();
            let recent = recent.clone();
            let child = child.clone();
            let notify_exit = !stderr;
            thread::spawn(move || {
                let reader = BufReader::new(stream);
                for line in reader.split(b'\n').map_while(Result::ok) {
                    let text = String::from_utf8_lossy(&line)
                        .trim_end_matches('\r')
                        .to_string();
                    {
                        let mut r = recent.lock().unwrap();
                        r.push_back(text.clone());
                        while r.len() > 30 {
                            r.pop_front();
                        }
                    }
                    sink(ServerEvent::Line { text, stderr });
                }
                // Report the exit only after all output has been forwarded.
                if let Some(other) = wait_for {
                    let _ = other.join();
                }
                if notify_exit {
                    for _ in 0..100 {
                        if let Ok(Some(status)) = child.lock().unwrap().try_wait() {
                            sink(ServerEvent::Exited(status.code()));
                            return;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            })
        };
        let stderr_thread = stderr.map(|err| spawn_reader(Box::new(err), true, None));
        if let Some(out) = stdout {
            spawn_reader(Box::new(out), false, stderr_thread);
        }
        Ok(Self {
            child,
            recent,
            name: program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Last lines of output, for error messages.
    pub fn recent_output(&self) -> String {
        self.recent
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn exit_code(&self) -> Option<Option<i32>> {
        self.child
            .lock()
            .unwrap()
            .try_wait()
            .ok()
            .flatten()
            .map(|s| s.code())
    }

    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Checks that a text from the server output signals readiness.
pub fn is_ready_line(line: &str, ready_texts: &[String]) -> bool {
    let lower = line.to_ascii_lowercase();
    ready_texts
        .iter()
        .any(|t| lower.contains(&t.to_ascii_lowercase()))
}

/// Explains common server failures with a hint.
pub fn diagnose_server_output(output: &str) -> Option<&'static str> {
    let lower = output.to_ascii_lowercase();
    if lower.contains("libusb_open() failed") || lower.contains("libusb_error_access") {
        Some("no permission to access the USB probe (install udev rules for ST-LINK / your probe)")
    } else if lower.contains("open failed")
        || lower.contains("no device found")
        || lower.contains("unable to find")
    {
        Some("the debug probe was not found; check the USB cable and that no other debugger is using it")
    } else if lower.contains("unknown device")
        || lower.contains("unexpected idcode")
        || lower.contains("unknown target")
    {
        Some("the target was not recognised; check `device` / `configFiles`")
    } else if lower.contains("address already in use") || lower.contains("couldn't bind") {
        Some("the GDB port is already in use; another server is probably still running")
    } else if lower.contains("target voltage may be too low") || lower.contains("init mode failed")
    {
        Some("cannot connect to the target; check power and SWD wiring (try `connect under reset`)")
    } else {
        None
    }
}

/// Fails if `program` is not runnable, with a helpful message.
pub fn check_program(program: &Path, what: &str) -> Result<()> {
    if program.components().count() > 1 && !program.exists() {
        bail!("{what} `{}` does not exist", program.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(v: serde_json::Value) -> Config {
        let mut v = v;
        v["serverPath"] = json!("/bin/sh");
        Config::from_json(&v).unwrap()
    }

    #[test]
    fn openocd_command_from_device() {
        let c = config(
            json!({"device": "STM32F407VG", "rtos": "auto", "speed": 4000, "serialNumber": "ABC"}),
        );
        let cmd = server_command(&c, 50000).unwrap().unwrap();
        let a = cmd.args.join(" ");
        assert!(a.contains("-c gdb_port 50000"));
        assert!(a.contains("-f interface/stlink.cfg"));
        assert!(a.contains("-f target/stm32f4x.cfg"));
        assert!(a.contains("adapter serial ABC"));
        assert!(a.contains("adapter speed 4000"));
        assert!(a.ends_with("-c $_TARGETNAME configure -rtos auto"));
        assert!(!a.contains("transport select"));
        let target_pos = cmd
            .args
            .iter()
            .position(|x| x == "target/stm32f4x.cfg")
            .unwrap();
        let rtos_pos = cmd.args.iter().position(|x| x.contains("-rtos")).unwrap();
        assert!(
            rtos_pos > target_pos,
            "rtos must be configured after the target"
        );
        assert!(is_ready_line(
            "Info : Listening on port 50000 for gdb connections",
            &cmd.ready_texts
        ));
    }

    #[test]
    fn openocd_transport_selection() {
        let c = config(json!({"device": "STM32F103C8", "probe": "cmsis-dap"}));
        let a = server_command(&c, 1).unwrap().unwrap().args.join(" ");
        assert!(
            a.contains("-f interface/cmsis-dap.cfg -c transport select swd -f target/stm32f1x.cfg")
        );
        let c = config(json!({"device": "STM32F103C8", "interface": "jtag"}));
        let a = server_command(&c, 1).unwrap().unwrap().args.join(" ");
        assert!(a.contains("transport select hla_jtag"));
    }

    #[test]
    fn openocd_command_from_config_files() {
        let c = config(json!({
            "configFiles": ["interface/stlink.cfg", "target/stm32g4x.cfg"],
            "searchDir": ["/opt/scripts"],
            "openocdCommands": ["init"],
            "serverArgs": ["-d2"]
        }));
        let cmd = server_command(&c, 3333).unwrap().unwrap();
        assert_eq!(
            cmd.args[..2],
            ["-s".to_string(), "/opt/scripts".to_string()]
        );
        let a = cmd.args.join(" ");
        assert!(a.contains("-f interface/stlink.cfg -f target/stm32g4x.cfg"));
        assert!(a.ends_with("-c init -d2"));
    }

    #[test]
    fn other_server_commands() {
        let c = config(json!({"serverType": "stutil", "semihosting": true}));
        let cmd = server_command(&c, 4242).unwrap().unwrap();
        assert_eq!(cmd.args, ["-p", "4242", "--semihosting"]);

        let c = config(json!({"serverType": "pyocd", "device": "STM32F407VG", "speed": 1000}));
        let cmd = server_command(&c, 3333).unwrap().unwrap();
        assert_eq!(
            cmd.args,
            [
                "gdbserver",
                "--port",
                "3333",
                "--target",
                "stm32f407vg",
                "--frequency",
                "1000000"
            ]
        );

        let c = config(json!({"serverType": "jlink", "device": "STM32F407VG"}));
        let a = server_command(&c, 2331).unwrap().unwrap().args.join(" ");
        assert!(a.contains("-if SWD -device STM32F407VG -port 2331 -swoport 2332 -telnetport 2333"));

        let c = config(
            json!({"serverType": "qemu", "executable": "/fw.elf", "machine": "olimex-stm32-h405"}),
        );
        let cmd = server_command(&c, 1234).unwrap().unwrap();
        let a = cmd.args.join(" ");
        assert!(a.contains("-M olimex-stm32-h405"));
        assert!(a.contains(&format!(
            "-gdb tcp:127.0.0.1:1234 -kernel {}",
            c.resolve("/fw.elf").display()
        )));
        assert!(cmd.ready_texts.is_empty());

        let c = config(json!({"serverType": "stlink", "stm32cubeprogrammer": "/opt/cp/bin"}));
        let cmd = server_command(&c, 61234).unwrap().unwrap();
        let cube = c.resolve("/opt/cp/bin").to_string_lossy().into_owned();
        assert_eq!(cmd.args, ["-p", "61234", "-cp", cube.as_str(), "-d"]);

        let c = config(json!({"serverType": "external", "gdbTarget": "192.168.0.5:3333"}));
        assert!(server_command(&c, 1).unwrap().is_none());
        assert_eq!(gdb_target(&c, 1), "192.168.0.5:3333");
        let c = config(json!({"serverType": "external", "gdbTarget": "2331"}));
        assert_eq!(gdb_target(&c, 1), "localhost:2331");
    }

    #[test]
    fn ready_text_override() {
        let c = config(
            json!({"serverType": "qemu", "executable": "/fw.elf", "serverReadyText": "go!"}),
        );
        let cmd = server_command(&c, 1).unwrap().unwrap();
        assert_eq!(cmd.ready_texts, ["go!"]);
    }

    #[test]
    fn diagnoses_common_errors() {
        assert!(diagnose_server_output("Error: open failed")
            .unwrap()
            .contains("probe"));
        assert!(diagnose_server_output("Error: couldn't bind gdb to socket")
            .unwrap()
            .contains("port"));
        assert!(diagnose_server_output("all good").is_none());
    }

    #[test]
    fn finds_executables_in_extra_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "fake-tool.exe"
        } else {
            "fake-tool"
        };
        std::fs::write(tmp.path().join(name), b"").unwrap();
        assert_eq!(
            find_executable(&["fake-tool"], &[tmp.path().to_path_buf()]),
            Some(tmp.path().join(name))
        );
        assert!(find_executable(&["surely-not-a-real-tool-xyz"], &[]).is_none());
        std::fs::create_dir_all(tmp.path().join("STM32CubeCLT_1.16.0/bin")).unwrap();
        std::fs::create_dir_all(tmp.path().join("STM32CubeCLT_1.18.0/bin")).unwrap();
        let dirs = glob_dirs(tmp.path(), "stm32cubeclt", "bin");
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].to_string_lossy().contains("1.18.0"));
    }

    #[test]
    fn free_port_is_bindable() {
        let port = free_port().unwrap();
        assert!(port > 0);
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_process_forwards_output_and_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let p = ManagedProcess::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "echo hello; echo oops >&2; exit 3".into()],
            Path::new("/"),
            &[],
            move |e| {
                let _ = tx.send(e);
            },
        )
        .unwrap();
        let mut events = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            let done = matches!(e, ServerEvent::Exited(_));
            events.push(e);
            if done {
                break;
            }
        }
        assert!(events.contains(&ServerEvent::Line {
            text: "hello".into(),
            stderr: false
        }));
        assert!(events.contains(&ServerEvent::Exited(Some(3))));
        assert!(p.recent_output().contains("hello"));
    }
}
