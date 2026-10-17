#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;
use devagotchi_desktop::{Launch, parse_launch, run, USAGE};

fn main() -> ExitCode {
    let launch = parse_launch(
        std::env::args_os().skip(1),
        std::env::var_os("DEVAGOTCHI_SOCKET"),
    );
    match launch {
        Ok(Launch::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Launch::Open(connection)) => match run(connection) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Devagotchi: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("Devagotchi: {error}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
