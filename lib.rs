//! Tauri application entry point - registers commands, events, and plugins.

use tauri::Manager;

pub mod cli;
pub mod editor;
pub mod execution;
pub mod features;
pub mod grpc_process;
pub mod measurements;
pub mod plugs;
pub mod procedure;
pub mod python;
pub mod reports;
pub mod validation;

pub use execution::{OrchestratorState, StandalonePlugServiceState, UIResponseState};

/// Kill all child process groups. Each child was spawned as its own process group leader
/// via `command_group`, so killing the group (-pid) kills the child and all its descendants.
/// This is a synchronous last-resort safety net for when the async cleanup didn't complete.
#[cfg(unix)]
fn kill_child_process_groups() {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let our_pid = std::process::id().to_string();
    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-P", &our_pid])
        .output()
    else {
        return;
    };
    let Ok(pids) = String::from_utf8(output.stdout) else {
        return;
    };
    for pid_str in pids.lines() {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // killpg sends signal to the entire process group
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_child_process_groups() {
    // On Windows, process cleanup is handled via Job Objects (see windows dependency)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri_specta::Builder::<tauri::Wry>::new()
        .commands(tauri_specta::collect_commands![
            // Python environment commands
            python::venv::get_python_state,
            python::venv::sync_python,
            python::venv::delete_venv,
            // Reports commands
            reports::get_report,
            reports::list_reports,
            reports::mark_report_uploaded,
            reports::mark_report_upload_failed,
            reports::list_report_attachments,
            reports::read_attachment_file,
            reports::delete_report,
            reports::open_report_dir,
            // Execution commands
            execution::commands::execute_parallel_runs,
            execution::commands::stop_execution,
            execution::commands::submit_unit_input,
            execution::commands::kill_execution,
            features::operator_ui::commands::submit_native_ui_response,
            // Procedure commands
            procedure::commands::file_exists,
            procedure::commands::load_procedure,
            procedure::commands::read_procedure_file,
            procedure::commands::write_procedure_file,
            procedure::commands::create_directory,
            procedure::commands::list_yaml_files,
            procedure::commands::list_subdirectories,
            procedure::commands::delete_directory,
            // Editor commands
            editor::commands::watch_yaml_file,
            editor::commands::unwatch_yaml_file,
            editor::commands::load_editor_settings,
            editor::commands::save_editor_settings,
            editor::commands::save_yaml_config,
            // Python file commands
            python::commands::read_python_file,
            python::commands::write_python_file,
            python::commands::analyze_python_file,
            python::commands::to_python_identifier_text,
            python::commands::compute_relative_path,
        ])
        .events(tauri_specta::collect_events![
            // Python events
            python::venv::PythonInstallOutputEvent,
            // Execution events
            execution::orchestrator::orchestrator::ExecutionCompleteEvent,
            execution::orchestrator::orchestrator::ExecutionPlanEvent,
            execution::orchestrator::orchestrator::ExecutionStatsEvent,
            execution::orchestrator::orchestrator::JobCompleteEventWrapper,
            execution::orchestrator::orchestrator::JobProgressEvent,
            execution::orchestrator::orchestrator::OrchestratorExecutionProgressEvent,
            features::operator_ui::UiRequestEvent,
            execution::events::PlugStatusUpdateEvent,
            execution::events::PlugLogEvent,
            execution::events::UiUpdateEvent,
            execution::commands::UnitInputRequestEvent,
            execution::commands::InitializationStatusEvent,
            execution::commands::ExecutionProgressEvent,
            execution::commands::ExecutionCompleteEventPayload,
            execution::commands::OrchestratorCleanupCompleteEvent,
            execution::commands::OrchestratorTeardownCompleteEvent,
            // Editor events
            editor::commands::YamlFileChangedEvent,
            editor::commands::YamlFileDeletedEvent,
        ])
        .typ::<procedure::schema::ProcedureDefinition>()
        .typ::<procedure::schema::PhaseDefinition>()
        .typ::<procedure::schema::PlugDefinition>()
        .typ::<procedure::schema::UnitConfig>()
        .typ::<procedure::schema::UnitFieldConfig>()
        .typ::<procedure::schema::PhaseStage>()
        .typ::<procedure::schema::Scope>()
        .typ::<procedure::schema::SlotConfig>()
        .typ::<procedure::schema::FirstFailureAction>()
        .typ::<procedure::schema::MeasurementSpec>()
        .typ::<procedure::schema::ValidatorSpec>()
        .typ::<procedure::schema::AggregationSpec>()
        .typ::<procedure::schema::MultiDimensionalSpec>()
        .typ::<procedure::schema::AxisSpec>()
        .typ::<procedure::schema::SelectOption>()
        .typ::<procedure::schema::TextSize>()
        .typ::<procedure::schema::FontFamily>()
        .typ::<procedure::schema::TextColor>()
        .typ::<procedure::schema::UIConfig>()
        .typ::<procedure::schema::ProcedureDefinition>()
        .typ::<procedure::error::ErrorCode>()
        .typ::<features::operator_ui::UiComponent>()
        .typ::<features::operator_ui::ComponentValue>()
        .typ::<execution::reports::DashboardInfo>()
        .typ::<execution::types::UnitInfo>()
        .typ::<validation::ValidationResult>()
        .error_handling(tauri_specta::ErrorHandlingMode::Throw);

    #[cfg(debug_assertions)]
    builder
        .export(
            specta_typescript::Typescript::default()
                .bigint(specta_typescript::BigIntExportBehavior::Number)
                .header("// @ts-nocheck"),
            "../app/lib/bindings.ts",
        )
        .expect("Failed to export typescript bindings");

    tauri::Builder::default()
        // Register Tauri plugins for filesystem, shell, dialogs, etc.
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                ])
                .level(log::LevelFilter::Debug)
                .format(move |out, message, _record| out.finish(format_args!("{}", message)))
                .build(),
        )
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_upload::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_deep_link::init())
        // Register global state accessible from commands
        .manage(OrchestratorState::default())
        // Kill all workers when window is closed
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app_handle = window.app_handle().clone();
                api.prevent_close();

                tauri::async_runtime::spawn(async move {
                    let orchestrator_state = app_handle.state::<OrchestratorState>();

                    // Graceful cleanup with timeout — if it deadlocks, we still exit
                    let cleanup = async {
                        // Cancel all pending initializations
                        let pending_inits = orchestrator_state.pending_initializations.lock().await;
                        for (exec_id, pending) in pending_inits.iter() {
                            log::info!("Cancelling pending initialization: {}", exec_id);
                            pending.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                            let mut handle_guard = pending.handle.lock().await;
                            if let Some(handle) = handle_guard.take() {
                                handle.abort();
                            }
                        }
                        drop(pending_inits);

                        // Set shutdown flags on all execution states to prevent worker replacement
                        let state_refs = orchestrator_state.state_refs.lock().await;
                        for (execution_id, state_arc) in state_refs.iter() {
                            log::info!("Setting shutdown flags for execution: {}", execution_id);
                            let mut state = state_arc.write().await;
                            state.shutdown_requested = true;
                            state.force_kill_requested = true;
                        }
                        drop(state_refs);

                        let worker_refs = orchestrator_state.worker_refs.lock().await;
                        for (execution_id, workers_arc) in worker_refs.iter() {
                            log::info!("Killing workers for execution: {}", execution_id);
                            let mut workers = workers_arc.write().await;
                            for worker in workers.iter_mut() {
                                if let Err(e) = worker.force_shutdown().await {
                                    log::warn!("Failed to kill worker: {}", e);
                                }
                            }
                            // Clear the Vec so job tasks see is_empty() and skip replacement
                            workers.clear();
                        }
                        drop(worker_refs);

                        let resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;
                        for (execution_id, rm_arc) in resource_manager_refs.iter() {
                            log::info!("Stopping plug services for execution: {}", execution_id);
                            let rm = rm_arc.read().await;
                            if let Err(e) = rm.force_destroy_all_plugs(Some(&app_handle)).await {
                                log::warn!("Failed to stop plug services: {}", e);
                            }
                        }
                        drop(resource_manager_refs);
                    };

                    if tokio::time::timeout(std::time::Duration::from_secs(5), cleanup).await.is_err() {
                        log::error!("Process cleanup timed out after 5s, forcing exit");
                    }

                    log::info!("All workers killed, exiting application");
                    app_handle.exit(0);
                });
            }
        })
        .manage(UIResponseState::default())
        .manage(StandalonePlugServiceState::default())
        // Register command handlers and mount event system
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            // Safety net: kill all child process groups on exit regardless of how we got here.
            // Covers: Cmd+Q, force quit, cleanup timeout, crash.
            // RunEvent::Exit fires after the event loop stops, right before process termination.
            if let tauri::RunEvent::Exit = event {
                kill_child_process_groups();
            }
        });
}
