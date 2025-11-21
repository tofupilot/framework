//! Execution commands for test orchestration and control.

use serde::Serialize;
use specta::Type;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_specta::Event;
use tokio::sync::Mutex as TokioMutex;

use crate::execution;
use crate::execution::orchestrator::orchestrator::{ExecutionStrategy, Orchestrator};
use crate::execution::types::{validate_unit_info, PendingUnitInput, UnitInfo};
use crate::execution::OrchestratorState;
use crate::procedure::schema::UnitConfig;
use crate::validation;

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct UnitInputRequestEvent {
    pub execution_id: String,
    pub unit_config: Option<UnitConfig>,
}

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct ExecutionProgressEvent {
    pub execution_id: String,
    pub status: String,
    #[specta(skip)]
    pub stats: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct ExecutionCompleteEventPayload {
    pub execution_id: String,
    pub status: String,
    #[specta(skip)]
    pub stats: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct OrchestratorCleanupCompleteEvent {
    pub execution_id: String,
}

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct OrchestratorTeardownCompleteEvent {
    pub execution_id: String,
}

async fn emit_cancellation_cleanup(
    execution_id: &str,
    error_message: &str,
    orchestrator_state: &OrchestratorState,
    app_handle: &AppHandle,
) {
    let _ = ExecutionCompleteEventPayload {
        execution_id: execution_id.to_string(),
        status: "cancelled".to_string(),
        stats: serde_json::json!({
            "completed_jobs": 0,
            "failed_jobs": 0,
            "skipped_jobs": 0,
            "workers_active": 0,
            "workers_total": 0,
            "results": []
        }),
        error: Some(error_message.to_string()),
    }.emit(app_handle);

    let _ = OrchestratorCleanupCompleteEvent {
        execution_id: execution_id.to_string(),
    }.emit(app_handle);

    {
        let mut orchestrators = orchestrator_state.orchestrators.lock().await;
        orchestrators.remove(execution_id);
    }
    {
        let mut state_refs = orchestrator_state.state_refs.lock().await;
        state_refs.remove(execution_id);
    }
    {
        let mut worker_refs = orchestrator_state.worker_refs.lock().await;
        worker_refs.remove(execution_id);
    }
    {
        let mut resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;
        resource_manager_refs.remove(execution_id);
    }

    let _ = OrchestratorTeardownCompleteEvent {
        execution_id: execution_id.to_string(),
    }.emit(app_handle);
}

async fn resolve_execution_id(
    execution_id: &str,
    orchestrator_state: &OrchestratorState,
) -> String {
    if execution_id == "pending" {
        let pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.keys().next().cloned().unwrap_or_else(|| execution_id.to_string())
    } else {
        execution_id.to_string()
    }
}

async fn cancel_pending_unit_input(
    execution_id: &str,
    orchestrator_state: &OrchestratorState,
    app_handle: &AppHandle,
    error_message: &str,
) -> bool {
    let was_pending = {
        let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.remove(execution_id).is_some()
    };

    if was_pending {
        log::debug!("Unit input was pending - emitting cleanup events");
        emit_cancellation_cleanup(execution_id, error_message, orchestrator_state, app_handle).await;
    }

    was_pending
}

#[tauri::command]
#[specta::specta]
pub async fn execute_parallel_runs(
    app_handle: AppHandle,
    procedure_file: String,
    procedure_dir: String,
    slots: Vec<String>,
    worker_count_override: Option<u32>,
    strategy: Option<String>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<String, String> {
    let procedure_file_buf = PathBuf::from(&procedure_file);
    let procedure_dir = PathBuf::from(&procedure_dir);

    let procedure_def = crate::procedure::load_procedure_definition(&procedure_file_buf)
        .map_err(|e| format!("Failed to load procedure: {}", e))?;

    // Validate slot names
    let slot_diagnostics = validation::validate_slot_names(&slots);
    let slot_errors: Vec<_> = slot_diagnostics
        .iter()
        .filter(|d| matches!(d.severity, validation::DiagnosticSeverity::Error))
        .collect();
    if !slot_errors.is_empty() {
        let error_messages: Vec<String> = slot_errors
            .iter()
            .map(|e| format!("• {}", e.message))
            .collect();
        return Err(format!(
            "Slot validation failed:\n{}",
            error_messages.join("\n")
        ));
    }

    // Initialize report manager with UUID
    let execution_uuid = uuid::Uuid::new_v4(); // Single execution ID for all slots
    let execution_id_str = execution_uuid.to_string();

    // Determine worker count: UI override > YAML > CPU count (default)
    let worker_count = worker_count_override
        .map(|c| c as usize)
        .or_else(|| procedure_def.execution.as_ref().map(|e| e.workers))
        .unwrap_or_else(num_cpus::get);

    log::info!("Initializing {} workers for execution", worker_count);

    let mut orchestrator = Orchestrator::new(
        worker_count,
        procedure_dir.clone(),
        execution_id_str.clone(),
        execution_id_str.clone(),
        procedure_def.clone(),
    );
    orchestrator.set_app_handle(app_handle.clone());
    orchestrator.initialize().await.map_err(|e| {
        log::error!("Worker initialization failed: {}", e);
        format!("Failed to initialize workers: {}", e)
    })?;

    // Store orchestrator refs BEFORE waiting for unit input so stop/kill can find them
    let state_arc = orchestrator.state.clone();
    let workers_arc = orchestrator.workers.clone();
    let resource_manager_arc = orchestrator.resource_manager.clone();
    let orchestrator_arc = Arc::new(TokioMutex::new(orchestrator));
    {
        let mut orchestrators = orchestrator_state.orchestrators.lock().await;
        orchestrators.insert(execution_id_str.clone(), orchestrator_arc.clone());
    }
    {
        let mut state_refs = orchestrator_state.state_refs.lock().await;
        state_refs.insert(execution_id_str.clone(), state_arc.clone());
    }
    {
        let mut worker_refs = orchestrator_state.worker_refs.lock().await;
        worker_refs.insert(execution_id_str.clone(), workers_arc.clone());
    }
    {
        let mut resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;
        resource_manager_refs.insert(execution_id_str.clone(), resource_manager_arc.clone());
    }

    // Emit unit input request and wait for response
    let (unit_input_tx, unit_input_rx) = tokio::sync::oneshot::channel();
    {
        let mut pending_unit_inputs = orchestrator_state.pending_unit_inputs.lock().await;
        pending_unit_inputs.insert(
            execution_id_str.clone(),
            PendingUnitInput {
                sender: unit_input_tx,
                unit_config: procedure_def.unit.clone(),
            },
        );
    }

    let _ = UnitInputRequestEvent {
        execution_id: execution_id_str.clone(),
        unit_config: procedure_def.unit.clone(),
    }
    .emit(&app_handle);

    // Wait for unit input with timeout (5 minutes), checking for cancellation
    let unit_info_result = {
        let state_arc_for_cancel = state_arc.clone();
        let execution_id_for_cancel = execution_id_str.clone();
        let orchestrator_state_for_cancel = (*orchestrator_state).clone();

        tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(300), unit_input_rx) => {
                result
                    .map_err(|_| "Unit input timeout: no serial number received within 5 minutes".to_string())
                    .and_then(|r| r.map_err(|_| "Unit input channel closed before receiving serial number".to_string()))
            }
            _ = async {
                loop {
                    // Check if cancelled via stop/kill
                    let pending = orchestrator_state_for_cancel.pending_unit_inputs.lock().await;
                    if !pending.contains_key(&execution_id_for_cancel) {
                        break;
                    }
                    drop(pending);

                    // Check if shutdown requested
                    let state = state_arc_for_cancel.read().await;
                    if state.shutdown_requested || state.force_kill_requested {
                        break;
                    }
                    drop(state);

                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            } => {
                Err("Execution cancelled during unit identification".to_string())
            }
        }
    };

    let unit_info = match unit_info_result {
        Ok(info) => info,
        Err(e) => {
            log::debug!("Unit input cancelled or failed: {}", e);

            // Check if this was already handled by stop/kill
            let was_already_cancelled = {
                let pending = orchestrator_state.pending_unit_inputs.lock().await;
                !pending.contains_key(&execution_id_str)
            };

            if was_already_cancelled {
                // User clicked stop/kill - cleanup already handled by cancel_pending_unit_input
                // Return Ok to avoid showing error toast
                return Ok(execution_id_str);
            } else {
                // Genuine error - emit cleanup and return error
                emit_cancellation_cleanup(&execution_id_str, &e, &orchestrator_state, &app_handle).await;
                return Err(e);
            }
        }
    };

    log::trace!(
        "Received unit serial number: {}",
        unit_info.serial_number.as_deref().unwrap_or("none")
    );

    // Store unit info in orchestrator so initialize_report_managers can use it
    {
        let mut orchestrator = orchestrator_arc.lock().await;
        orchestrator.set_initial_unit_info(unit_info.clone());

        // Initialize report managers AFTER we have unit info
        if let Err(e) = orchestrator
            .initialize_report_managers(&procedure_file_buf, &slots)
            .await
        {
            log::trace!(
                "WARNING: Failed to initialize report manager: {}",
                e
            );
            // Continue execution even if report manager fails
        }

        // Determine execution strategy: UI override > YAML > default (phase_first)
        let exec_strategy = if let Some(ui_strategy) = strategy.as_deref() {
            match ui_strategy {
                "slot_first" => ExecutionStrategy::SlotFirst,
                "phase_first" => ExecutionStrategy::PhaseFirst,
                other => {
                    log::error!(
                        " Invalid UI execution strategy: {}. Using default (phase_first)",
                        other
                    );
                    ExecutionStrategy::PhaseFirst
                }
            }
        } else {
            procedure_def.execution.as_ref().map(|e| e.strategy).unwrap_or(ExecutionStrategy::PhaseFirst)
        };

        // Submit procedure with determined execution strategy and initial unit info
        orchestrator
            .submit_procedure(slots, exec_strategy, unit_info)
            .await
            .map_err(|e| format!("Failed to submit procedure: {}", e))?;
    }

    // Get initial stats after submission
    let initial_stats = {
        let orchestrator = orchestrator_arc.lock().await;
        orchestrator.get_stats().await
    };

    // Emit initial stats immediately
    let _ = ExecutionProgressEvent {
        execution_id: execution_id_str.clone(),
        status: "running".to_string(),
        stats: serde_json::to_value(&initial_stats).unwrap_or(serde_json::json!({})),
    }
    .emit(&app_handle);

    // Start execution in background and emit progress updates
    let app_handle_clone = app_handle.clone();
    let orchestrator_clone = orchestrator_arc.clone();
    let key_clone = execution_id_str.clone();
    let orchestrators_map_clone = orchestrator_state.orchestrators.clone();
    let state_refs_map_clone = orchestrator_state.state_refs.clone();
    let worker_refs_map_clone = orchestrator_state.worker_refs.clone();
    let resource_manager_refs_map_clone = orchestrator_state.resource_manager_refs.clone();

    tokio::spawn(async move {
        // Start the actual execution with app handle for UI support
        let app_handle_for_execution = Some(app_handle_clone.clone());
        let orchestrator_for_exec = orchestrator_clone.clone();

        let execution_handle = {
            // Spawn the actual execution in a separate task
            tokio::spawn(async move {
                let mut orchestrator = orchestrator_for_exec.lock().await;
                orchestrator.execute_all(app_handle_for_execution).await
            })
        };

        // Monitor progress while execution is running
        loop {
            let stats = {
                let orchestrator = orchestrator_clone.lock().await;
                orchestrator.get_stats().await
            };

            // Emit progress update
            let _ = ExecutionProgressEvent {
                execution_id: key_clone.clone(),
                status: "running".to_string(),
                stats: serde_json::to_value(&stats).unwrap_or(serde_json::json!({})),
            }
            .emit(&app_handle_clone);

            // Check if execution task is complete
            if execution_handle.is_finished() {
                log::debug!(
                    "Execution task finished for '{}'",
                    key_clone
                );
                // Shutdown workers BEFORE emitting cleanup-complete so frontend listeners receive the events
                {
                    log::debug!(
                        "Calling orchestrator.shutdown() for '{}'",
                        key_clone
                    );
                    let mut orchestrator = orchestrator_clone.lock().await;
                    let _ = orchestrator.shutdown(Some(&app_handle_clone)).await;
                    log::debug!(
                        "orchestrator.shutdown() completed for '{}'",
                        key_clone
                    );
                }

                // Get the execution result
                match execution_handle.await {
                    Ok(Ok(final_stats)) => {
                        // Get final results
                        let results = {
                            let orchestrator = orchestrator_clone.lock().await;
                            let state = orchestrator.state.read().await;
                            state.job_results.iter().map(|(id, result)| {
                                // Determine success and error based on execution context
                                let (success, error) = if let Some(ref e) = result.error {
                                    (false, Some(e.clone()))
                                } else if let Some(secs) = result.timeout_secs {
                                    (false, Some(format!("Timed out after {} seconds", secs)))
                                } else if result.is_failure() {
                                    (false, None)
                                } else {
                                    (true, None)
                                };

                                serde_json::json!({
                                    "job_id": id.to_string(),
                                    "success": success,
                                    "measurements": result.measurements,
                                    "logs": result.logs,
                                    "error": error,
                                    "duration_ms": (result.completed_at - result.started_at).num_milliseconds()
                                })
                            }).collect::<Vec<_>>()
                        };

                        let _ = ExecutionCompleteEventPayload {
                            execution_id: key_clone.clone(),
                            status: "complete".to_string(),
                            stats: serde_json::json!({
                                "stats": final_stats,
                                "results": results
                            }),
                            error: None,
                        }
                        .emit(&app_handle_clone);

                        // Emit cleanup complete to signal frontend can reset
                        log::debug!(
                            "Emitting orchestrator-cleanup-complete for '{}'",
                            key_clone
                        );
                        let _ = OrchestratorCleanupCompleteEvent {
                            execution_id: key_clone.clone(),
                        }
                        .emit(&app_handle_clone);
                    }
                    Ok(Err(e)) => {
                        // Emit execution-complete with error info
                        let _ = ExecutionCompleteEventPayload {
                            execution_id: key_clone.clone(),
                            status: "error".to_string(),
                            stats: serde_json::json!({
                                "completed_jobs": 0,
                                "failed_jobs": 0,
                                "skipped_jobs": 0,
                                "workers_active": 0,
                                "workers_total": 0,
                                "results": []
                            }),
                            error: Some(e),
                        }
                        .emit(&app_handle_clone);

                        // Emit cleanup complete so frontend can reset
                        let _ = OrchestratorCleanupCompleteEvent {
                            execution_id: key_clone.clone(),
                        }
                        .emit(&app_handle_clone);
                    }
                    Err(e) => {
                        // Emit execution-complete with panic info
                        let _ = ExecutionCompleteEventPayload {
                            execution_id: key_clone.clone(),
                            status: "panic".to_string(),
                            stats: serde_json::json!({
                                "completed_jobs": 0,
                                "failed_jobs": 0,
                                "skipped_jobs": 0,
                                "workers_active": 0,
                                "workers_total": 0,
                                "results": []
                            }),
                            error: Some(format!("Execution task panicked: {}", e)),
                        }
                        .emit(&app_handle_clone);

                        // Emit cleanup complete so frontend can reset
                        let _ = OrchestratorCleanupCompleteEvent {
                            execution_id: key_clone.clone(),
                        }
                        .emit(&app_handle_clone);
                    }
                }

                // Clean up orchestrator and all refs from maps
                {
                    let mut orchestrators = orchestrators_map_clone.lock().await;
                    orchestrators.remove(&key_clone);
                }
                {
                    let mut state_refs = state_refs_map_clone.lock().await;
                    state_refs.remove(&key_clone);
                }
                {
                    let mut worker_refs = worker_refs_map_clone.lock().await;
                    worker_refs.remove(&key_clone);
                }
                {
                    let mut resource_manager_refs = resource_manager_refs_map_clone.lock().await;
                    resource_manager_refs.remove(&key_clone);
                }
                log::debug!(
                    "Orchestrator '{}' teardown complete",
                    key_clone
                );

                let _ = OrchestratorTeardownCompleteEvent {
                    execution_id: key_clone.clone(),
                }
                .emit(&app_handle_clone);

                break;
            }

            // Small delay before next progress check
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });

    Ok(execution_id_str)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_execution(
    execution_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    log::debug!("stop_execution called for '{}'", execution_id);

    let actual_execution_id = resolve_execution_id(&execution_id, &orchestrator_state).await;

    if cancel_pending_unit_input(
        &actual_execution_id,
        &orchestrator_state,
        &app_handle,
        "Execution cancelled during unit identification",
    ).await {
        return Ok(());
    }

    // Get the state Arc directly - this doesn't require locking the orchestrator
    let state_arc = {
        let state_refs = orchestrator_state.state_refs.lock().await;
        state_refs.get(&actual_execution_id).cloned()
    };

    let Some(state_arc) = state_arc else {
        return Ok(());
    };

    // Set shutdown_requested flag immediately - this works even while execute_all is running!
    {
        let mut state = state_arc.write().await;
        if state.shutdown_requested {
            return Ok(());
        }
        state.shutdown_requested = true;
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn submit_unit_input(
    execution_id: String,
    unit_data: HashMap<String, String>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<(), String> {
    log::trace!(
        "Submitting unit info for execution '{}': {:?}",
        execution_id, unit_data
    );

    let pending_input = {
        let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.remove(&execution_id)
    };

    if let Some(pending_input) = pending_input {
        let serial_number = unit_data.get("serial_number").cloned();
        let part_number = unit_data.get("part_number").cloned();
        let revision_number = unit_data.get("revision_number").cloned();
        let batch_number = unit_data.get("batch_number").cloned();

        let unit_info = UnitInfo {
            serial_number,
            part_number,
            revision_number,
            batch_number,
            sub_units: None,
            status: "active".to_string(),
        };

        // Validate unit info against config
        validate_unit_info(&unit_info, &pending_input.unit_config)?;

        pending_input.sender.send(unit_info).map_err(|_| {
            "Failed to send unit input: execution may have been cancelled".to_string()
        })?;
        Ok(())
    } else {
        Err(format!(
            "No pending unit input request for execution '{}': may have already been submitted or timed out",
            execution_id
        ))
    }
}

#[tauri::command]
#[specta::specta]
pub async fn kill_execution(
    execution_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    log::debug!("kill_execution called for '{}'", execution_id);

    let actual_execution_id = resolve_execution_id(&execution_id, &orchestrator_state).await;

    if cancel_pending_unit_input(
        &actual_execution_id,
        &orchestrator_state,
        &app_handle,
        "Execution killed during unit identification",
    ).await {
        return Ok(());
    }

    let (state_arc, workers_arc, resource_manager_arc) = {
        let state_refs = orchestrator_state.state_refs.lock().await;
        let worker_refs = orchestrator_state.worker_refs.lock().await;
        let resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;

        let state = state_refs.get(&actual_execution_id).cloned();
        let workers = worker_refs.get(&actual_execution_id).cloned();
        let resource_manager = resource_manager_refs.get(&actual_execution_id).cloned();

        (state, workers, resource_manager)
    };

    let Some(state_arc) = state_arc else {
        log::debug!("Killing process aborted: Orchestrator state not found");
        return Ok(());
    };

    let Some(workers_arc) = workers_arc else {
        log::debug!("Killing process aborted : Workers not found");
        return Ok(());
    };

    let Some(resource_manager_arc) = resource_manager_arc else {
        log::debug!("Killing process aborted : ResourceManager not found");
        return Ok(());
    };

    {
        let state = state_arc.read().await;
        if state.force_kill_requested {
            log::debug!("Force kill already requested");
            return Ok(());
        }
    }

    execution::orchestrator::orchestrator::Orchestrator::force_kill_immediate(
        state_arc,
        workers_arc,
        resource_manager_arc,
        Some(actual_execution_id),
        Some(app_handle),
    )
    .await?;

    log::debug!("Force kill completed");

    Ok(())
}

// UI response submission moved to execution/ui/commands.rs
pub use crate::features::operator_ui::commands::{submit_native_ui_response, UiResponseValue};
