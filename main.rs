// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "--cli" | "-c" => {
                let procedure_path = tofupilot_lib::cli::parse_args();
                tofupilot_lib::cli::run(procedure_path);
            }
            "--help" | "-h" => {
                tofupilot_lib::cli::print_help(&args[0]);
                std::process::exit(0);
            }
            _ => tofupilot_lib::run(),
        }
    } else {
        tofupilot_lib::run();
    }
}
