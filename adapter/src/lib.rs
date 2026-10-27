//! Haltline: a Debug Adapter Protocol server for STM32 / ARM Cortex-M
//! microcontrollers, built on top of GDB/MI and a GDB server
//! (OpenOCD, ST-LINK GDB server, st-util, pyOCD, J-Link or QEMU).

pub mod breakpoints;
pub mod config;
pub mod cortex;
pub mod dap;
pub mod disasm;
pub mod mi;
pub mod server;
pub mod session;
pub mod svd;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
