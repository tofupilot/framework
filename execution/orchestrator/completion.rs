//! Job completion handling and result processing
//!
//! This module handles:
//! - Processing job completion events
//! - Determining job success/failure outcomes
//! - Managing phase result types (Error, Timeout, Repeat, Stop, etc.)
//! - Coordinating teardown after phase failures
//! - Emitting completion events and statistics

use tauri::{AppHandle, Emitter};

use crate::execution::job::{JobResult, JobStatus, Outcome, PhaseResult};
use crate::execution::LogEntry;
use crate::schema::procedure::{PhaseNextAction, StageScope};

use super::JobCompletionEvent;
use super::Orchestrator;

impl Orchestrator {
    /// Handle job completion. Returns true if execution should continue, false if it should stop.
    pub(super) async fn handle_job_completion(
        &self,
        event: JobCompletionEvent,
        app_handle: Option<AppHandle>,
    ) -> bool {
        crate::cli_output::debug(format!(
            "Handling job completion for {}",
            event.original_job.phase_name
        ));

        let job_result = match event.result {
            Ok(result) => result,
            Err(e) => {
                // Convert external errors to Error PhaseResult
                println!("Job {} failed: {}", event.job_id, e);

                // Create a proper log entry for the error
                let mut error_logs = vec![];

                // Check if this is a timeout error and create appropriate log
                if e.contains("timed out") || e.contains("timeout") {
                    error_logs.push(LogEntry {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        level: "ERROR".to_string(),
                        message: e.clone(),
                        file: None,
                        line: None,
                    });

                    // Parse timeout duration if possible
                    let timeout_ms = e
                        .split_whitespace()
                        .filter_map(|s| s.parse::<u64>().ok())
                        .next()
                        .or(event.original_job.timeout_ms)
                        .unwrap_or(0);

                    {
                        let mut result = JobResult::new_timeout(timeout_ms);
                        result.logs = error_logs;
                        result
                    }
                } else {
                    error_logs.push(LogEntry {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        level: "ERROR".to_string(),
                        message: e.clone(),
                        file: None,
                        line: None,
                    });

                    {
                        let mut result = JobResult::new_error(e);
                        result.logs = error_logs;
                        result
                    }
                }
            }
        };

        // Check if this is a retry that will exceed the limit
        let is_retry_limit_exceeded =
            job_result.phase_result == PhaseResult::Retry && !event.original_job.can_retry();

        // Compute Outcome from execution context
        crate::cli_output::debug(format!(
            "Phase '{}' has {} measurements to validate",
            event.original_job.phase_name,
            job_result.measurements.len()
        ));

        for measurement in &job_result.measurements {
            if let Some(validators) = &measurement.validators {
                crate::cli_output::debug(format!(
                    "  Measurement '{}': {} validators",
                    measurement.name,
                    validators.len()
                ));
                for validator in validators {
                    crate::cli_output::debug(format!(
                        "    Validator operator={:?}, outcome={:?}",
                        validator.operator, validator.outcome
                    ));
                }
            }
        }

        let measurements_pass =
            crate::measurements::evaluation::check_all_measurements_pass(&job_result.measurements);

        crate::cli_output::debug(format!(
            "Phase '{}' measurements_pass = {}",
            event.original_job.phase_name, measurements_pass
        ));

        if !measurements_pass {
            crate::cli_output::warning(format!(
                "Phase '{}' measurements failed critical validation",
                event.original_job.phase_name
            ));
        }

        // Read shutdown_requested from state
        let shutdown_requested = {
            let state = self.state.read().await;
            state.shutdown_requested
        };

        let mut phase_outcome = Outcome::from_execution(
            &job_result.phase_result,
            job_result.timeout_secs,
            job_result.error.as_ref(),
            measurements_pass,
            shutdown_requested,
        );

        // Handle retry limit exceeded as error
        if is_retry_limit_exceeded {
            phase_outcome = Outcome::Error;
        }

        // Get phase definition to check for when config
        let phase_def = self.procedure_definition.as_ref().and_then(|proc_def| {
            let all_phases = proc_def.get_all_phases_with_stage_scope();
            let found = all_phases
                .iter()
                .find(|(stage, phase)| {
                    *stage == event.original_job.stage_scope
                        && phase.get_display_name() == event.original_job.phase_name
                })
                .map(|(_, phase)| *phase);

            found
        });

        // Check for then.timeout next action if this was a timeout
        let _timeout_next_action = if job_result.timeout_secs.is_some() {
            phase_def
                .and_then(|def| def.then.as_ref())
                .and_then(|then| then.timeout.clone())
        } else {
            None
        };

        // Determine job status for orchestrator
        let job_status = JobStatus::Completed;

        // Prepare error message if applicable
        let error_message = if is_retry_limit_exceeded {
            Some(format!(
                "Phase exceeded retry limit ({} retries)",
                crate::execution::constants::limits::DEFAULT_RETRY_LIMIT
            ))
        } else if let Some(ref e) = job_result.error {
            Some(e.clone())
        } else {
            job_result
                .timeout_secs
                .map(|secs| format!("Phase timed out after {} seconds", secs))
        };

        // Log resource metrics if available
        if let Some(ref metrics) = job_result.resource_metrics {
            crate::cli_output::debug(format!(
                "Resource usage for '{}': CPU: {:.1}%, Memory: {:.1}MB peak, {:.1}MB avg, Processes: {}",
                event.original_job.phase_name,
                metrics.cpu_usage_percent,
                metrics.memory_peak_mb,
                metrics.memory_avg_mb,
                metrics.process_count
            ));
        }

        // Emit job completion progress using unified helper
        if let Some(ref app) = app_handle {
            self.emit_job_progress(
                app,
                event.job_id.to_string(),
                &event.original_job,
                job_status,
                Some(phase_outcome),
                error_message.clone(),
                Some(event.worker_id),
            );

            // Get attachments for this job from the appropriate report manager(s)
            let attachments = {
                let report_managers_lock = self.report_managers.read().await;
                if let Some(slot_id) = &event.original_job.slot_id {
                    // Slot-specific phase: get attachments from the slot's manager
                    if let Some(manager) = report_managers_lock.get(slot_id) {
                        manager
                            .get_job_attachments(&event.job_id)
                            .unwrap_or_else(Vec::new)
                    } else {
                        Vec::new()
                    }
                } else {
                    // Shared phase: get attachments from the first slot's manager (they should all have the same data)
                    if let Some((_, manager)) = report_managers_lock.iter().next() {
                        manager
                            .get_job_attachments(&event.job_id)
                            .unwrap_or_else(Vec::new)
                    } else {
                        Vec::new()
                    }
                }
            };

            // Emit job-complete event with outcome, action, measurements, attachments, logs, and resource metrics
            crate::cli_output::debug(format!(
                "Emitting job-complete for {}: outcome={:?}, is_retry_limit_exceeded={}",
                event.original_job.phase_name, phase_outcome, is_retry_limit_exceeded
            ));

            // Calculate duration in milliseconds
            let duration_ms = (job_result.completed_at - job_result.started_at)
                .num_milliseconds()
                .max(0) as u64;

            let job_complete_event = super::JobCompleteEvent {
                job_id: event.job_id.to_string(),
                slot_id: event.original_job.slot_id.clone(),
                phase_key: event.original_job.phase_key.clone(),
                phase_name: event.original_job.phase_name.clone(),
                stage_scope: event.original_job.stage_scope.clone(),
                outcome: phase_outcome,
                action: format!("{:?}", job_result.phase_result),
                next_action: job_result.next_action.as_ref().map(|a| format!("{:?}", a)),
                measurements: job_result.measurements.clone(),
                attachments,
                logs: job_result.logs.clone(),
                resource_metrics: job_result.resource_metrics.clone(),
                retry_count: event.original_job.retry_count,
                retry_limit: event.original_job.retry_limit,
                started_at: job_result.started_at.to_rfc3339(),
                completed_at: job_result.completed_at.to_rfc3339(),
                duration_ms,
                worker_id: event.worker_id,
                error: error_message.clone(),
            };

            let _ = app.emit("job-complete", job_complete_event);
        }

        // Destroy plugs after teardown phases complete

        // Destroy slot-level plugs after TeardownSlot completes for a slot
        if let Some(ref slot_id) = event.original_job.slot_id {
            if matches!(event.original_job.stage_scope, StageScope::TeardownEach) {
                crate::cli_output::print_section(
                    crate::cli_output::Section::Plugs,
                    format!(
                        "Destroying slot-level plugs for {} after TeardownSlot phase",
                        slot_id
                    ),
                );

                self.emit_plug_scope_event("running").await;

                let resource_manager = self.resource_manager.write().await;
                if resource_manager.has_each_scope_plugs(&slot_id).await {
                    match resource_manager
                        .destroy_each_scope_plugs(slot_id.clone(), app_handle.as_ref())
                        .await
                    {
                        Ok(_) => {
                            self.emit_plug_scope_event("pass").await;
                        }
                        Err(e) => {
                            let error_msg = format!(
                                "Failed to destroy each-scope plugs for {}: {}",
                                slot_id, e
                            );
                            crate::cli_output::warning(&error_msg);
                            self.emit_plug_scope_event("error").await;
                        }
                    }

                    // Emit execution progress after each-scope plug teardown completes
                    self.emit_stats(app_handle.as_ref()).await;
                }
            }
        }

        // Destroy all-scope plugs after TeardownAll completes
        if matches!(event.original_job.stage_scope, StageScope::TeardownAll) {
            crate::cli_output::print_section(
                crate::cli_output::Section::Plugs,
                "Destroying all-scope plugs after TeardownAll phase",
            );

            self.emit_plug_scope_event("running").await;

            let resource_manager = self.resource_manager.write().await;
            if resource_manager.has_all_scope_plugs().await {
                match resource_manager
                    .destroy_all_scope_plugs(app_handle.as_ref())
                    .await
                {
                    Ok(_) => {
                        self.emit_plug_scope_event("pass").await;
                    }
                    Err(e) => {
                        let error_msg = format!("Failed to destroy all-scope plugs: {}", e);
                        crate::cli_output::warning(&error_msg);
                        self.emit_plug_scope_event("error").await;
                    }
                }

                // Emit execution progress after all-scope plug teardown completes
                self.emit_stats(app_handle.as_ref()).await;
            }
        }

        let mut state = self.state.write().await;

        // Special handling for setup and teardown phase failures
        let phase_failed = matches!(
            phase_outcome,
            Outcome::Fail | Outcome::Error | Outcome::Timeout | Outcome::Aborted
        ) || is_retry_limit_exceeded;

        if phase_failed {
            match event.original_job.stage_scope {
                StageScope::SetupAll => {
                    // Setup procedure failure - cancel ALL remaining jobs and ensure teardown procedure runs
                    crate::cli_output::warning("Setup procedure failed: Cancelling all slots and ensuring teardown runs");
                    let cancelled_jobs = state.cancel_all_jobs("Setup procedure failed");

                    self.emit_cancelled_jobs(
                        &cancelled_jobs,
                        "Cancelled due to setup procedure failure",
                        JobStatus::Skipped,
                        Outcome::Skip,
                        app_handle.as_ref(),
                    )
                    .await;
                }
                StageScope::SetupEach => {
                    // Setup slot failure - skip to teardown slot for this slot
                    let slot_display = event.original_job.slot_id.as_deref().unwrap_or("null");
                    crate::cli_output::warning(format!(
                        "Setup slot failed for {}: Skipping to teardown slot",
                        slot_display
                    ));
                    let cancelled_jobs = if let Some(ref slot_id) = event.original_job.slot_id {
                        state.cancel_slot_jobs(slot_id)
                    } else {
                        Vec::new()
                    };

                    self.emit_cancelled_jobs(
                        &cancelled_jobs,
                        "Cancelled due to setup slot failure",
                        JobStatus::Skipped,
                        Outcome::Skip,
                        app_handle.as_ref(),
                    )
                    .await;
                }
                _ => {
                    // Handle other stage/scope combinations normally (existing logic below)
                }
            }
        }

        // Determine next action using is_terminal check (like OpenHTF)
        crate::cli_output::debug(format!(
            "Phase '{}': phase_result={:?}, phase_outcome={:?}",
            event.original_job.phase_name, job_result.phase_result, phase_outcome
        ));

        // Check if this is a terminal result (exception, timeout, or STOP)
        let is_terminal = job_result.error.is_some()
            || job_result.timeout_secs.is_some()
            || matches!(job_result.phase_result, PhaseResult::Stop);

        let next_action = if matches!(job_result.phase_result, PhaseResult::Retry) {
            // PhaseResult::Retry should automatically trigger retry
            PhaseNextAction::Retry
        } else if is_terminal {
            // Terminal results default to Stop, but then.{outcome} can override
            if let Some(def) = phase_def {
                self.get_next_action_for_terminal(&phase_outcome, def)
            } else {
                PhaseNextAction::Stop
            }
        } else if let Some(def) = phase_def {
            // Non-terminal: check then config or use outcome-based defaults
            self.get_next_action_for_non_terminal(&phase_outcome, def)
        } else {
            // No phase definition: use outcome-based defaults
            match phase_outcome {
                Outcome::Pass
                | Outcome::Skip
                | Outcome::Fail
                | Outcome::Timeout
                | Outcome::Aborted => PhaseNextAction::Continue,
                Outcome::Error => PhaseNextAction::Stop,
            }
        };

        crate::cli_output::warning(format!(
            "Phase '{}': is_terminal={}, Computed next_action={:?}",
            event.original_job.phase_name, is_terminal, next_action
        ));

        // Store the computed next_action in the job result for later use
        let mut job_result = job_result;
        job_result.next_action = Some(next_action.clone());

        // Apply the next action
        match next_action {
            PhaseNextAction::Retry => {
                // Check if we can retry
                let should_retry = event.original_job.can_retry();

                if !should_retry {
                    // Retry limit exceeded - treat as error and complete
                    state.complete_job_with_info(
                        event.job_id,
                        event.original_job.phase_key.clone(),
                        event.original_job.phase_name.clone(),
                        event.original_job.slot_id.clone(),
                        job_result,
                    );
                    drop(state);
                    self.emit_stats(app_handle.as_ref()).await;
                    return true; // Continue execution
                }

                // Perform retry
                let retry_job = event.original_job.create_retry_job();

                let delay_msg = if let Some(ms) = retry_job.retry_delay_ms {
                    format!(" (waiting {}ms before retry)", ms)
                } else {
                    String::new()
                };

                let reason = if let Some(err) = &job_result.error {
                    format!("error: {}", err)
                } else if let Some(secs) = job_result.timeout_secs {
                    format!("timeout after {}s", secs)
                } else {
                    "explicit retry".to_string()
                };

                crate::cli_output::print_section(
                    crate::cli_output::Section::Phase,
                    format!(
                        "Retrying job {} due to {} (attempt {}/{}{})",
                        retry_job.phase_name,
                        reason,
                        retry_job.retry_count + 1,
                        retry_job.retry_limit + 1,
                        delay_msg
                    ),
                );

                state.job_info.insert(
                    event.job_id,
                    (
                        event.original_job.phase_key.clone(),
                        event.original_job.phase_name.clone(),
                        event.original_job.slot_id.clone(),
                    ),
                );
                state.complete_job(event.job_id, job_result);

                if let Some(delay_ms) = retry_job.retry_delay_ms {
                    let state_arc = self.state.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        let mut state = state_arc.write().await;
                        state.enqueue_retry_job(retry_job);
                    });
                } else {
                    state.enqueue_retry_job(retry_job);
                }
                return true; // Continue execution
            }
            PhaseNextAction::Stop => {
                // Stop this slot's execution
                let slot_display = event.original_job.slot_id.as_deref().unwrap_or("null");
                crate::cli_output::print_section(
                    crate::cli_output::Section::Phase,
                    format!("Stopping slot {} due to phase outcome", slot_display),
                );
                let cancelled_jobs = if let Some(ref slot_id) = event.original_job.slot_id {
                    state.cancel_slot_jobs(slot_id)
                } else {
                    Vec::new()
                };
                self.emit_cancelled_jobs(
                    &cancelled_jobs,
                    &format!("Skipped because slot {} stopped", slot_display),
                    JobStatus::Skipped,
                    Outcome::Skip,
                    app_handle.as_ref(),
                )
                .await;
                state.complete_job_with_info(
                    event.job_id,
                    event.original_job.phase_key.clone(),
                    event.original_job.phase_name.clone(),
                    event.original_job.slot_id.clone(),
                    job_result,
                );
            }
            PhaseNextAction::Continue | PhaseNextAction::Skip => {
                // Continue to next phase or skip
                state.complete_job_with_info(
                    event.job_id,
                    event.original_job.phase_key.clone(),
                    event.original_job.phase_name.clone(),
                    event.original_job.slot_id.clone(),
                    job_result,
                );
            }
            PhaseNextAction::Fail => {
                // Mark as failed and stop execution
                crate::cli_output::print_section(
                    crate::cli_output::Section::Phase,
                    format!(
                        "Phase {} failed - stopping execution",
                        event.original_job.phase_name
                    ),
                );

                // Check for should_stop_on_first_failure
                if state.should_stop_on_first_failure {
                    let cancelled_jobs = state.cancel_all_jobs(
                        "Stopped due to on_first_failure: stop after phase failure",
                    );
                    self.emit_cancelled_jobs(
                        &cancelled_jobs,
                        "Stopped due to on_first_failure: stop",
                        JobStatus::Skipped,
                        Outcome::Skip,
                        app_handle.as_ref(),
                    )
                    .await;
                }

                state.complete_job_with_info(
                    event.job_id,
                    event.original_job.phase_key.clone(),
                    event.original_job.phase_name.clone(),
                    event.original_job.slot_id.clone(),
                    job_result,
                );
            }
        }

        // If next_action is Stop, request shutdown to trigger teardown of queued jobs
        if matches!(next_action, PhaseNextAction::Stop) {
            state.shutdown_requested = true;
            crate::cli_output::warning(format!(
                "Phase '{}' resulted in STOP action - setting shutdown_requested=true and will return false",
                event.original_job.phase_name
            ));
        }

        drop(state);

        // Emit updated execution stats after job completion
        self.emit_stats(app_handle.as_ref()).await;

        // Return false if next_action is Stop (halt execution), true otherwise
        let should_continue = !matches!(next_action, PhaseNextAction::Stop);
        crate::cli_output::debug(format!(
            "Phase '{}' completion handler returning should_continue={}",
            event.original_job.phase_name, should_continue
        ));
        should_continue
    }

    /// Get next action for terminal results (error, timeout, STOP)
    /// Terminal results default to Stop, but then.{outcome} can override
    fn get_next_action_for_terminal(
        &self,
        outcome: &Outcome,
        phase_def: &crate::schema::procedure::PhaseDefinition,
    ) -> PhaseNextAction {
        if let Some(then_config) = &phase_def.then {
            let configured = match outcome {
                Outcome::Fail => then_config.fail.clone(), // STOP → Fail outcome
                Outcome::Error => then_config.error.clone(), // Error/Timeout → Error outcome
                _ => None,
            };

            if let Some(next_action) = configured {
                crate::cli_output::debug(format!(
                    "Phase '{}': Terminal result, using then.{:?}: {:?}",
                    phase_def.get_display_name(),
                    outcome,
                    next_action
                ));
                return next_action;
            }
        }

        crate::cli_output::debug(format!(
            "Phase '{}': Terminal result, using default: Stop",
            phase_def.get_display_name()
        ));
        PhaseNextAction::Stop
    }

    /// Get next action for non-terminal results
    /// Non-terminal results default based on outcome
    fn get_next_action_for_non_terminal(
        &self,
        outcome: &Outcome,
        phase_def: &crate::schema::procedure::PhaseDefinition,
    ) -> PhaseNextAction {
        if let Some(then_config) = &phase_def.then {
            let configured = match outcome {
                Outcome::Pass => then_config.pass.clone(),
                Outcome::Fail => then_config.fail.clone(),
                Outcome::Skip => None,
                Outcome::Error => then_config.error.clone(),
                Outcome::Aborted => then_config.error.clone(),
                Outcome::Timeout => then_config.error.clone(),
            };

            if let Some(next_action) = configured {
                crate::cli_output::debug(format!(
                    "Phase '{}': Non-terminal, using then.{:?}: {:?}",
                    phase_def.get_display_name(),
                    outcome,
                    next_action
                ));
                return next_action;
            }
        }

        // Default actions for non-terminal outcomes
        let default_action = match outcome {
            Outcome::Pass | Outcome::Skip | Outcome::Fail | Outcome::Timeout | Outcome::Aborted => {
                PhaseNextAction::Continue
            }
            Outcome::Error => PhaseNextAction::Stop,
        };

        crate::cli_output::debug(format!(
            "Phase '{}': Non-terminal, using default for {:?}: {:?}",
            phase_def.get_display_name(),
            outcome,
            default_action
        ));

        default_action
    }
}
