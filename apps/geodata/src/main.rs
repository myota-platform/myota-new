//! Entry point for the MyOTA geodata catalogue service.
//!
//! Scaffolding from chunk F1: prints one line and exits. A7 replaces this
//! with a service that serves traffic.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", myota_geodata::describe(&args));
    ExitCode::SUCCESS
}
