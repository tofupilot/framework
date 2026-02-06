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

impl Orchestrator {
    pub(super) async fn spawn_job_execution(
        &self,
        mut job: Job,
        worker_id: usize,
        worker: Worker,
        app_handle: Option<AppHandle>,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), String> {
        // Set initial unit info on job for Python access
        job.initial_unit_info = self.initial_unit_info.clone();

        // Build phase_results from completed phases (same slot or shared)
        {
            let state = self.state.read().await;
            let mut phase_results: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (job_id, info) in &state.job_info {
                // Include results from same slot or shared phases
                if info.slot_id == job.slot_id || info.slot_id.is_none() {
                    if let Some(result) = state.job_results.get(job_id) {
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
        }

        // Track phase in display systems
        {
            let state = self.state.read().await;
            let mut phase_slots: Vec<String> = Vec::new();

            // Check already tracked jobs for this phase
            for info in state.job_info.values() {
                if info.phase_key == job.phase_key {
                    let slot = info.slot_id.clone().unwrap_or_else(|| "<shared>".to_string());
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
            state.job_info.insert(job.id, crate::execution::state::JobInfo::from_job(&job));
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

            // Spawn a warning task only if timeout is set
            let warning_handle = if let Some(timeout_ms) = original_job.timeout_ms {
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
                            original_job.id, ports
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

            // Execute job with optional timeout
            let result = if let Some(timeout_ms) = original_job.timeout_ms {
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
                                log::debug!("Skipping timeout handling - orchestrator already shut down");
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
                            log::warn!(
                                "Failed to kill worker after timeout: {}",
                                kill_error
                            );
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
                                worker_id, start_error
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

                            log::warn!(
                                "Worker {} crashed with IPC error, replacing...",
                                worker_id
                            );

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
                                    worker_id, start_error
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
                        original_job.id, e
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
