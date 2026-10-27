//! Job execution and worker management
//!
//! This module handles:
//! - Spawning job execution tasks
//! - Managing worker scope during job execution
//! - Resolving the plug ports a job needs
//! - Timeout handling and worker recovery

use std::collections::HashMap;

use crate::constants::timeouts;
use crate::event_sink::ExecutionEvent;
use crate::job::{Job, JobStatus};
use crate::worker::Worker;

use super::Orchestrator;
use super::{JobCompletionEvent, JobProgress};

/// The key a phase of the sequence at `scope` uses for a sibling's
/// result: `phase_key` minus the scope prefix, when `phase_key` is a
/// direct child of that scope (`calib/check` in `calib` is `check`;
/// `calib/inner/x` is not a sibling). `None` outside a sequence.
fn sibling_local_key(scope: &[String], phase_key: &str) -> Option<String> {
    if scope.is_empty() {
        return None;
    }
    let prefix = format!("{}/", scope.join("/"));
    let local = phase_key.strip_prefix(prefix.as_str())?;
    (!local.contains('/')).then(|| local.to_string())
}

/// The phase timeout that actually applies. Debug mode returns `None`
/// (timeout disabled so a breakpoint pause isn't killed); otherwise the
/// job's own `timeout_ms` passes through. Pure so the gate is testable.
fn effective_timeout_ms(timeout_ms: Option<u64>, debug: bool) -> Option<u64> {
    timeout_ms.filter(|_| !debug)
}

/// A phase that asked for a plug it cannot reach fails in the worker with
/// "Phase <name>: required parameter '<key>' unresolved: …". Add the plug's
/// own reason (missing instance, slot plug in a shared phase) so the error
/// says why instead of only that it is missing.
fn explain_unavailable_plugs(
    result: Result<crate::job::JobResult, String>,
    unavailable: &HashMap<String, String>,
) -> Result<crate::job::JobResult, String> {
    if unavailable.is_empty() {
        return result;
    }
    let explain = |error: &mut String| {
        let mut keys: Vec<&String> = unavailable.keys().collect();
        keys.sort();
        // The worker binds a parameter to a plug case-insensitively.
        let lowered = error.to_lowercase();
        for key in keys {
            if lowered.contains(&format!("required parameter '{}'", key.to_lowercase())) {
                error.push_str(&format!("\nPlug {key}: {}", unavailable[key]));
            }
        }
    };
    match result {
        Ok(mut job_result) => {
            if let Some(error) = job_result.error.as_mut() {
                explain(error);
            }
            Ok(job_result)
        }
        Err(mut error) => {
            explain(&mut error);
            Err(error)
        }
    }
}

impl Orchestrator {
    pub(super) async fn spawn_job_execution(
        &self,
        mut job: Job,
        worker_id: usize,
        worker: Worker,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), String> {
        // Accumulate unit_info from completed phases (same slot or shared).
        // NOTE: `phase_results` is built lower down — a previous version
        // of this block also constructed it here, but the result was
        // unconditionally overwritten by the second pass that knows
        // about measurement aggregations. Wasted lock churn + a brief
        // inconsistency window between the two reads.
        {
            let state = self.state.read().await;

            // Collect completed job results relevant to this slot for unit info merging.
            // Keyed by phase_key -- keeps only the highest retry_count per phase.
            let mut unit_results_by_phase: std::collections::HashMap<
                String,
                &crate::job::JobResult,
            > = std::collections::HashMap::new();
            // Separately track the first attempt's input_unit_info per phase (lowest retry_count).
            // Used as the diff baseline so retries that set the same value as a prior attempt
            // are still detected as intentional changes.
            let mut first_input_by_phase: std::collections::HashMap<
                String,
                (usize, Option<crate::unit::UnitInfo>),
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
                    }
                }
            }

            // Merge completed phases' unit info on top of initial_unit_info:
            // only update fields that differ from the initial value (so phases
            // can't accidentally reset fields).
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
                let mut results_with_unit: Vec<(String, &crate::job::JobResult)> =
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
                        let input_operated_by = input.and_then(|u| u.operated_by.clone());
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

                                crate::unit::UnitInfo {
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
                                    operated_by: merge_field(
                                        &phase_unit.operated_by,
                                        base.operated_by,
                                        &input_operated_by,
                                    ),
                                    sub_units: merged_sub_units,
                                    status: phase_unit.status.clone(),
                                    // Operator-entered metadata rides the base
                                    // (identify-time) info; phases don't set it
                                    // via unit_json (Python metadata flows
                                    // through the JobComplete bundle instead).
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
        // that this phase is entitled to read. Without the filter, a phase
        // naming an undeclared sibling got its result or a TypeError
        // depending on scheduling.
        let visible = self
            .procedure_definition
            .visible_phase_keys(&job.phase_key, job.stage_scope);
        {
            let state = self.state.read().await;
            let mut phase_results: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            // A sibling's result under its short key. Kept apart and merged
            // after the walk: `job_info` is a HashMap, and a procedure phase
            // keyed `check` would otherwise win over the sequence's own
            // `check` on iteration order, half of the time.
            let mut sibling_results: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            // Track highest retry_count per phase_key so only the final attempt wins
            let mut phase_retry_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for (job_id, info) in &state.job_info {
                // Include results from same slot or shared phases
                if (info.slot_id == job.slot_id || info.slot_id.is_none())
                    && visible.contains(&info.phase_key)
                {
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
                            // A phase of a sequence names a sibling's result by
                            // the key the file wrote (`check`), a Python
                            // parameter; the engine knows it as `calib/check`.
                            // Hand it over under both.
                            if let Some(local) =
                                sibling_local_key(&job.sequence_path, &info.phase_key)
                            {
                                sibling_results.insert(local, json.clone());
                            }
                            phase_results.insert(info.phase_key.clone(), json);
                        }
                    }
                }
            }
            phase_results.extend(sibling_results);
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

        // Update job status
        job.status = JobStatus::Running;

        // Store job info when starting (needed for shutdown event emission)
        {
            let mut state = self.state.write().await;
            state
                .job_info
                .insert(job.id, crate::state::JobInfo::from_job(&job));
            // The sequence calls this job runs in are entered from now
            // on: their teardown runs if the scope is cancelled later.
            state.mark_sequence_entered(&job);
        }

        // Clone event_sink for use inside the spawned task
        let event_sink = self.event_sink.clone();

        // Emit job started event. Not for a forced phase: its code never
        // runs, only its result is reported.
        if self.forced_result(&job.phase_key).is_none() {
            let progress = JobProgress {
                job_id: job.id.to_string(),
                slot_id: job.slot_id.clone(),
                phase_key: job.phase_key.clone(),
                phase_name: job.phase_name.clone(),
                stage_scope: job.stage_scope,
                status: JobStatus::Running,
                worker_id: Some(worker_id),
                started_at: Some(chrono::Utc::now()),
                timeout_ms: job.timeout_ms,
                outcome: None,
                retry_count: job.retry_count,
                error: None,
            };
            event_sink.emit(&ExecutionEvent::JobProgress {
                job_id: progress.job_id,
                slot_id: progress.slot_id,
                phase_key: progress.phase_key,
                phase_name: progress.phase_name,
                stage_scope: progress.stage_scope,
                status: progress.status,
                worker_id: progress.worker_id,
                started_at: progress.started_at,
                timeout_ms: progress.timeout_ms,
                outcome: progress.outcome,
                retry_count: progress.retry_count,
                error: progress.error,
            });
        }

        // Clone what we need before spawning
        let job_id = job.id;
        let completion_tx = self.completion_tx.clone();
        let original_job = job.clone();
        let attachment_dir = self.attachment_dir.clone();
        let procedure_dir = self.procedure_dir.clone();
        let python_path = self.python_path.clone();
        let procedure_env = self.procedure_env.clone();
        let workers = self.workers.clone();
        let resource_manager = self.resource_manager.clone();
        let state = self.state.clone();

        // Get plug configurations for this job before spawning
        let plug_configs_for_job = self.get_plug_configs_for_job(&original_job);
        let forced = self.forced_result(&original_job.phase_key);

        // Capture debug flag before the spawn (self doesn't cross in).
        // Gates the timeout skip. Crash-respawned workers are NOT re-armed
        // for debug: a crash ends the debugger session, so a respawn must
        // not re-block on wait_for_client.
        let debug = self.debug_port.is_some();

        // Spawn task to execute job
        tokio::spawn(async move {
            // Held for the whole job: dropped with the task, not when
            // `spawn_job_execution` returns.
            let _permit = permit;
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

            // Spawn a warning task only if timeout is set (and not in
            // debug mode, where the timeout is disabled).
            let warning_handle =
                if let Some(timeout_ms) = effective_timeout_ms(original_job.timeout_ms, debug) {
                    let warning_time_ms = timeout_ms * timeouts::TIMEOUT_WARNING_THRESHOLD / 100;
                    let phase_name_clone = original_job.phase_name.clone();
                    let slot_id_clone = original_job.slot_id.clone();
                    let workers_for_warning = workers.clone();
                    let sink_for_warning = event_sink.clone();
                    let job_id_for_warning = original_job.id.to_string();
                    let key_for_warning = original_job.phase_key.clone();

                    Some(tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(warning_time_ms)).await;

                        // Check if orchestrator still active before warning
                        let workers_check = workers_for_warning.read().await;
                        if workers_check.is_empty() {
                            return;
                        }
                        drop(workers_check);

                        log::debug!(
                            "Phase '{}' for {} has been running for {}ms, will timeout in {}ms",
                            phase_name_clone,
                            slot_id_clone.as_deref().unwrap_or("<shared>"),
                            warning_time_ms,
                            timeout_ms - warning_time_ms
                        );
                        // The phase's own log, as the docs promise: a phase
                        // nearing its limit says so before it is cut.
                        sink_for_warning.emit(&ExecutionEvent::PhaseLogLine {
                            job_id: job_id_for_warning,
                            phase_key: key_for_warning,
                            slot_id: slot_id_clone,
                            level: "WARNING".to_string(),
                            message: format!(
                                "{}% of its {} timeout used",
                                timeouts::TIMEOUT_WARNING_THRESHOLD,
                                crate::procedure::schema::format_duration(timeout_ms)
                            ),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            file: None,
                            line: None,
                        });
                    }))
                } else {
                    None
                };

            // Forced: the phase's code does not run and its plugs were
            // not created (`schedule_available_jobs`); its result is the
            // forced one, through the same completion path as a real
            // result so the run's outcome follows it.
            if let Some(forced) = forced {
                if let Some(handle) = warning_handle {
                    handle.abort();
                }
                let mut result = crate::job::JobResult::forced(forced);
                result.retry_count = original_job.retry_count;
                let _ = completion_tx
                    .send(JobCompletionEvent {
                        job_id,
                        result: Ok(result),
                        original_job: original_job.clone(),
                        worker_id,
                    })
                    .await;
                return;
            }

            // NOTE: Slot-scope plugs will be created before first per-slot setup phase runs
            // Execution-scope plugs will be created before first shared setup phase runs

            // Resolve the ports of the plugs this job uses
            let mut unavailable_plugs: HashMap<String, String> = HashMap::new();
            let mut never_opened: std::collections::HashSet<String> = Default::default();
            let plug_ports = if !original_job.required_plugs.is_empty() {
                // Use pre-extracted plug configurations for this job
                let plug_configs = plug_configs_for_job;

                // Events now emitted at plug level in ResourceManager

                // A read guard: plug ports are resolved under inner locks,
                // and a write guard here serialized plug startup across
                // every job.
                let resource_manager_ref = resource_manager.read().await;

                // Ports of the plugs this job can reach (pass slot_id for scope
                // management). The others are only an error for a phase that
                // asks for them: their reason is added to its error below.
                let (ports, unavailable, not_opened) = resource_manager_ref
                    .resolve_plug_ports(&plug_configs, original_job.slot_id.clone())
                    .await;
                log::debug!(
                    "Started plug services for job {}: {:?}",
                    original_job.id,
                    ports
                );
                unavailable_plugs = unavailable;
                never_opened = not_opened;
                ports
            } else {
                HashMap::new()
            };
            let original_job = {
                let mut job = original_job;
                // Only a plug that did not open makes its phase SKIP; one
                // out of this phase's reach is the procedure's error.
                job.unavailable_plugs = unavailable_plugs
                    .iter()
                    .filter(|(k, _)| never_opened.contains(*k))
                    .map(|(k, why)| (k.clone(), why.clone()))
                    .collect();
                job
            };

            // Execute job with optional timeout. Debug mode disables the
            // timeout so a breakpoint pause isn't killed as a hung worker.
            let result = if let Some(timeout_ms) =
                effective_timeout_ms(original_job.timeout_ms, debug)
            {
                // With timeout
                let timeout_duration = std::time::Duration::from_millis(timeout_ms);
                let began = chrono::Utc::now();
                match tokio::time::timeout(
                    timeout_duration,
                    worker.execute_job(
                        original_job.clone(),
                        plug_ports,
                        event_sink.clone(),
                        attachment_dir.clone(),
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
                    // A shell phase's process group died with the future
                    // the timeout dropped. The Python worker sat idle and
                    // stays: killing it only cost a respawn.
                    Err(_) if original_job.runtime_type == crate::job::RuntimeType::Shell => {
                        if let Some(handle) = warning_handle {
                            handle.abort();
                        }
                        Err(format!(
                            "Timed out after {}",
                            crate::procedure::schema::format_duration(timeout_ms)
                        ))
                    }
                    Err(_) => {
                        let cut = chrono::Utc::now();
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
                            log::debug!("Failed to kill worker after timeout: {}", kill_error);
                        }

                        // Stamped now, at the cut: the kill and the
                        // replacement worker's start below are not the
                        // phase's time.
                        let mut timed_out =
                            crate::orchestrator::completion::error_handling::convert_error_to_result(
                                format!(
                                    "Timed out after {}",
                                    crate::procedure::schema::format_duration(timeout_ms)
                                ),
                                &original_job,
                                original_job.id,
                            );
                        timed_out.started_at = began;
                        timed_out.completed_at = cut;
                        let timed_out = Ok(timed_out);

                        // `shutdown()` took the pool and completed this job
                        // as interrupted: nothing left to report to.
                        {
                            let workers_check = workers.read().await;
                            if workers_check.is_empty() || worker_id >= workers_check.len() {
                                log::debug!(
                                    "Skipping timeout handling - orchestrator already shut down"
                                );
                                return;
                            }
                        }
                        // A stop is winding the loop down: no replacement,
                        // but the completion still goes out. Returning
                        // without it left the worker busy forever and the
                        // loop waiting for it.
                        if state.read().await.shutdown_requested {
                            log::debug!("Skipping worker replacement after timeout - orchestrator shutting down");
                            timed_out
                        } else {
                            let mut new_worker = Worker::new_with_python(
                                worker_id,
                                procedure_dir.clone(),
                                python_path.clone(),
                            )
                            .with_procedure_env(procedure_env.clone());
                            if let Err(start_error) = new_worker.start(&event_sink).await {
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

                            // Handled as a timeout in handle_job_completion
                            timed_out
                        }
                    }
                }
            } else {
                worker
                    .execute_job(
                        original_job.clone(),
                        plug_ports,
                        event_sink.clone(),
                        attachment_dir.clone(),
                    )
                    .await
            };

            let result = explain_unavailable_plugs(result, &unavailable_plugs);

            // Check if worker crashed (IPC error) and needs replacement
            if let Err(ref error_msg) = result {
                // Startup-stage stalls killed the wedged worker process,
                // so they need the same pool replacement as an IPC crash —
                // otherwise a retry or the next scheduled phase lands on
                // a dead worker. Markers are shared consts with the error
                // formatter in `worker/runner.rs`.
                if error_msg.contains("IPC error")
                    || error_msg.contains("Connection closed")
                    || error_msg.contains("Broken pipe")
                    || error_msg.contains(crate::worker::runner::STALL_MARKER_ACK)
                    || error_msg.contains(crate::worker::runner::STALL_MARKER_IMPORT)
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

                            log::debug!("Worker {} crashed with IPC error, replacing", worker_id);

                            // Get the worker from the array to take ownership
                            let mut crashed_worker = {
                                let mut workers_mut = workers.write().await;
                                if worker_id < workers_mut.len() {
                                    // Take the worker out temporarily (replace with a placeholder)
                                    std::mem::replace(
                                        &mut workers_mut[worker_id],
                                        Worker::new_with_python(
                                            worker_id,
                                            procedure_dir.clone(),
                                            python_path.clone(),
                                        )
                                        .with_procedure_env(procedure_env.clone()),
                                    )
                                } else {
                                    // Worker already gone, create a dummy one just to shut down
                                    Worker::new_with_python(
                                        worker_id,
                                        procedure_dir.clone(),
                                        python_path.clone(),
                                    )
                                    .with_procedure_env(procedure_env.clone())
                                }
                            };

                            let _ = crashed_worker.force_shutdown().await;

                            let mut new_worker = Worker::new_with_python(
                                worker_id,
                                procedure_dir.clone(),
                                python_path.clone(),
                            )
                            .with_procedure_env(procedure_env.clone());
                            let start_result = new_worker.start(&event_sink).await;

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

            // Send completion event. Bounded — `.await` lets
            // backpressure propagate from a lagging schedule loop
            // back into this worker task, capping memory growth.
            let _ = completion_tx
                .send(JobCompletionEvent {
                    job_id,
                    result,
                    original_job: original_job.clone(),
                    worker_id,
                })
                .await;

            // No plug cleanup for a finished job on purpose: a plug
            // outlives every phase that uses it and is destroyed at its
            // scope boundary (`destroy_*_scope_plugs`). This used to call
            // `stop_plug_services_for_slot`, which took a lock and walked
            // the allocation to move a reference count nothing read.
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
    fn no_timeout_stays_none_regardless_of_debug() {
        assert_eq!(effective_timeout_ms(None, false), None);
        assert_eq!(effective_timeout_ms(None, true), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_plug_is_explained_whatever_the_parameter_case() {
        let unavailable = HashMap::from([("PSU".to_string(), "never started".to_string())]);
        let error =
            "Phase off: required parameter 'psu' unresolved: no plug key or phase result psu \
             (plugs: none; results: none); add a plug with `key: psu` or add `depends_on: [psu]`"
                .to_string();
        let explained = explain_unavailable_plugs(Err(error), &unavailable).unwrap_err();
        assert!(
            explained.contains("\nPlug PSU: never started"),
            "{explained}"
        );
    }
}
