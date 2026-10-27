//! `haltline-dap`: speaks DAP on stdin/stdout.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => println!("haltline-dap {}", haltline_dap::VERSION),
        Some("--help" | "-h") => {
            println!(
                "haltline-dap {}\n\nDebug Adapter Protocol server for STM32 / ARM Cortex-M.\n\
                 It is started by an editor (Zed) and talks DAP over stdin/stdout.\n\n\
                 Options:\n  --version   print the version\n  --help      print this help",
                haltline_dap::VERSION
            );
        }
        Some(other) => {
            eprintln!("unknown argument `{other}` (try --help)");
            std::process::exit(2);
        }
        None => haltline_dap::session::run_stdio(),
    }
}
