use clap::Parser;

/// Run heavy builds and tests on another machine over SSH.
#[derive(Parser)]
#[command(name = "hopbox", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    eprintln!("hopbox: pre-release scaffold, no commands yet (see VISION.md)");
}
