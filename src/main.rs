mod api;
mod cli;
mod commands;
mod context_engine;
mod error;
mod files;
mod handoff;
mod hooks;
mod models;
mod output;
mod ripgrep;
mod sanitize;
mod time_util;
mod tools;
mod toon;
mod usage_store;
mod worker;

use clap::Parser;

fn main() {
    // Fast-path: -v alone, before the full command graph / clap parses
    // anything else (mirrors the JS build's `tryFastPath`).
    let raw: Vec<String> = std::env::args().collect();
    if raw.len() == 2 && raw[1] == "-v" {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let cli = cli::Cli::parse();

    match cli::dispatch(&cli.command) {
        Ok(output) => println!("{}", output.render()),
        Err(err) => {
            // Errors render to stdout as structured TOON (matching the JS
            // build's runHandler, which writes formatted errors to stdout,
            // not stderr) with the exit code carrying the real signal.
            println!("{}", toon::render(&err.to_toon_value()));
            std::process::exit(err.exit_code());
        }
    }
}
