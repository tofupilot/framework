// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;

mod cli;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "--cli" | "-c" => {
                let (procedure_dir, procedure_file) = cli::parse_cli_args();
                tofupilot_lib::run_cli_mode(procedure_dir, procedure_file);
            }
            "--help" | "-h" => {
                cli::print_help(&args[0]);
                std::process::exit(0);
            }
            _ => tofupilot_lib::run(),
        }
    } else {
        tofupilot_lib::run();
    }
}
