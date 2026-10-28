<div align="center">

# Haltline

**STM32 and ARM Cortex-M debugging for the [Zed](https://zed.dev) editor**

[![CI](https://github.com/ghorik78/Haltline/actions/workflows/ci.yml/badge.svg)](https://github.com/ghorik78/Haltline/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ghorik78/Haltline)](https://github.com/ghorik78/Haltline/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey)

**English** · [Русский](README.ru.md)

</div>

Haltline adds a firmware debugger to Zed, similar to Cortex-Debug in VS Code. It flashes your firmware, stops at `main` and shows the CPU registers, peripheral registers from SVD files, variables and memory. It also supports disassembly, breakpoints and watchpoints, and explains hard faults. It works with OpenOCD, ST-LINK GDB server, st-util, pyOCD, SEGGER J-Link and QEMU.

## Contents

- [Features](#features)
- [How it works](#how-it-works)
- [Requirements](#requirements)
- [Installation](#installation)
- [Updating](#updating)
- [Quick start](#quick-start)
- [Configuration examples](#configuration-examples)
- [Configuration reference](#configuration-reference)
- [Using the debugger](#using-the-debugger)
- [Troubleshooting](#troubleshooting)
- [Limitations](#limitations)
- [Development](#development)
- [License](#license)

## Features

| Feature | Details |
|---|---|
| **GDB servers** | OpenOCD, ST-LINK_gdbserver (STM32CubeIDE / STM32CubeCLT), st-util, pyOCD, SEGGER J-Link, QEMU, or any server that is already running |
| **Flashing** | Flashes on launch and, optionally, on restart. Prints the stages (erase, write, program, verify) with progress, and can verify the flash afterwards |
| **CPU registers** | r0–r12, SP, LR, PC; xPSR with decoded flags and the exception name; MSP/PSP, PRIMASK, BASEPRI, FAULTMASK, CONTROL; FPU registers |
| **Peripherals (SVD)** | Every peripheral, register and bit field, with enumerated values. Registers and single fields can be edited (`MODE = Output`). Registers whose read has side effects are not read automatically |
| **Breakpoints** | Line, conditional, hit count, logpoints, function, instruction address, and data breakpoints (watchpoints on read, write or access) |
| **Fault analysis** | Stops when a fault handler is entered and explains it: CFSR/HFSR bits, the faulting address, and the source line where the fault happened |
| **Disassembly** | Instruction stepping in an assembly view, with source lines as comments. Breakpoints can be set on instructions |
| **Call stack** | Includes frames across exception entry (`<exception entry>`) |
| **Variables** | Locals, statics and globals; structs, arrays (split into ranges), pointers; watch expressions, hover, value editing, hex display |
| **Memory** | Viewing and writing memory in Zed's Memory view |
| **RTOS** | FreeRTOS and Zephyr tasks shown as threads (through OpenOCD) |
| **Semihosting** | `printf` output in the debug console |

## How it works

```
Zed ──DAP──▶ haltline-dap ──GDB/MI──▶ arm-none-eabi-gdb ──▶ GDB server ──▶ probe ──▶ MCU
             (Haltline)                                      (OpenOCD, J-Link, …)
```

Haltline has two parts:

* **The Zed extension** registers the `Haltline` debugger and the schema of its settings, and finds or downloads the adapter.
* **`haltline-dap`** is a small native program. It implements the Debug Adapter Protocol: it starts the GDB server and GDB, and turns Zed's requests into GDB/MI commands.

## Requirements

* **Zed** with debugger support (tested with Zed 1.26).
* **An ARM GDB**: `arm-none-eabi-gdb` (Arm GNU Toolchain, STM32CubeCLT) or `gdb-multiarch`. Haltline looks for it in `PATH` and in the standard STM32CubeCLT and Arm toolchain folders.
* **A GDB server for your probe**: OpenOCD (ST-LINK, J-Link, CMSIS-DAP), ST-LINK_gdbserver, st-util, pyOCD or SEGGER J-Link. To try Haltline without a board, use QEMU (`qemu-system-arm`).
* **The ELF file** of your firmware, built with debug information (`-g`).

Prebuilt adapters: Linux (x86_64, aarch64), macOS (Intel, Apple Silicon), Windows (x64, ARM64).

On Ubuntu/Debian, the open-source tools can be installed with:

```sh
sudo apt install gdb-multiarch openocd
```

## Installation

Haltline is not in the Zed extension registry yet. It is installed from a GitHub release.

### Install script (recommended)

The script installs the extension and the matching adapter into Zed's data folder.

**Linux / macOS**

```sh
curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash
```

**Windows (PowerShell)**

```powershell
irm https://github.com/ghorik78/Haltline/releases/latest/download/install.ps1 | iex
```

Then restart Zed, or run `zed: reload extensions` from the command palette.

Script options:

| Linux / macOS | Windows | Purpose |
|---|---|---|
| `--version v0.1.1` | `-Version v0.1.1` | Install a specific release instead of the latest |
| `--no-adapter` | `-NoAdapter` | Install only the extension; it downloads the adapter on first use |
| `--from DIR` | `-From DIR` | Use archives that are already downloaded (offline install) |
| `--zed-data DIR` | `-ZedData DIR` | Zed data folder, if it is not in the default place |

To pass options to the piped script, use `bash -s --`, for example:

```sh
curl -fsSL https://github.com/ghorik78/Haltline/releases/latest/download/install.sh | bash -s -- --version v0.1.1
```

Every release has a `SHA256SUMS` file for checking the downloaded archives.

### Manual installation

1. Download `haltline-zed-extension.tar.gz` (or `.zip`) from [Releases](https://github.com/ghorik78/Haltline/releases/latest).
2. Unpack it so that `extension.toml` ends up here:

   | OS | Path |
   |---|---|
   | Linux | `~/.local/share/zed/extensions/installed/haltline/extension.toml` |
   | Linux (Flatpak) | `~/.var/app/dev.zed.Zed/data/zed/extensions/installed/haltline/extension.toml` |
   | macOS | `~/Library/Application Support/Zed/extensions/installed/haltline/extension.toml` |
   | Windows | `%LOCALAPPDATA%\Zed\extensions\installed\haltline\extension.toml` |

3. Restart Zed. The adapter for your platform is downloaded from the same release the first time you start debugging.

### From source

```sh
git clone https://github.com/ghorik78/Haltline && cd Haltline
cargo install --path adapter      # installs haltline-dap into ~/.cargo/bin
```

In Zed, open **Extensions → Install Dev Extension** and select the repository folder. Zed builds the extension itself; this needs Rust installed through rustup.

### Which adapter is used

The extension uses the first `haltline-dap` it finds, in this order:

1. The Zed setting `"dap": { "Haltline": { "binary": "/path/to/haltline-dap" } }`
2. `"adapterPath"` in the debug configuration
3. `haltline-dap` in `PATH`
4. The adapter downloaded from the GitHub release that matches the extension version

## Updating

Extensions installed outside the Zed registry are not updated automatically.

* **Installed with the script or manually:** run the install script again. It replaces the extension and installs the matching adapter. Then restart Zed. Adapters of older versions are removed the next time the extension downloads one.
* **A specific version** (also to roll back): run the script with `--version vX.Y.Z` (`-Version` on Windows).
* **Dev extension:** run `git pull`, then click **Rebuild** next to Haltline on the Extensions page, and update the adapter with `cargo install --path adapter --force`.
* **Your own adapter** (`dap.Haltline.binary`, `adapterPath` or one in `PATH`): update it yourself. Otherwise Zed keeps using the old one.

To check the version of an adapter, run `haltline-dap --version`.

**Uninstall:** delete the `extensions/installed/haltline` and `extensions/work/haltline` folders from Zed's data folder.

## Quick start

1. Create `.zed/debug.json` in your project:

   ```json
   [
     {
       "label": "Debug firmware",
       "adapter": "Haltline",
       "request": "launch",
       "executable": "build/firmware.elf",
       "device": "STM32F407VG",
       "svdFile": "STM32F407.svd"
     }
   ]
   ```

2. Connect the board through an ST-LINK.
3. In Zed, run `debugger: start` (or click **+** in the Debug panel) and choose **Debug firmware**.

Haltline starts OpenOCD with `interface/stlink.cfg` and the target file for the device (`target/stm32f4x.cfg`). It then flashes the ELF file, resets the MCU and stops at `main`.

SVD files for STM32 come with STM32CubeIDE and STM32CubeCLT (`STMicroelectronics_CMSIS_SVD` folder), in ST's CMSIS packs, and in [cmsis-svd-data](https://github.com/cmsis-svd/cmsis-svd-data).

## Configuration examples

All examples go into the `.zed/debug.json` array. Zed suggests the settings and their descriptions while you edit the file.

<details>
<summary><b>OpenOCD with your own config files, an RTOS and a build step</b></summary>

```json
{
  "label": "STM32G4 + FreeRTOS",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "configFiles": ["interface/stlink.cfg", "target/stm32g4x.cfg"],
  "svdFile": "STM32G474.svd",
  "rtos": "FreeRTOS",
  "build": { "command": "make", "args": ["-j8"] }
}
```
</details>

<details>
<summary><b>ST-LINK GDB server (STM32CubeCLT)</b></summary>

```json
{
  "label": "ST-LINK GDB server",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "stlink",
  "executable": "build/app.elf",
  "svdFile": "STM32H743.svd"
}
```

STM32CubeProgrammer is found automatically in STM32CubeCLT; otherwise set `stm32cubeprogrammer`.
</details>

<details>
<summary><b>SEGGER J-Link</b></summary>

```json
{
  "label": "J-Link",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "jlink",
  "device": "STM32F407VG",
  "executable": "build/app.elf",
  "speed": 4000
}
```
</details>

<details>
<summary><b>pyOCD and st-util</b></summary>

```json
{
  "label": "pyOCD",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "pyocd",
  "device": "STM32F103C8",
  "executable": "build/app.elf"
},
{
  "label": "st-util",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "stutil",
  "executable": "build/app.elf"
}
```
</details>

<details>
<summary><b>Attach to a running board without a reset</b></summary>

```json
{
  "label": "Attach",
  "adapter": "Haltline",
  "request": "attach",
  "executable": "build/app.elf",
  "device": "STM32F407VG"
}
```

Add `"continueOnAttach": true` to let the program keep running after attaching.
</details>

<details>
<summary><b>A GDB server that is already running</b></summary>

```json
{
  "label": "External server",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "external",
  "gdbTarget": "localhost:3333",
  "executable": "build/app.elf"
}
```
</details>

<details>
<summary><b>Verify the flash, re-flash on restart, semihosting</b></summary>

```json
{
  "label": "Flash, verify, printf",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "device": "STM32F407VG",
  "verifyAfterLoad": true,
  "reloadOnRestart": true,
  "semihosting": true
}
```

With `reloadOnRestart`, the **Restart** button flashes the rebuilt firmware again.
</details>

<details>
<summary><b>Custom GDB commands</b></summary>

```json
{
  "label": "Custom commands",
  "adapter": "Haltline",
  "request": "launch",
  "executable": "build/app.elf",
  "device": "STM32F407VG",
  "preLaunchCommands": ["monitor adapter speed 1800"],
  "postLaunchCommands": ["set var g_debug_mode = 1"],
  "resetCommands": ["monitor reset init"]
}
```
</details>

<details>
<summary><b>QEMU (no board needed)</b></summary>

```json
{
  "label": "QEMU",
  "adapter": "Haltline",
  "request": "launch",
  "serverType": "qemu",
  "machine": "netduinoplus2",
  "executable": "build/app.elf"
}
```

There is a ready-made example in [`adapter/tests/firmware`](adapter/tests/firmware). Open that folder in Zed and start **Test firmware (QEMU, STM32F405)**. It needs `arm-none-eabi-gcc`, `make`, `qemu-system-arm` and an ARM GDB.
</details>

## Configuration reference

The full schema with descriptions is in [`debug_adapter_schemas/Haltline.json`](debug_adapter_schemas/Haltline.json). Relative paths are resolved against `cwd`, which is the project root by default.

### General

| Setting | Default | Description |
|---|---|---|
| `request` | — | `launch` flashes and resets the target; `attach` connects to a running target without a reset |
| `executable` | — | ELF file of the firmware. Required for `launch` |
| `cwd` | project root | Folder used for relative paths and for running the GDB server |
| `svdFile` | — | CMSIS-SVD file; turns on the **Peripherals** view |

### GDB server

| Setting | Default | Description |
|---|---|---|
| `serverType` | `openocd` | `openocd`, `stlink`, `stutil`, `pyocd`, `jlink`, `qemu`, `external` |
| `serverPath` | auto | Path to the server program |
| `serverArgs` | `[]` | Extra arguments for the server |
| `serverEnv` | `{}` | Extra environment variables for the server |
| `serverReadyText` | auto | Text in the server output that means it accepts connections |
| `serverTimeout` | `15000` | Milliseconds to wait for the server to start |
| `gdbTarget` | — | `host:port` of the server. Required for `external` |
| `port` | free port | GDB port of a server started by Haltline |
| `gdbPath` | auto | `arm-none-eabi-gdb` or `gdb-multiarch` |
| `gdbArgs` | `[]` | Extra GDB arguments |

### Probe and target

| Setting | Default | Description |
|---|---|---|
| `device` | — | MCU part number, e.g. `STM32F407VG`. Picks the OpenOCD target file (STM32 C0, F0–F7, G0, G4, H5, H7, L0–L5, U0, U5, WB, WL, MP1). Required for J-Link |
| `probe` | `stlink` | `stlink`, `stlink-dap`, `jlink`, `cmsis-dap`, `daplink`, `picoprobe`. Picks the OpenOCD interface file |
| `interface` | `swd` | `swd` or `jtag` |
| `speed` | server default | Probe clock in kHz |
| `serialNumber` | — | Probe serial number, when several probes are connected |
| `configFiles` | from `device` | OpenOCD config files (`-f`) |
| `searchDir` | `[]` | OpenOCD script folders (`-s`) |
| `openocdCommands` | `[]` | Extra OpenOCD commands (`-c`) |
| `rtos` | — | OpenOCD RTOS support: `auto`, `FreeRTOS`, `Zephyr`, … |
| `targetId` | from `device` | pyOCD target name |
| `machine` | `netduinoplus2` | QEMU machine |
| `stm32cubeprogrammer` | auto | STM32CubeProgrammer `bin` folder, for the ST-LINK GDB server |
| `semihosting` | `false` | Show the program's `printf` output in the debug console (OpenOCD, st-util, pyOCD, QEMU) |

### Flashing and startup

| Setting | Default | Description |
|---|---|---|
| `loadImage` | `true` (`false` for QEMU) | Flash the firmware on launch |
| `verifyAfterLoad` | `false` | After flashing, compare the MCU memory with the ELF file; a mismatch fails the launch |
| `loadTimeout` | `180000` | Milliseconds allowed for flashing |
| `reloadOnRestart` | `false` | Flash again on **Restart** |
| `runToEntryPoint` | `main` | Function to run to after start and restart; `""` disables this |
| `stopOnEntry` | `false` | Stop right after the reset, in `Reset_Handler` |
| `continueOnAttach` | `false` | Keep the program running after attaching |
| `resetCommands` | depends on the server | Commands that reset and halt the MCU (see below) |

Default reset commands: `monitor reset halt` (OpenOCD, pyOCD, external), `monitor reset` (ST-LINK, st-util), `monitor reset` + `monitor halt` (J-Link), `monitor system_reset` (QEMU).

### Custom GDB commands

| Setting | When it runs |
|---|---|
| `preLaunchCommands` / `postLaunchCommands` | Before the reset and flashing / after them |
| `overrideLaunchCommands` | Replaces the whole launch sequence (reset, load, reset) |
| `preAttachCommands` / `postAttachCommands` / `overrideAttachCommands` | The same for `attach` |
| `preRestartCommands` / `postRestartCommands` / `overrideRestartCommands` | The same for **Restart** |

A command that starts with `-` is sent as a GDB/MI command; everything else runs as a GDB console command.

### Display and diagnostics

| Setting | Default | Description |
|---|---|---|
| `showDisassembly` | `auto` | `auto`: when stepping by instruction or when there is no source; `always`; `never` |
| `hexValues` | `false` | Show values in hex |
| `showGlobals` | `true` | Show the **Globals** and **Static** scopes |
| `forceHardwareBreakpoints` | `false` | Always use hardware breakpoints |
| `sourceFileMap` | `{}` | Source path substitutions, e.g. `{"/build/src": "/home/me/project/src"}` |
| `debugOutput` | `false` | Log every GDB/MI command and response to the debug console |
| `adapterPath` | — | Use this `haltline-dap` instead of the downloaded one |

## Using the debugger

* **Flashing.** The debug console shows each stage as it happens: erase, write with a percentage, program and verify. A summary line follows, e.g. `Flashed 48.2 KiB in 6.3 s (erase 2.1 s, write 0.4 s, program 3.8 s)`. To cancel flashing, close the session with **×** in the session list.
* **Registers.** These are in the **Registers** scope of the Variables panel. Flags in xPSR and CONTROL can be expanded, and values can be edited in place.
* **Peripherals.** These are in the **Peripherals** scope: peripheral → register → fields. Registers are read only when their peripheral is expanded. To read a register whose read has side effects (e.g. `USART1.DR`), use `:svd USART1.DR`.
* **Disassembly.** Set `"debugger": { "stepping_granularity": "instruction" }` in Zed settings, or run `:asm on`. Code without source is always shown as assembly.
* **Breakpoint hit counts.** `5` or `>=5` (from the 5th hit), `==5`, `>5`, `<5`, `<=5`, `%5` (every 5th hit).
* **Logpoints** print a message and do not stop. Expressions go in braces: `i={i}, x={p->x}`.
* **Watchpoints.** Use the context menu of a variable or a peripheral register. Cortex-M has 4 DWT comparators (2 on M0).
* **Faults.** The **Faults** filter in the Breakpoints list is on by default. It stops on HardFault, MemManage, BusFault and UsageFault, and explains the cause in the console.
* **Restart** resets the MCU (and re-flashes it with `reloadOnRestart`), then runs to `main` again.

### Debug console

| Input | Action |
|---|---|
| `g_counter`, `buf[3]`, `*ptr` | Evaluate a C expression |
| `info registers`, `x/8xw 0x20000000`, `monitor reset halt` | Run a GDB command (recognized automatically) |
| `> command` | Always run as a GDB command |
| `:asm [on\|off\|auto]` | Assembly view of the current frame |
| `:hex [on\|off]` | Hex display of values |
| `:reset [run]` | Reset the MCU and stay halted, or reset and run |
| `:fault` | Explain the current fault |
| `:svd [PERIPH[.REG]]` | List peripherals or read registers |
| `:mi -command` | Send a raw GDB/MI command |
| `:help` | Help |

## Troubleshooting

| Problem | What to do |
|---|---|
| The probe is not found | Check the USB connection and drivers (udev rules on Linux). Set `probe` or `configFiles`, and `serialNumber` if several probes are connected |
| GDB is not found | Install `gdb-multiarch` or the Arm GNU Toolchain, or set `gdbPath` |
| The server does not start | Its output is in the debug console. Check `serverPath` and the config files |
| Flashing times out | Increase `loadTimeout` and lower `speed` |
| Breakpoints are not hit, or the source is not found | Build with `-g`. If the firmware was built in another folder, use `sourceFileMap` |
| Something else is wrong | Set `"debugOutput": true` to see the GDB/MI traffic, and open the adapter logs with `dev: open debug adapter logs` |

Bug reports are welcome in [Issues](https://github.com/ghorik78/Haltline/issues). Please attach your `debug.json` and the debug console output.

## Limitations

* Zed has no separate register, peripheral or disassembly panels. Registers and peripherals are shown in the Variables panel, and assembly in generated `.s` files.
* Zed does not show progress bars for debug adapters, so flashing progress is printed to the debug console.
* SWO/ITM tracing and RTT are not supported yet.
* QEMU does not allow writing to emulated peripheral registers. Writing works on a real MCU.

## Development

```
src/                      Zed extension (WebAssembly)
debug_adapter_schemas/    configuration schema
adapter/src/              haltline-dap: DAP, GDB/MI, session, SVD, Cortex-M, flashing, disassembly
adapter/tests/            adapter tests and end-to-end tests on QEMU
scripts/                  packaging and install scripts
```

```sh
cargo test --workspace
```

The end-to-end tests run the real adapter with QEMU and GDB. When the tools are missing, they are skipped; `HALTLINE_E2E=1` makes a missing tool an error. Tool paths can be set with `HALTLINE_QEMU`, `HALTLINE_GDB` and `HALTLINE_CC`.

**Releases.** Set the same version in `Cargo.toml`, `adapter/Cargo.toml` and `extension.toml`, then push a tag:

```sh
git tag v0.1.2 && git push origin v0.1.2
```

GitHub Actions builds the adapter for all platforms, packages the extension and publishes the release.

## License

[MIT](LICENSE)
