use std::env;
use std::path::PathBuf;

pub fn parse_cli_args() -> (PathBuf, Option<String>) {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        print_usage(&args[0]);
        std::process::exit(1);
    }

    let procedure_dir = PathBuf::from(&args[2]);
    if !procedure_dir.exists() {
        eprintln!(
            "Error: Directory does not exist: {}",
            procedure_dir.display()
        );
        std::process::exit(1);
    }

    let mut procedure_file = None;

    let mut i = 3;
    while i < args.len() {
        let arg = &args[i];
        if arg.starts_with("--") {
            eprintln!("Error: Unknown parameter: {}", arg);
            print_usage(&args[0]);
            std::process::exit(1);
        } else if procedure_file.is_none() {
            procedure_file = Some(arg.to_string());
        } else {
            eprintln!("Error: Unexpected argument: {}", arg);
            print_usage(&args[0]);
            std::process::exit(1);
        }
        i += 1;
    }

    (procedure_dir, procedure_file)
}

pub fn print_help(program_name: &str) {
    let cpu_count = num_cpus::get();

    println!("TofuPilot");
    println!("\nUsage:");
    println!("  {} [OPTIONS]", program_name);
    println!("\nOptions:");
    println!("  --cli, -c <dir> [file]  Run procedure from directory in CLI mode");
    println!("  --help, -h              Show this help message");
    println!("\nExecution Configuration:");
    println!("  All execution parameters (slots, workers, model) are configured in the YAML file:");
    println!();
    println!("  execution:");
    println!("    model: slot_first          # or phase_first");
    println!("    workers: {}                 # default: CPU cores", cpu_count);
    println!("    slots:");
    println!("      - id: SLOT_A");
    println!("        name: Socket A");
    println!("      - id: SLOT_B");
    println!("        name: Socket B");
    println!("\nExecution Models:");
    println!("  phase_first             Run same phase across all slots (efficient batching)");
    println!("  slot_first              Complete each slot before next (slot-focused testing)");
    println!("\nWorker Guidelines:");
    println!("  CPU-heavy tests:        {} workers (= CPU cores)", cpu_count);
    println!("  I/O-heavy tests:        {} workers (2x CPU cores)", cpu_count * 2);
    println!("  Memory-limited:         {} workers", (cpu_count / 2).max(1));
    println!("\nExamples:");
    println!("  {} --cli demo/my-test.yaml", program_name);
    println!("  {} --cli ./procedure.yaml", program_name);
}

fn print_usage(program_name: &str) {
    eprintln!("Usage: {} --cli <yaml_file>", program_name);
    eprintln!("\nExamples:");
    eprintln!("  {} --cli demo/my-test.yaml", program_name);
    eprintln!("  {} --cli ./procedure.yaml", program_name);
    eprintln!("\nRun '{} --help' for more information.", program_name);
}
