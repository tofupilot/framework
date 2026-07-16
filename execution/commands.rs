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
    pub slot_id: String,
    pub unit_config: Option<UnitConfig>,
}

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct InitializationStatusEvent {
    pub execution_id: String,
    pub status: InitializationStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InitializationStatus {
    Initializing,
    Ready,
    Failed,
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

/// Emitted when unit identification is automatically resolved via auto_identify.
/// The frontend uses this to mark unit input as complete without showing the form.
#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct UnitInputAutoEvent {
    pub execution_id: String,
    pub slot_id: String,
    pub unit_config: Option<UnitConfig>,
    pub serial_number: String,
    pub part_number: String,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    pub sub_units: Option<HashMap<String, String>>,
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
    }
    .emit(app_handle);

    let _ = OrchestratorCleanupCompleteEvent {
        execution_id: execution_id.to_string(),
    }
    .emit(app_handle);

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
    }
    .emit(app_handle);
}

async fn resolve_execution_id(
    execution_id: &str,
    orchestrator_state: &OrchestratorState,
) -> String {
    if execution_id == "pending" {
        // pending_unit_inputs outer key = execution_id
        let pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| execution_id.to_string())
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
        // Remove the entire execution entry (all slots)
        pending.remove(execution_id).is_some()
    };

    if was_pending {
        log::debug!("Unit input was pending - emitting cleanup events");
        emit_cancellation_cleanup(execution_id, error_message, orchestrator_state, app_handle)
            .await;
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
    prefill_units: Option<HashMap<String, HashMap<String, String>>>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<String, String> {
    let procedure_file_buf = PathBuf::from(&procedure_file);
    let procedure_dir_path = PathBuf::from(&procedure_dir);

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

    // Generate execution ID
    let execution_uuid = uuid::Uuid::new_v4();
    let execution_id_str = execution_uuid.to_string();

    // Determine worker count: UI override > YAML > CPU count (default)
    let worker_count = worker_count_override
        .map(|c| c as usize)
        .or_else(|| procedure_def.execution.as_ref().map(|e| e.workers))
        .unwrap_or_else(num_cpus::get);

    log::info!(
        "Starting parallel initialization for execution {}",
        execution_id_str
    );

    // === UNIT INPUT: auto_identify or interactive (per slot) ===
    let auto_identify = procedure_def
        .unit
        .as_ref()
        .map(|u| u.auto_identify)
        .unwrap_or(false);

    // Create one channel per slot
    let mut slot_txs: HashMap<String, tokio::sync::oneshot::Sender<UnitInfo>> = HashMap::new();
    let mut slot_rxs: HashMap<String, tokio::sync::oneshot::Receiver<UnitInfo>> = HashMap::new();
    for slot_id in &slots {
        let (tx, rx) = tokio::sync::oneshot::channel();
        slot_txs.insert(slot_id.clone(), tx);
        slot_rxs.insert(slot_id.clone(), rx);
    }

    if let Some(ref prefill) = prefill_units {
        // "Run again": use prefilled unit data per slot, skip interactive prompt
        for slot_id in &slots {
            let slot_data = prefill.get(slot_id).cloned().unwrap_or_default();

            let serial_number = slot_data.get("serial_number").cloned().unwrap_or_default();
            let part_number = slot_data.get("part_number").cloned().unwrap_or_default();
            let revision_number = slot_data.get("revision_number").cloned();
            let batch_number = slot_data.get("batch_number").cloned();
            let sub_units: Option<HashMap<String, String>> = {
                let map: HashMap<String, String> = slot_data
                    .iter()
                    .filter_map(|(k, v)| {
                        k.strip_prefix("sub_unit:")
                            .map(|label| (label.to_string(), v.clone()))
                    })
                    .collect();
                if map.is_empty() {
                    None
                } else {
                    Some(map)
                }
            };
            // Only keys declared in the unit config are accepted; blank
            // values mean the operator left the optional field empty.
            let metadata: Option<HashMap<String, String>> = {
                let declared = procedure_def
                    .unit
                    .as_ref()
                    .and_then(|u| u.metadata.as_ref());
                let map: HashMap<String, String> = slot_data
                    .iter()
                    .filter_map(|(k, v)| {
                        k.strip_prefix("metadata:").and_then(|md_key| {
                            let is_declared = declared.is_some_and(|md| md.contains_key(md_key));
                            let val = v.trim();
                            (is_declared && !val.is_empty())
                                .then(|| (md_key.to_string(), val.to_string()))
                        })
                    })
                    .collect();
                if map.is_empty() {
                    None
                } else {
                    Some(map)
                }
            };

            let unit_info = UnitInfo {
                serial_number: Some(serial_number.clone()),
                part_number: Some(part_number.clone()),
                revision_number: revision_number.clone(),
                batch_number: batch_number.clone(),
                sub_units: sub_units.clone(),
                status: "complete".to_string(),
                metadata,
            };

            validate_unit_info(&unit_info, &procedure_def.unit)?;

            let _ = UnitInputAutoEvent {
                execution_id: execution_id_str.clone(),
                slot_id: slot_id.clone(),
                unit_config: procedure_def.unit.clone(),
                serial_number: serial_number.clone(),
                part_number: part_number.clone(),
                revision_number: revision_number.clone(),
                batch_number: batch_number.clone(),
                sub_units: sub_units.clone(),
            }
            .emit(&app_handle);

            if let Some(tx) = slot_txs.remove(slot_id) {
                let _ = tx.send(unit_info);
            }
        }
    } else if auto_identify {
        // Build UnitInfo directly from default_value fields — no user input needed
        let unit = procedure_def
            .unit
            .as_ref()
            .expect("unit config required for auto_identify");
        let serial_number = unit
            .serial_number
            .as_ref()
            .and_then(|f| f.default_value.clone())
            .unwrap_or_default();
        let part_number = unit
            .part_number
            .as_ref()
            .and_then(|f| f.default_value.clone())
            .unwrap_or_default();
        let revision_number = unit
            .revision_number
            .as_ref()
            .and_then(|f| f.default_value.clone());
        let batch_number = unit
            .batch_number
            .as_ref()
            .and_then(|f| f.default_value.clone());
        let sub_units: Option<HashMap<String, String>> = unit
            .sub_units
            .as_ref()
            .map(|items| {
                items
                    .0
                    .iter()
                    .filter_map(|item| {
                        item.serial_number
                            .as_ref()
                            .and_then(|f| f.default_value.clone())
                            .map(|val| (item.get_key(), val))
                    })
                    .collect()
            })
            .filter(|m: &HashMap<String, String>| !m.is_empty());
        // Metadata fields with a default_value resolve like other fields;
        // default-less fields are simply absent (metadata is never required).
        let auto_metadata: Option<HashMap<String, String>> = unit
            .metadata
            .as_ref()
            .map(|md| {
                md.iter()
                    .filter_map(|(key, f)| f.default_value.clone().map(|val| (key.clone(), val)))
                    .collect()
            })
            .filter(|m: &HashMap<String, String>| !m.is_empty());

        // Emit one auto event per slot and send to all channels immediately
        for slot_id in &slots {
            let _ = UnitInputAutoEvent {
                execution_id: execution_id_str.clone(),
                slot_id: slot_id.clone(),
                unit_config: procedure_def.unit.clone(),
                serial_number: serial_number.clone(),
                part_number: part_number.clone(),
                revision_number: revision_number.clone(),
                batch_number: batch_number.clone(),
                sub_units: sub_units.clone(),
            }
            .emit(&app_handle);

            let auto_unit_info = UnitInfo {
                serial_number: Some(serial_number.clone()),
                part_number: Some(part_number.clone()),
                revision_number: revision_number.clone(),
                batch_number: batch_number.clone(),
                sub_units: sub_units.clone(),
                status: "complete".to_string(),
                metadata: auto_metadata.clone(),
            };
            // Defaults must satisfy their own constraints (pattern,
            // min/max length) — parity with the interactive path and
            // the main engine's auto_identify_unit_info.
            validate_unit_info(&auto_unit_info, &procedure_def.unit)?;
            if let Some(tx) = slot_txs.remove(slot_id) {
                let _ = tx.send(auto_unit_info);
            }
        }
    } else {
        // Interactive: create per-slot pending inputs and emit one request per slot
        {
            let mut pending_unit_inputs = orchestrator_state.pending_unit_inputs.lock().await;
            let mut slot_map: HashMap<String, PendingUnitInput> = HashMap::new();
            for (slot_id, tx) in slot_txs.drain() {
                slot_map.insert(
                    slot_id.clone(),
                    PendingUnitInput {
                        sender: tx,
                        unit_config: procedure_def.unit.clone(),
                    },
                );
            }
            pending_unit_inputs.insert(execution_id_str.clone(), slot_map);
        }

        for slot_id in &slots {
            let _ = UnitInputRequestEvent {
                execution_id: execution_id_str.clone(),
                slot_id: slot_id.clone(),
                unit_config: procedure_def.unit.clone(),
            }
            .emit(&app_handle);
        }
    }

    // Emit initialization status: initializing
    let _ = InitializationStatusEvent {
        execution_id: execution_id_str.clone(),
        status: InitializationStatus::Initializing,
        error: None,
    }
    .emit(&app_handle);

    // === START BACKGROUND INITIALIZATION ===
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let app_handle_for_init = app_handle.clone();
    let execution_id_for_init = execution_id_str.clone();
    let procedure_def_for_init = procedure_def.clone();
    let procedure_dir_for_init = procedure_dir_path.clone();

    let init_handle = tokio::spawn(async move {
        // Check cancellation before starting
        if cancelled_clone.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Initialization cancelled".to_string());
        }

        // Create orchestrator
        let mut orchestrator = Orchestrator::new(
            worker_count,
            procedure_dir_for_init.clone(),
            execution_id_for_init.clone(),
            execution_id_for_init.clone(),
            procedure_def_for_init,
        );
        orchestrator.set_app_handle(app_handle_for_init.clone());

        // Check cancellation before heavy initialization
        if cancelled_clone.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Initialization cancelled".to_string());
        }

        // Initialize workers (this runs uv sync and spawns Python processes)
        if let Err(e) = orchestrator.initialize().await {
            log::error!("Worker initialization failed: {}", e);
            return Err(format!("Failed to initialize workers: {}", e));
        }

        // Check cancellation AFTER init - if cancelled, clean up workers before returning
        if cancelled_clone.load(std::sync::atomic::Ordering::Relaxed) {
            log::debug!("Initialization cancelled after workers started, shutting down workers");
            let _ = orchestrator.shutdown(None).await;
            return Err("Initialization cancelled".to_string());
        }

        Ok(crate::execution::InitializationResult {
            orchestrator,
            python_cmd: String::new(), // Not needed, workers already started
        })
    });

    // Store pending initialization so stop/kill can cancel it
    {
        let mut pending_inits = orchestrator_state.pending_initializations.lock().await;
        pending_inits.insert(
            execution_id_str.clone(),
            crate::execution::PendingInitialization {
                handle: Arc::new(TokioMutex::new(Some(init_handle))),
                cancelled: cancelled.clone(),
            },
        );
    }

    // === WAIT FOR UNIT INPUT (with cancellation checks, per slot) ===
    // For auto_identify / prefill the channels are already filled, so receive directly.
    // For interactive: each slot's channel completes when submit_unit_input sends to it,
    // or errors if the entry is dropped (stop/kill removes the outer map entry, dropping senders).
    let channels_prefilled = prefill_units.is_some() || auto_identify;
    let unit_infos_result: Result<HashMap<String, UnitInfo>, String> = if channels_prefilled {
        // All channels already sent; collect them
        let mut collected: HashMap<String, UnitInfo> = HashMap::new();
        for (slot_id_key, rx) in slot_rxs {
            match rx.await {
                Ok(info) => {
                    collected.insert(slot_id_key, info);
                }
                Err(_) => {
                    return Err("Unit input channel closed before receiving unit info".to_string())
                }
            }
        }
        Ok(collected)
    } else {
        // Await all slot receivers concurrently with a timeout.
        // Cancellation: stop/kill removes the outer entry, dropping the senders, which causes
        // receivers to return Err. We distinguish success (Ok) from cancellation (Err).
        let slot_ids: Vec<String> = slot_rxs.keys().cloned().collect();
        let receivers: Vec<tokio::sync::oneshot::Receiver<UnitInfo>> = slot_ids
            .iter()
            .map(|k| slot_rxs.remove(k).unwrap())
            .collect();

        let collect_all = futures::future::join_all(receivers.into_iter().map(|rx| rx));

        match tokio::time::timeout(std::time::Duration::from_secs(300), collect_all).await {
            Err(_) => {
                Err("Unit input timeout: no serial number received within 5 minutes".to_string())
            }
            Ok(slot_results) => {
                let mut collected: HashMap<String, UnitInfo> = HashMap::new();
                let mut was_cancelled = false;
                for (sid, result) in slot_ids.iter().zip(slot_results.into_iter()) {
                    match result {
                        Ok(info) => {
                            collected.insert(sid.clone(), info);
                        }
                        Err(_) => {
                            was_cancelled = true;
                        }
                    }
                }
                if was_cancelled {
                    Err("Execution cancelled during unit identification".to_string())
                } else {
                    Ok(collected)
                }
            }
        }
    };

    // Handle unit input result
    let unit_infos = match unit_infos_result {
        Ok(infos) => infos,
        Err(e) => {
            log::debug!("Unit input cancelled or failed: {}", e);

            // Cancel background initialization
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);

            // Take and abort pending initialization handle
            {
                let mut pending_inits = orchestrator_state.pending_initializations.lock().await;
                if let Some(pending) = pending_inits.remove(&execution_id_str) {
                    let mut handle_guard = pending.handle.lock().await;
                    if let Some(handle) = handle_guard.take() {
                        handle.abort();
                    }
                }
            }

            // Check if this was already handled by stop/kill
            let was_already_cancelled = {
                let pending = orchestrator_state.pending_unit_inputs.lock().await;
                !pending.contains_key(&execution_id_str)
            };

            if was_already_cancelled {
                return Ok(execution_id_str);
            } else {
                emit_cancellation_cleanup(&execution_id_str, &e, &orchestrator_state, &app_handle)
                    .await;
                return Err(e);
            }
        }
    };

    log::trace!("Received unit infos for {} slots", unit_infos.len());

    // === WAIT FOR INITIALIZATION TO COMPLETE ===
    // User has submitted unit input, now we need to wait for init if not done
    // IMPORTANT: We take the handle but keep the entry in pending_inits until refs are stored.
    // This prevents a race condition where stop_execution can't find the execution.
    let init_handle = {
        let pending_inits = orchestrator_state.pending_initializations.lock().await;
        match pending_inits.get(&execution_id_str) {
            Some(pending) => {
                let mut handle_guard = pending.handle.lock().await;
                handle_guard.take() // Take handle, leave entry with None
            }
            None => None, // Entry was removed (cancelled)
        }
    };

    let init_result = match init_handle {
        Some(handle) => handle.await,
        None => {
            // Init was already cancelled
            return Err("Initialization was cancelled".to_string());
        }
    };

    // Handle initialization result and store refs ATOMICALLY
    // This ensures stop_execution can always find the orchestrator via state_refs
    let orchestrator_arc = match init_result {
        Ok(Ok(result)) => {
            let orchestrator = result.orchestrator;

            // Store refs IMMEDIATELY after init completes, before any other operation
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
                let mut resource_manager_refs =
                    orchestrator_state.resource_manager_refs.lock().await;
                resource_manager_refs
                    .insert(execution_id_str.clone(), resource_manager_arc.clone());
            }

            // Remove from pending_inits now that refs are stored
            // stop/kill can now find this execution via state_refs
            {
                let mut pending_inits = orchestrator_state.pending_initializations.lock().await;
                pending_inits.remove(&execution_id_str);
            }

            // Emit initialization status: ready
            let _ = InitializationStatusEvent {
                execution_id: execution_id_str.clone(),
                status: InitializationStatus::Ready,
                error: None,
            }
            .emit(&app_handle);

            orchestrator_arc
        }
        Ok(Err(e)) => {
            log::error!("Initialization failed: {}", e);
            // Emit initialization status: failed
            let _ = InitializationStatusEvent {
                execution_id: execution_id_str.clone(),
                status: InitializationStatus::Failed,
                error: Some(e.clone()),
            }
            .emit(&app_handle);

            // Clean up pending state
            {
                let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
                pending.remove(&execution_id_str);
            }
            {
                let mut pending_inits = orchestrator_state.pending_initializations.lock().await;
                pending_inits.remove(&execution_id_str);
            }

            emit_cancellation_cleanup(&execution_id_str, &e, &orchestrator_state, &app_handle)
                .await;
            return Err(e);
        }
        Err(e) => {
            let error_msg = format!("Initialization task panicked: {}", e);
            log::error!("{}", error_msg);
            // Emit initialization status: failed
            let _ = InitializationStatusEvent {
                execution_id: execution_id_str.clone(),
                status: InitializationStatus::Failed,
                error: Some(error_msg.clone()),
            }
            .emit(&app_handle);

            // Clean up pending state
            {
                let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
                pending.remove(&execution_id_str);
            }
            {
                let mut pending_inits = orchestrator_state.pending_initializations.lock().await;
                pending_inits.remove(&execution_id_str);
            }

            emit_cancellation_cleanup(
                &execution_id_str,
                &error_msg,
                &orchestrator_state,
                &app_handle,
            )
            .await;
            return Err(error_msg);
        }
    };

    // === SETUP PROCEDURE AND START EXECUTION ===
    {
        let mut orchestrator = orchestrator_arc.lock().await;
        orchestrator.set_initial_unit_infos(unit_infos.clone());

        // Initialize report managers AFTER we have unit infos
        if let Err(e) = orchestrator
            .initialize_report_managers(&procedure_file_buf, &slots, &unit_infos)
            .await
        {
            log::trace!("WARNING: Failed to initialize report manager: {}", e);
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
            procedure_def
                .execution
                .as_ref()
                .map(|e| e.strategy)
                .unwrap_or(ExecutionStrategy::PhaseFirst)
        };

        // Submit procedure with determined execution strategy and per-slot unit infos
        orchestrator
            .submit_procedure(slots, exec_strategy, unit_infos)
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
                log::debug!("Execution task finished for '{}'", key_clone);
                // Shutdown workers BEFORE emitting cleanup-complete so frontend listeners receive the events
                {
                    log::debug!("Calling orchestrator.shutdown() for '{}'", key_clone);
                    let mut orchestrator = orchestrator_clone.lock().await;
                    let _ = orchestrator.shutdown(Some(&app_handle_clone)).await;
                    log::debug!("orchestrator.shutdown() completed for '{}'", key_clone);
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
                        log::debug!("Emitting orchestrator-cleanup-complete for '{}'", key_clone);
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
                log::debug!("Orchestrator '{}' teardown complete", key_clone);

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

    // Cancel pending initialization if any - don't abort, let task clean up workers
    {
        let pending_inits = orchestrator_state.pending_initializations.lock().await;
        if let Some(pending) = pending_inits.get(&actual_execution_id) {
            log::debug!(
                "Cancelling pending initialization for '{}'",
                actual_execution_id
            );
            pending
                .cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);

            // Try to take the handle - if None, main flow already took it and will handle cleanup
            let handle = {
                let mut handle_guard = pending.handle.lock().await;
                handle_guard.take()
            };

            if let Some(handle) = handle {
                // Spawn cleanup task to wait for init to finish and clean up workers
                tokio::spawn(async move {
                    match handle.await {
                        Ok(Ok(result)) => {
                            // Orchestrator was created but we cancelled, shut it down
                            log::debug!("Cleaning up orchestrator after cancellation");
                            let mut orchestrator = result.orchestrator;
                            let _ = orchestrator.shutdown(None).await;
                        }
                        Ok(Err(_)) => {
                            // Init failed or was cancelled internally, cleanup already done
                        }
                        Err(_) => {
                            // Task panicked, nothing to clean up
                        }
                    }
                });
            } else {
                log::debug!("Handle already taken by main flow, cleanup will happen there");
            }
        }
    }

    if cancel_pending_unit_input(
        &actual_execution_id,
        &orchestrator_state,
        &app_handle,
        "Execution cancelled during unit identification",
    )
    .await
    {
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
    slot_id: String,
    unit_data: HashMap<String, String>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<(), String> {
    log::trace!(
        "Submitting unit info for execution '{}' slot '{}': {:?}",
        execution_id,
        slot_id,
        unit_data
    );

    // Peek the unit config WITHOUT consuming the pending input:
    // validation failures below must leave the request pending so the
    // operator can correct the form and resubmit. Removing first would
    // drop the oneshot sender and make the identify step irrecoverable.
    let unit_config = {
        let pending = orchestrator_state.pending_unit_inputs.lock().await;
        match pending
            .get(&execution_id)
            .and_then(|slot_map| slot_map.get(&slot_id))
        {
            Some(input) => input.unit_config.clone(),
            None => {
                return Err(format!(
                    "No pending unit input request for execution '{}' slot '{}': may have already been submitted or timed out",
                    execution_id, slot_id
                ))
            }
        }
    };

    {
        let serial_number = unit_data.get("serial_number").cloned();
        let part_number = unit_data.get("part_number").cloned();
        let revision_number = unit_data.get("revision_number").cloned();
        let batch_number = unit_data.get("batch_number").cloned();

        // Extract sub-units from unit_data (keys: sub_unit:Label -> serial_number)
        let sub_units = {
            let mut sub_units_map: HashMap<String, String> = HashMap::new();
            for (key, value) in &unit_data {
                if let Some(label) = key.strip_prefix("sub_unit:") {
                    sub_units_map.insert(label.to_string(), value.clone());
                }
            }
            if sub_units_map.is_empty() {
                None
            } else {
                Some(sub_units_map)
            }
        };

        // Extract metadata from unit_data (keys: metadata:<key>). Only
        // declared keys are accepted; blank values mean the operator
        // left the optional field empty.
        let metadata = {
            let declared = unit_config.as_ref().and_then(|u| u.metadata.as_ref());
            let map: HashMap<String, String> = unit_data
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix("metadata:").and_then(|md_key| {
                        let is_declared = declared.is_some_and(|md| md.contains_key(md_key));
                        let val = value.trim();
                        (is_declared && !val.is_empty())
                            .then(|| (md_key.to_string(), val.to_string()))
                    })
                })
                .collect();
            if map.is_empty() {
                None
            } else {
                Some(map)
            }
        };

        let unit_info = UnitInfo {
            serial_number,
            part_number,
            revision_number,
            batch_number,
            sub_units,
            status: "active".to_string(),
            metadata,
        };

        // Validate unit info against config BEFORE consuming the
        // pending input — a validation error keeps the request pending.
        validate_unit_info(&unit_info, &unit_config)?;

        // Now consume the pending input and send.
        let pending_input = {
            let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
            if let Some(slot_map) = pending.get_mut(&execution_id) {
                let input = slot_map.remove(&slot_id);
                // If the inner map is now empty, remove the outer entry too
                // (all slots submitted — the wait loop will see the key
                // gone and unblock)
                if slot_map.is_empty() {
                    pending.remove(&execution_id);
                }
                input
            } else {
                None
            }
        };

        let pending_input = pending_input.ok_or_else(|| {
            format!(
                "No pending unit input request for execution '{}' slot '{}': may have already been submitted or timed out",
                execution_id, slot_id
            )
        })?;

        pending_input.sender.send(unit_info).map_err(|_| {
            "Failed to send unit input: execution may have been cancelled".to_string()
        })?;
        Ok(())
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

    // Cancel pending initialization if any - don't abort, let task clean up workers
    {
        let pending_inits = orchestrator_state.pending_initializations.lock().await;
        if let Some(pending) = pending_inits.get(&actual_execution_id) {
            log::debug!(
                "Killing pending initialization for '{}'",
                actual_execution_id
            );
            pending
                .cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);

            // Try to take the handle - if None, main flow already took it and will handle cleanup
            let handle = {
                let mut handle_guard = pending.handle.lock().await;
                handle_guard.take()
            };

            if let Some(handle) = handle {
                // Spawn cleanup task to wait for init to finish and force-kill workers
                tokio::spawn(async move {
                    match handle.await {
                        Ok(Ok(result)) => {
                            // Orchestrator was created but we killed, force shutdown
                            log::debug!("Force killing orchestrator workers after kill");
                            let mut orchestrator = result.orchestrator;
                            let _ = orchestrator.shutdown(None).await;
                        }
                        Ok(Err(_)) => {
                            // Init failed or was cancelled internally, cleanup already done
                        }
                        Err(_) => {
                            // Task panicked, nothing to clean up
                        }
                    }
                });
            } else {
                log::debug!("Handle already taken by main flow, force kill will happen there");
            }
        }
    }

    if cancel_pending_unit_input(
        &actual_execution_id,
        &orchestrator_state,
        &app_handle,
        "Execution killed during unit identification",
    )
    .await
    {
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
