// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "run" => {
                tofupilot_studio_lib::cli::init_logger();
                let procedure_path = tofupilot_studio_lib::cli::parse_args();
                tofupilot_studio_lib::cli::run(procedure_path);
            }
            "help" => {
                tofupilot_studio_lib::cli::print_help(&args[0]);
                std::process::exit(0);
            }
            _ => tofupilot_studio_lib::run(),
        }
    } else {
        tofupilot_studio_lib::run();
    }
}
