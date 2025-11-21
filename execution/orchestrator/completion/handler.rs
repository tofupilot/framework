use tauri::AppHandle;

use crate::execution::job::{JobResult, JobStatus, Outcome};
use crate::procedure::schema::{PhaseNextAction, StageScope};

use super::super::orchestrator::{JobCompletionEvent, Orchestrator};
use super::{attachment_collector, error_handling, event_emitter, next_action, outcome_resolver};

impl Orchestrator {
    pub(in crate::execution::orchestrator) async fn handle_job_completion(
        &self,
        event: JobCompletionEvent,
        app_handle: Option<AppHandle>,
    ) -> bool {
        log::debug!(
            "Handling job completion for {}",
            event.original_job.phase_name
        );

        let job_result = match &event.result {
            Ok(result) => result.clone(),
            Err(e) => error_handling::convert_error_to_result(
                e.to_string(),
                &event.original_job,
                event.job_id,
            ),
        };

        let shutdown_requested = {
            let state = self.state.read().await;
            state.shutdown_requested
        };

        let (phase_outcome, is_retry_limit_exceeded) =
            outcome_resolver::resolve_outcome(&job_result, &event.original_job, shutdown_requested);

        let phase_def = self.get_phase_definition(&event);

        let error_message =
            outcome_resolver::format_error_message(is_retry_limit_exceeded, &job_result);

        log::debug!(
            "Phase '{}': phase_result={:?}, phase_outcome={:?}",
            event.original_job.phase_name,
            job_result.phase_result,
            phase_outcome
        );

        let next_action =
            next_action::determine_next_action(&job_result, &phase_outcome, phase_def);

        let mut job_result = job_result;
        job_result.phase_outcome = phase_outcome;
        job_result.next_action = Some(next_action.clone());

        event_emitter::log_resource_metrics(&event.original_job, &job_result);
        event_emitter::log_phase_completion(
            &event.original_job,
            &job_result,
            phase_outcome,
            &error_message,
        );

        if let Some(ref app) = app_handle {
            let attachments = attachment_collector::collect_attachments(
                &self.report_managers,
                &event.job_id,
                &event.original_job.slot_id,
            )
            .await;

            event_emitter::emit_job_complete_event(
                app,
                event.job_id,
                &event.original_job,
                &job_result,
                phase_outcome,
                error_message.clone(),
                event.worker_id,
                attachments,
                is_retry_limit_exceeded,
            );
        }

        self.handle_plug_teardown(&event, app_handle.as_ref()).await;

        let mut state = self.state.write().await;

        let phase_failed = matches!(
            phase_outcome,
            Outcome::Fail | Outcome::Error | Outcome::Timeout | Outcome::Aborted
        ) || is_retry_limit_exceeded;

        if phase_failed {
            self.handle_phase_failure(&mut state, &event, app_handle.as_ref())
                .await;
        }

        let should_continue = self
            .apply_next_action(
                next_action,
                &mut state,
                event,
                job_result,
                app_handle.as_ref(),
            )
            .await;

        drop(state);

        self.emit_stats(app_handle.as_ref()).await;

        should_continue
    }

    fn get_phase_definition(
        &self,
        event: &JobCompletionEvent,
    ) -> Option<&crate::procedure::schema::PhaseDefinition> {
        let all_phases = self.procedure_definition.get_all_phases_with_stage_scope();
        all_phases
            .iter()
            .find(|(stage, phase)| {
                *stage == event.original_job.stage_scope
                    && phase.key == event.original_job.phase_key
            })
            .map(|(_, phase)| *phase)
    }

    async fn handle_plug_teardown(
        &self,
        event: &JobCompletionEvent,
        app_handle: Option<&AppHandle>,
    ) {
        if let Some(ref slot_id) = event.original_job.slot_id {
            if matches!(event.original_job.stage_scope, StageScope::TeardownEach) {
                log::info!(
                    "Destroying slot-level plugs for {} after TeardownSlot phase",
                    slot_id
                );

                self.emit_plug_scope_event("running").await;

                let resource_manager = self.resource_manager.write().await;
                if resource_manager.has_each_scope_plugs(&slot_id).await {
                    match resource_manager
                        .destroy_each_scope_plugs(slot_id.clone(), app_handle)
                        .await
                    {
                        Ok(_) => {
                            self.emit_plug_scope_event("pass").await;
                        }
                        Err(e) => {
                            log::warn!("Failed to destroy each-scope plugs for {}: {}", slot_id, e);
                            self.emit_plug_scope_event("error").await;
                        }
                    }

                    self.emit_stats(app_handle).await;
                }
            }
        }

        if matches!(event.original_job.stage_scope, StageScope::TeardownAll) {
            log::info!("Destroying all-scope plugs after TeardownAll phase");

            self.emit_plug_scope_event("running").await;

            let resource_manager = self.resource_manager.write().await;
            if resource_manager.has_all_scope_plugs().await {
                match resource_manager.destroy_all_scope_plugs(app_handle).await {
                    Ok(_) => {
                        self.emit_plug_scope_event("pass").await;
                    }
                    Err(e) => {
                        log::warn!("Failed to destroy all-scope plugs: {}", e);
                        self.emit_plug_scope_event("error").await;
                    }
                }

                self.emit_stats(app_handle).await;
            }
        }
    }

    async fn handle_phase_failure(
        &self,
        state: &mut crate::execution::state::OrchestratorState,
        event: &JobCompletionEvent,
        app_handle: Option<&AppHandle>,
    ) {
        match event.original_job.stage_scope {
            StageScope::SetupAll => {
                log::warn!(
                    "Setup procedure failed: Cancelling all slots and ensuring teardown runs"
                );
                let cancelled_jobs = state.cancel_all_jobs("Setup procedure failed");

                self.emit_cancelled_jobs(
                    &cancelled_jobs,
                    "Cancelled due to setup procedure failure",
                    JobStatus::Skipped,
                    Outcome::Skip,
                    app_handle,
                )
                .await;
            }
            StageScope::SetupEach => {
                let slot_display = event.original_job.slot_id.as_deref().unwrap_or("null");
                log::warn!(
                    "Setup slot failed for {}: Skipping to teardown slot",
                    slot_display
                );
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
                    app_handle,
                )
                .await;
            }
            _ => {}
        }
    }

    async fn apply_next_action(
        &self,
        next_action: PhaseNextAction,
        state: &mut crate::execution::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
        app_handle: Option<&AppHandle>,
    ) -> bool {
        match next_action {
            PhaseNextAction::Retry => {
                self.handle_retry(state, event, job_result, app_handle)
                    .await
            }
            PhaseNextAction::Stop => {
                self.handle_stop(state, event, job_result, app_handle).await;
                false
            }
            PhaseNextAction::Continue | PhaseNextAction::Skip => {
                state.complete_job_with_info(
                    event.job_id,
                    event.original_job.phase_key.clone(),
                    event.original_job.phase_name.clone(),
                    event.original_job.slot_id.clone(),
                    job_result,
                );
                true
            }
            PhaseNextAction::Fail => {
                self.handle_fail(state, event, job_result, app_handle).await;
                true
            }
        }
    }

    async fn handle_retry(
        &self,
        state: &mut crate::execution::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
        app_handle: Option<&AppHandle>,
    ) -> bool {
        let should_retry = event.original_job.can_retry();

        if !should_retry {
            state.complete_job_with_info(
                event.job_id,
                event.original_job.phase_key.clone(),
                event.original_job.phase_name.clone(),
                event.original_job.slot_id.clone(),
                job_result,
            );
            self.emit_stats(app_handle).await;
            return true;
        }

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

        log::info!(
            "Retrying job {} due to {} (attempt {}/{}{})",
            retry_job.phase_name,
            reason,
            retry_job.retry_count + 1,
            retry_job.retry_limit + 1,
            delay_msg
        );

        state.job_info.insert(
            event.job_id,
            crate::execution::state::JobInfo {
                phase_key: event.original_job.phase_key.clone(),
                phase_name: event.original_job.phase_name.clone(),
                slot_id: event.original_job.slot_id.clone(),
            },
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

        true
    }

    async fn handle_stop(
        &self,
        state: &mut crate::execution::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
        app_handle: Option<&AppHandle>,
    ) {
        let slot_display = event.original_job.slot_id.as_deref().unwrap_or("null");
        log::info!("Stopping slot {} due to phase outcome", slot_display);

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
            app_handle,
        )
        .await;

        state.complete_job_with_info(
            event.job_id,
            event.original_job.phase_key.clone(),
            event.original_job.phase_name.clone(),
            event.original_job.slot_id.clone(),
            job_result,
        );

        state.shutdown_requested = true;
        log::warn!(
            "Phase '{}' resulted in STOP action - setting shutdown_requested=true and will return false",
            event.original_job.phase_name
        );
    }

    async fn handle_fail(
        &self,
        state: &mut crate::execution::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
        app_handle: Option<&AppHandle>,
    ) {
        log::info!(
            "Phase {} failed - stopping execution",
            event.original_job.phase_name
        );

        if state.should_stop_on_first_failure {
            let cancelled_jobs =
                state.cancel_all_jobs("Stopped due to on_first_failure: stop after phase failure");
            self.emit_cancelled_jobs(
                &cancelled_jobs,
                "Stopped due to on_first_failure: stop",
                JobStatus::Skipped,
                Outcome::Skip,
                app_handle,
            )
            .await;

            state.shutdown_requested = true;
            log::warn!(
                "Phase '{}' failed with on_first_failure: stop - setting shutdown_requested=true to trigger graceful shutdown with teardown",
                event.original_job.phase_name
            );
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
