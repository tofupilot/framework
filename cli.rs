use crate::execution::orchestrator::orchestrator::Orchestrator;
use crate::execution::types::UnitInfo;
use std::env;
use std::path::PathBuf;

pub fn init_logger() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
}

pub fn parse_args() -> PathBuf {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        print_help(&args[0]);
        std::process::exit(1);
    }

    PathBuf::from(&args[2])
}

pub fn print_help(program_name: &str) {
    println!("TofuPilot");
    println!("\nUsage:");
    println!("  {}                       Launch GUI application", program_name);
    println!("  {} run <procedure.yaml>  Run procedure in CLI mode", program_name);
    println!("  {} help                  Show this help message", program_name);
}

pub fn run(procedure_path: PathBuf) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log::error!("Failed to create runtime: {}", e);
            std::process::exit(1);
        }
    };

    runtime.block_on(async {
        let procedure_def = match crate::procedure::load_procedure_definition(&procedure_path) {
            Ok(def) => def,
            Err(e) => {
                log::error!("Failed to load procedure: {}", e);
                return;
            }
        };

        let procedure_dir = procedure_path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();

        let worker_count = procedure_def.execution.as_ref().map(|e| e.workers).unwrap_or_else(|| {
            log::warn!("No execution section found in procedure, defaulting to {} workers", crate::procedure::schema::DEFAULT_WORKERS);
            crate::procedure::schema::DEFAULT_WORKERS
        });
        let execution_id = uuid::Uuid::new_v4().to_string();
        let run_id = uuid::Uuid::new_v4().to_string();
        let mut orchestrator = Orchestrator::new(
            worker_count,
            procedure_dir.clone(),
            execution_id.clone(),
            run_id,
            procedure_def.clone(),
        );

        if let Err(e) = orchestrator.initialize().await {
            log::error!("Failed to initialize workers: {}", e);
            let _ = orchestrator.shutdown(None).await;
            std::process::exit(1);
        }

        let slots: Vec<String> = if let Some(execution) = &procedure_def.execution {
            if execution.slots.is_empty() {
                vec!["SLOT_1".to_string()]
            } else {
                execution.slots.iter().map(|s| s.key.clone()).collect()
            }
        } else {
            vec!["SLOT_1".to_string()]
        };

        for slot in &slots {
            if slot.trim().is_empty() {
                log::error!("Slot names cannot be empty");
                let _ = orchestrator.shutdown(None).await;
                std::process::exit(1);
            }
        }

        log::info!("Workers: {} | Slots: {}", worker_count, slots.len());

        // Build unit info from YAML default values
        let unit_info = if let Some(unit_config) = &procedure_def.unit {
            // Generate sub_units dict from items
            let sub_units = if let Some(sub_units_config) = &unit_config.sub_units {
                let mut sub_units_map = std::collections::HashMap::new();
                for item in &sub_units_config.0 {
                    let default_value = item.serial_number.as_ref()
                        .and_then(|c| c.default_value.clone())
                        .unwrap_or_default();
                    sub_units_map.insert(item.get_key(), default_value);
                }
                if sub_units_map.is_empty() {
                    None
                } else {
                    Some(sub_units_map)
                }
            } else {
                None
            };

            UnitInfo {
                serial_number: unit_config.serial_number.as_ref().and_then(|c| c.default_value.clone()),
                part_number: unit_config.part_number.as_ref().and_then(|c| c.default_value.clone()),
                revision_number: unit_config.revision_number.as_ref().and_then(|c| c.default_value.clone()),
                batch_number: unit_config.batch_number.as_ref().and_then(|c| c.default_value.clone()),
                sub_units,
                status: "active".to_string(),
            }
        } else {
            UnitInfo {
                serial_number: None,
                part_number: None,
                revision_number: None,
                batch_number: None,
                sub_units: None,
                status: "active".to_string(),
            }
        };

        // Set unit info BEFORE initializing report managers (same order as GUI flow)
        orchestrator.set_initial_unit_info(unit_info.clone());

        let _ = orchestrator
            .initialize_report_managers(&procedure_path, &slots)
            .await;

        let strategy = procedure_def.execution.as_ref().map(|e| e.strategy).unwrap_or(crate::procedure::schema::ExecutionStrategy::PhaseFirst);
        if let Err(e) = orchestrator
            .submit_procedure(slots, strategy, unit_info)
            .await
        {
            log::error!("Failed to submit procedure: {}", e);
            let _ = orchestrator.shutdown(None).await;
            std::process::exit(1);
        }

        let exit_code = match orchestrator.execute_all(None).await {
            Ok(stats) => {
                let passed = stats.completed_jobs - stats.failed_jobs;
                let status = if stats.failed_jobs == 0 { "PASSED" } else { "FAILED" };

                log::info!("Result: {} ({} passed, {} failed)", status, passed, stats.failed_jobs);

                if stats.failed_jobs == 0 { 0 } else { 1 }
            }
            Err(e) => {
                log::error!("Execution failed: {}", e);
                1
            }
        };

        let _ = orchestrator.shutdown(None).await;
        std::process::exit(exit_code);
    });
}
