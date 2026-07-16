//! Job execution and worker management
//!
//! This module handles:
//! - Spawning job execution tasks
//! - Managing worker scope during job execution
//! - Resource allocation and teardown for jobs
//! - Timeout handling and worker recovery

use std::collections::HashMap;
use tauri::AppHandle;
use tauri_specta::Event;

use crate::execution::constants::timeouts;
use crate::execution::job::{Job, JobStatus};
use crate::execution::worker::Worker;
use crate::plugs::guard::ResourceManagerExt;

use super::orchestrator::Orchestrator;
use super::orchestrator::{JobCompletionEvent, JobProgress};

/// The phase timeout that actually applies. Debug mode returns `None`
/// (timeout disabled so a breakpoint pause isn't killed). Pure/testable.
fn effective_timeout_ms(timeout_ms: Option<u64>, debug: bool) -> Option<u64> {
    timeout_ms.filter(|_| !debug)
}

impl Orchestrator {
    pub(super) async fn spawn_job_execution(
        &self,
        mut job: Job,
        worker_id: usize,
        worker: Worker,
        app_handle: Option<AppHandle>,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), String> {
        // Build phase_results and accumulated unit_info from completed phases (same slot or shared)
        {
            let state = self.state.read().await;
            let mut phase_results: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            // Track highest retry_count per phase_key so only the final attempt wins
            let mut phase_retry_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();

            // Collect completed job results relevant to this slot for unit info merging.
            // Keyed by phase_key — keeps only the highest retry_count per phase.
            let mut unit_results_by_phase: std::collections::HashMap<
                String,
                &crate::execution::job::JobResult,
            > = std::collections::HashMap::new();
            // Separately track the first attempt's input_unit_info per phase (lowest retry_count).
            // Used as the diff baseline so retries that set the same value as a prior attempt
            // are still detected as intentional changes.
            let mut first_input_by_phase: std::collections::HashMap<
                String,
                (usize, Option<crate::execution::types::UnitInfo>),
            > = std::collections::HashMap::new();

            for (job_id, info) in &state.job_info {
                // Include results from same slot or shared phases
                if info.slot_id == job.slot_id || info.slot_id.is_none() {
                    if let Some(result) = state.job_results.get(job_id) {
                        // Only collect unit info from slot-specific phases.
                        // Shared phases carry an arbitrary slot's initial unit info
                        // which would incorrectly overwrite this slot's data.
                        if result.unit.is_some() && info.slot_id.is_some() {
                            let prev_count = unit_results_by_phase
                                .get(&info.phase_key)
                                .map(|r| r.retry_count)
                                .unwrap_or(0);
                            if result.retry_count >= prev_count {
                                unit_results_by_phase.insert(info.phase_key.clone(), result);
                            }
                            let prev_min = first_input_by_phase
                                .get(&info.phase_key)
                                .map(|(c, _)| *c)
                                .unwrap_or(usize::MAX);
                            if result.retry_count <= prev_min {
                                first_input_by_phase.insert(
                                    info.phase_key.clone(),
                                    (result.retry_count, result.input_unit_info.clone()),
                                );
                            }
                        }

                        // Only keep the result with the highest retry_count for each phase_key.
                        // HashMap iteration is unordered, so without this check an earlier
                        // failed attempt can overwrite the final successful one.
                        let prev = phase_retry_counts.get(&info.phase_key).copied();
                        if prev.is_some() && result.retry_count < prev.unwrap() {
                            continue;
                        }
                        phase_retry_counts.insert(info.phase_key.clone(), result.retry_count);

                        let mut data: serde_json::Map<String, serde_json::Value> = result
                            .measurements
                            .iter()
                            .map(|m| (m.name.clone(), m.value.to_raw_json()))
                            .collect();
                        data.insert(
                            "outcome".to_string(),
                            serde_json::json!(result.phase_outcome),
                        );
                        let duration_ms =
                            (result.completed_at - result.started_at).num_milliseconds();
                        data.insert("duration_ms".to_string(), serde_json::json!(duration_ms));
                        if let Ok(json) = serde_json::to_string(&data) {
                            phase_results.insert(info.phase_key.clone(), json);
                        }
                    }
                }
            }
            job.phase_results = phase_results;

            // Merge completed phases' unit info on top of initial_unit_info
            // Same logic as reports/mod.rs finalize_report: only update fields that
            // differ from the initial value (so phases can't accidentally reset fields)
            let initial = if let Some(slot_id) = &job.slot_id {
                self.initial_unit_infos
                    .get(slot_id.as_str())
                    .or_else(|| self.initial_unit_infos.values().next())
                    .cloned()
            } else {
                self.initial_unit_infos.values().next().cloned()
            };
            let mut merged = initial.clone();

            if !unit_results_by_phase.is_empty() {
                let mut results_with_unit: Vec<(String, &crate::execution::job::JobResult)> =
                    unit_results_by_phase.into_iter().collect();
                results_with_unit.sort_by_key(|(_, r)| r.started_at);

                for (phase_key, result) in results_with_unit {
                    if let Some(phase_unit) = &result.unit {
                        // Compare against the FIRST attempt's input, not the final retry's input.
                        // This ensures that when a retry sets the same value as a previous attempt,
                        // it's still detected as a change relative to what the phase originally received.
                        let first_input = first_input_by_phase
                            .get(&phase_key)
                            .and_then(|(_, inp)| inp.as_ref());
                        let input = first_input.or(result.input_unit_info.as_ref());
                        let input_serial = input.and_then(|u| u.serial_number.clone());
                        let input_part = input.and_then(|u| u.part_number.clone());
                        let input_revision = input.and_then(|u| u.revision_number.clone());
                        let input_batch = input.and_then(|u| u.batch_number.clone());
                        let input_sub_units =
                            input.and_then(|u| u.sub_units.clone()).unwrap_or_default();

                        merged = Some(match merged {
                            Some(base) => {
                                let merged_sub_units =
                                    match (base.sub_units, phase_unit.sub_units.clone()) {
                                        (Some(mut base_subs), Some(phase_subs)) => {
                                            for (key, value) in phase_subs {
                                                if input_sub_units.get(&key) != Some(&value) {
                                                    base_subs.insert(key, value);
                                                }
                                            }
                                            Some(base_subs)
                                        }
                                        (Some(base_subs), None) => Some(base_subs),
                                        (None, Some(phase_subs)) => {
                                            let filtered: std::collections::HashMap<
                                                String,
                                                String,
                                            > = phase_subs
                                                .into_iter()
                                                .filter(|(k, v)| input_sub_units.get(k) != Some(v))
                                                .collect();
                                            if filtered.is_empty() {
                                                None
                                            } else {
                                                Some(filtered)
                                            }
                                        }
                                        (None, None) => None,
                                    };

                                let merge_field =
                                    |phase_val: &Option<String>,
                                     base_val: Option<String>,
                                     input_val: &Option<String>|
                                     -> Option<String> {
                                        match phase_val {
                                            Some(v) if phase_val != input_val => Some(v.clone()),
                                            _ => base_val,
                                        }
                                    };

                                crate::execution::types::UnitInfo {
                                    serial_number: merge_field(
                                        &phase_unit.serial_number,
                                        base.serial_number,
                                        &input_serial,
                                    ),
                                    part_number: merge_field(
                                        &phase_unit.part_number,
                                        base.part_number,
                                        &input_part,
                                    ),
                                    revision_number: merge_field(
                                        &phase_unit.revision_number,
                                        base.revision_number,
                                        &input_revision,
                                    ),
                                    batch_number: merge_field(
                                        &phase_unit.batch_number,
                                        base.batch_number,
                                        &input_batch,
                                    ),
                                    sub_units: merged_sub_units,
                                    status: phase_unit.status.clone(),
                                    // Operator-entered metadata rides the base
                                    // (identify-time) info; phases don't set it
                                    // via unit_json (Python metadata flows
                                    // through unit_metadata_json instead).
                                    metadata: phase_unit.metadata.clone().or(base.metadata),
                                }
                            }
                            None => phase_unit.clone(),
                        });
                    }
                }
            }

            job.initial_unit_info = merged;
        }

        // Build phase_results from completed phases (same slot or shared)
        {
            let state = self.state.read().await;
            let mut phase_results: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            // Track highest retry_count per phase_key so only the final attempt wins
            let mut phase_retry_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for (job_id, info) in &state.job_info {
                // Include results from same slot or shared phases
                if info.slot_id == job.slot_id || info.slot_id.is_none() {
                    if let Some(result) = state.job_results.get(job_id) {
                        // Only keep the result with the highest retry_count for each phase_key.
                        // The previous Outcome::Retry filter missed cases where a failed
                        // measurement validation (Outcome::Fail) triggered a retry.
                        let prev = phase_retry_counts.get(&info.phase_key).copied();
                        if prev.is_some() && result.retry_count < prev.unwrap() {
                            continue;
                        }
                        phase_retry_counts.insert(info.phase_key.clone(), result.retry_count);

                        let mut data: serde_json::Map<String, serde_json::Value> = result
                            .measurements
                            .iter()
                            .map(|m| {
                                if let Some(ref aggregations) = m.aggregations {
                                    let agg_json =
                                        serde_json::to_value(aggregations).unwrap_or_default();
                                    (
                                        m.name.clone(),
                                        serde_json::json!({
                                            "__value__": m.value.to_raw_json(),
                                            "__aggregations__": agg_json,
                                        }),
                                    )
                                } else {
                                    (m.name.clone(), m.value.to_raw_json())
                                }
                            })
                            .collect();
                        data.insert(
                            "outcome".to_string(),
                            serde_json::json!(result.phase_outcome),
                        );
                        let duration_ms =
                            (result.completed_at - result.started_at).num_milliseconds();
                        data.insert("duration_ms".to_string(), serde_json::json!(duration_ms));
                        if let Ok(json) = serde_json::to_string(&data) {
                            phase_results.insert(info.phase_key.clone(), json);
                        }
                    }
                }
            }
            job.phase_results = phase_results;
        }

        // Track phase in display systems
        {
            let state = self.state.read().await;
            let mut phase_slots: Vec<String> = Vec::new();

            // Check already tracked jobs for this phase
            for info in state.job_info.values() {
                if info.phase_key == job.phase_key {
                    let slot = info
                        .slot_id
                        .clone()
                        .unwrap_or_else(|| "<shared>".to_string());
                    if !phase_slots.contains(&slot) {
                        phase_slots.push(slot);
                    }
                }
            }

            // Check queued jobs for this phase
            for queued_job in &state.job_queue {
                if queued_job.phase_key == job.phase_key {
                    let slot = queued_job
                        .slot_id
                        .clone()
                        .unwrap_or_else(|| "<shared>".to_string());
                    if !phase_slots.contains(&slot) {
                        phase_slots.push(slot);
                    }
                }
            }

            // Add current job's slot
            let current_slot = job
                .slot_id
                .clone()
                .unwrap_or_else(|| "<shared>".to_string());
            if !phase_slots.contains(&current_slot) {
                phase_slots.push(current_slot);
            }
        }

        // Allocate resources with RAII guard
        let resource_guard = if !job.required_plugs.is_empty() {
            Some(
                self.resource_manager
                    .allocate_with_guard(job.id, &job.required_plugs)
                    .await?,
            )
        } else {
            None
        };

        // Update job status
        job.status = JobStatus::Running;

        // Store job info when starting (needed for shutdown event emission)
        {
            let mut state = self.state.write().await;
            state
                .job_info
                .insert(job.id, crate::execution::state::JobInfo::from_job(&job));
        }

        // Emit job started event
        if let Some(ref app) = app_handle {
            let progress = JobProgress {
                job_id: job.id.to_string(),
                slot_id: job.slot_id.clone(),
                phase_key: job.phase_key.clone(),
                phase_name: job.phase_name.clone(),
                stage_scope: job.stage_scope.clone(),
                status: JobStatus::Running,
                worker_id: Some(worker_id),
                started_at: Some(chrono::Utc::now()),
                timeout_ms: job.timeout_ms,
                outcome: None,
                retry_count: job.retry_count,
                error: None,
            };
            let _ = super::orchestrator::JobProgressEvent(progress).emit(app);
        }

        // Clone what we need before spawning
        let job_id = job.id;
        let completion_tx = self.completion_tx.clone();
        let original_job = job.clone();
        let report_managers = self.report_managers.clone();
        let procedure_dir = self.procedure_dir.clone();
        let workers = self.workers.clone();
        let resource_manager = self.resource_manager.clone();
        let state = self.state.clone();

        // Get plug configurations for this job before spawning
        let plug_configs_for_job = self.get_plug_configs_for_job(&original_job);

        // Get all plug configs for potential slot creation
        let _all_plug_configs = self.get_all_plug_configs(&self.procedure_definition);

        // Capture debug flag before the spawn (self doesn't cross into the task).
        let debug = self.debug;

        // Spawn task to execute job
        tokio::spawn(async move {
            // Check if workers still exist (orchestrator not shut down)
            {
                let workers_check = workers.read().await;
                if workers_check.is_empty() {
                    log::debug!("Skipping job execution - orchestrator already shut down");
                    return;
                }
            }

            // Debug-only phase start logging
            {
                let timeout_msg = match original_job.timeout_ms {
                    Some(ms) => format!("timeout: {}ms", ms),
                    None => "no timeout".to_string(),
                };
                log::debug!(
                    "Starting phase '{}' for {} ({})",
                    original_job.phase_name,
                    original_job.slot_id.as_deref().unwrap_or("<shared>"),
                    timeout_msg
                );
            }

            // Spawn a warning task only if the timeout actually applies
            // (debug mode disables it, matching the timeout wrapper).
            let warning_handle =
                if let Some(timeout_ms) = effective_timeout_ms(original_job.timeout_ms, debug) {
                    let warning_time_ms = timeout_ms * timeouts::TIMEOUT_WARNING_THRESHOLD / 100;
                    let phase_name_clone = original_job.phase_name.clone();
                    let slot_id_clone = original_job.slot_id.clone();
                    let workers_for_warning = workers.clone();

                    Some(tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(warning_time_ms)).await;

                        // Check if orchestrator still active before warning
                        let workers_check = workers_for_warning.read().await;
                        if workers_check.is_empty() {
                            return;
                        }
                        drop(workers_check);

                        log::warn!(
                            "Phase '{}' for {} has been running for {}ms, will timeout in {}ms",
                            phase_name_clone,
                            slot_id_clone.as_deref().unwrap_or("<shared>"),
                            warning_time_ms,
                            timeout_ms - warning_time_ms
                        );
                    }))
                } else {
                    None
                };

            // Get the report managers for recording phase results
            // For shared phases: record in ALL slot report managers
            // For slot phases: record in the specific slot's report manager
            let report_managers_for_job = {
                let managers = report_managers.read().await;
                if let Some(slot_id) = &original_job.slot_id {
                    // Slot-specific phase: use only this slot's manager
                    if managers.contains_key(slot_id) {
                        Some(report_managers.clone())
                    } else {
                        None
                    }
                } else {
                    // Shared phase: record in all slot managers (pass all managers)
                    Some(report_managers.clone())
                }
            };

            // NOTE: Each-slot plugs will be created before first each-slot setup phase runs
            // All-slots plugs will be created before first all-slots setup phase runs

            // Allocate resources and start plug services for this job
            let plug_ports = if !original_job.required_plugs.is_empty() {
                // Use pre-extracted plug configurations for this job
                let plug_configs = plug_configs_for_job;

                // Events now emitted at plug level in ResourceManager

                let resource_manager_ref = resource_manager.write().await;
                // Allocate basic resources
                let _allocation = match resource_manager_ref
                    .allocate_resources(original_job.id, &original_job.required_plugs)
                    .await
                {
                    Ok(allocation) => allocation,
                    Err(e) => {
                        log::warn!("Failed to allocate resources: {}", e);
                        let _ = completion_tx.send(JobCompletionEvent {
                            job_id,
                            result: Err(format!("Failed to allocate resources: {}", e)),
                            original_job: original_job.clone(),
                            worker_id,
                        });
                        return;
                    }
                };

                // Start plug services and get ports (pass slot_id for scope management)
                match resource_manager_ref
                    .start_plug_services_for_slot(
                        original_job.id,
                        &plug_configs,
                        original_job.slot_id.clone(),
                    )
                    .await
                {
                    Ok(ports) => {
                        log::debug!(
                            "Started plug services for job {}: {:?}",
                            original_job.id,
                            ports
                        );

                        // Ready events now emitted at plug level in ResourceManager

                        ports
                    }
                    Err(e) => {
                        log::warn!("Failed to start plug services: {}", e);
                        HashMap::new()
                    }
                }
            } else {
                HashMap::new()
            };

            // Execute job with optional timeout. Debug mode disables the
            // timeout so a breakpoint pause isn't killed as a hung worker.
            let result = if let Some(timeout_ms) =
                effective_timeout_ms(original_job.timeout_ms, debug)
            {
                // With timeout
                let timeout_duration = std::time::Duration::from_millis(timeout_ms);
                match tokio::time::timeout(
                    timeout_duration,
                    worker.execute_job(
                        original_job.clone(),
                        plug_ports,
                        app_handle.clone(),
                        report_managers_for_job,
                    ),
                )
                .await
                {
                    Ok(exec_result) => {
                        if let Some(handle) = warning_handle {
                            handle.abort();
                        }
                        exec_result
                    }
                    Err(_) => {
                        if let Some(handle) = warning_handle {
                            handle.abort();
                        }

                        // Check if orchestrator still active before handling timeout
                        {
                            let workers_check = workers.read().await;
                            if workers_check.is_empty() {
                                log::debug!(
                                    "Skipping timeout handling - orchestrator already shut down"
                                );
                                return;
                            }
                        }

                        log::info!(
                            "Phase '{}' for {} timed out after {}ms - killing worker",
                            original_job.phase_name,
                            original_job.slot_id.as_deref().unwrap_or("<shared>"),
                            timeout_ms
                        );

                        // Kill the worker process - it's stuck in the phase execution
                        // We need to kill it because it won't see the interrupt while executing
                        let mut worker_mut = worker;
                        if let Err(kill_error) = worker_mut.shutdown_with_timeout(500).await {
                            log::warn!("Failed to kill worker after timeout: {}", kill_error);
                        }

                        // Check if orchestrator is still active before creating replacement worker
                        {
                            let workers_check = workers.read().await;
                            let state_check = state.read().await;
                            if workers_check.is_empty()
                                || worker_id >= workers_check.len()
                                || state_check.shutdown_requested
                            {
                                log::debug!("Skipping worker replacement after timeout - orchestrator shutting down or already shut down");
                                return;
                            }
                        }

                        let mut new_worker = Worker::new(worker_id, procedure_dir.clone());
                        if let Err(start_error) = new_worker.start(app_handle.as_ref()).await {
                            log::debug!(
                                "Failed to start replacement worker {}: {}",
                                worker_id,
                                start_error
                            );
                        }

                        // Replace the dead worker with a fresh one
                        {
                            let mut workers_mut = workers.write().await;
                            if worker_id < workers_mut.len() {
                                workers_mut[worker_id] = new_worker;
                            } else {
                                log::debug!(
                                    "Cannot replace worker {} - orchestrator already shut down (workers.len() = {})",
                                    worker_id, workers_mut.len()
                                );
                                return;
                            }
                        }

                        log::info!(
                            "Created and started new worker {} to replace timed-out worker",
                            worker_id
                        );

                        // Return a timeout error that will be properly handled in handle_job_completion
                        Err(format!("Phase timed out after {} milliseconds", timeout_ms))
                    }
                }
            } else {
                worker
                    .execute_job(
                        original_job.clone(),
                        plug_ports,
                        app_handle.clone(),
                        report_managers_for_job,
                    )
                    .await
            };

            // Check if worker crashed (IPC error) and needs replacement
            if let Err(ref error_msg) = result {
                if error_msg.contains("IPC error")
                    || error_msg.contains("Connection closed")
                    || error_msg.contains("Broken pipe")
                {
                    // Check if orchestrator is still active before attempting replacement
                    {
                        let workers_check = workers.read().await;
                        let state_check = state.read().await;
                        if workers_check.is_empty()
                            || worker_id >= workers_check.len()
                            || state_check.shutdown_requested
                        {
                            log::debug!(
                                "Skipping worker replacement for crashed worker {} - orchestrator shutting down or already shut down",
                                worker_id
                            );
                        } else {
                            drop(workers_check);

                            log::warn!("Worker {} crashed with IPC error, replacing...", worker_id);

                            // Get the worker from the array to take ownership
                            let mut crashed_worker = {
                                let mut workers_mut = workers.write().await;
                                if worker_id < workers_mut.len() {
                                    // Take the worker out temporarily (replace with a placeholder)
                                    std::mem::replace(
                                        &mut workers_mut[worker_id],
                                        Worker::new(worker_id, procedure_dir.clone()),
                                    )
                                } else {
                                    // Worker already gone, create a dummy one just to shut down
                                    Worker::new(worker_id, procedure_dir.clone())
                                }
                            };

                            let _ = crashed_worker.force_shutdown().await;

                            let mut new_worker = Worker::new(worker_id, procedure_dir.clone());
                            let start_result = new_worker.start(app_handle.as_ref()).await;

                            if let Err(start_error) = start_result {
                                log::debug!(
                                    "Failed to start replacement worker {}: {}",
                                    worker_id,
                                    start_error
                                );
                            } else {
                                // Replace the worker in the shared state
                                let mut workers_mut = workers.write().await;
                                if worker_id < workers_mut.len() {
                                    workers_mut[worker_id] = new_worker;
                                } else {
                                    log::debug!(
                                        "Cannot replace worker {} - orchestrator already shut down (workers.len() = {})",
                                        worker_id, workers_mut.len()
                                    );
                                }

                                log::info!(
                                    "Created and started new worker {} to replace crashed worker",
                                    worker_id
                                );
                            }
                        }
                    }
                }
            }

            // Send completion event
            let _ = completion_tx.send(JobCompletionEvent {
                job_id,
                result,
                original_job: original_job.clone(),
                worker_id,
            });

            // Clean up plug services for this job
            if !original_job.required_plugs.is_empty() {
                // Events now emitted at plug level in ResourceManager

                let resource_manager_ref = resource_manager.write().await;

                if let Err(e) = resource_manager_ref
                    .stop_plug_services_for_slot(original_job.id, original_job.slot_id.clone())
                    .await
                {
                    log::warn!(
                        "Failed to stop plug services for job {}: {}",
                        original_job.id,
                        e
                    );
                }

                // Events now emitted at plug level in ResourceManager
            }

            // Resources are automatically released when resource_guard is dropped
            drop(resource_guard);

            // Permit is automatically returned when dropped
        });

        Ok(())
    }
}

#[cfg(test)]
mod debug_timeout_tests {
    use super::effective_timeout_ms;

    #[test]
    fn normal_run_keeps_timeout() {
        assert_eq!(effective_timeout_ms(Some(5000), false), Some(5000));
    }

    #[test]
    fn debug_run_disables_timeout() {
        assert_eq!(effective_timeout_ms(Some(5000), true), None);
    }

    #[test]
    fn no_timeout_stays_none() {
        assert_eq!(effective_timeout_ms(None, true), None);
    }
}
