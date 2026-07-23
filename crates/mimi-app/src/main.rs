mod config;
mod ctl;
mod gui;
mod input;
mod palette;

use config::Config;

const USAGE: &str = "\
mimi — a minimalist, GPU-accelerated, agent-native terminal

USAGE:
    mimi                     Launch the terminal window
    mimi ctl <command>       Talk to a running mimi window (see `mimi ctl --help`)
    mimi --version           Print the version
    mimi --help              Show this message";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("ctl") => std::process::exit(ctl::main(&args[1..])),
        Some("--version" | "-V") => {
            println!("mimi {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help" | "-h") => println!("{USAGE}"),
        Some(other) if other.starts_with('-') => {
            eprintln!("unknown flag: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
        _ => {
            let config = Config::load();
            if let Err(e) = gui::run(config) {
                eprintln!("mimi: {e}");
                std::process::exit(1);
            }
        }
    }
}
