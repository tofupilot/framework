use crate::job::{JobResult, Outcome};
use crate::procedure::schema::{PhaseNextAction, StageScope};

use super::super::{JobCompletionEvent, Orchestrator};
use super::{error_handling, event_emitter, next_action, outcome_resolver};

/// Whether the phase's own `then:` says `continue` for this outcome: the
/// one thing that keeps a failing phase from ending its sequence.
fn explicitly_continues(
    def: Option<&crate::procedure::schema::PhaseDefinition>,
    outcome: Outcome,
) -> bool {
    let Some(then) = def.and_then(|d| d.then.as_ref()) else {
        return false;
    };
    let configured = match outcome {
        Outcome::Fail => &then.fail,
        Outcome::Error => &then.error,
        Outcome::Timeout => &then.timeout,
        _ => &None,
    };
    matches!(configured, Some(PhaseNextAction::Continue))
}

impl Orchestrator {
    pub(in crate::orchestrator) async fn handle_job_completion(
        &self,
        event: JobCompletionEvent,
    ) -> bool {
        log::debug!(
            "Handling job completion for {}",
            event.original_job.phase_name
        );

        let mut job_result = match &event.result {
            Ok(result) => result.clone(),
            Err(e) => error_handling::convert_error_to_result(
                e.to_string(),
                &event.original_job,
                event.job_id,
            ),
        };
        // The error path builds its result from scratch (attempt 0). An
        // attempt that ties with an earlier one is picked at random as the
        // phase's final result, so a retry that timed out could read PASS.
        job_result.retry_count = event.original_job.retry_count;

        // Stop scope = job scope: a phase finishing while ITS slot is
        // stopping (or the execution is) was cut short, whatever it
        // reports. Single slot: identical to reading the execution flag.
        //
        // Teardown is the exception: it is the work that runs BECAUSE of the
        // stop, so the stop says nothing about how it went. It keeps its own
        // outcome, error included: a power-off that crashed must read ERROR
        // with its traceback, not a silent STOP. A force kill is not an
        // exception to the exception: the killed worker's IPC error is an
        // artefact of the kill, and the teardown reads STOP like any phase
        // the kill cut short. A sequence's teardown phase is teardown work
        // too: it sits in its caller's stage, only `sequence_stages` says
        // what it is, which `is_teardown_work` reads.
        let is_teardown = event.original_job.is_teardown_work();
        let (shutdown_requested, should_stop_on_first_failure) = {
            let state = self.state.read().await;
            let cut_short = state.is_stopping_for(event.original_job.slot_id.as_deref());
            (
                cut_short && (!is_teardown || state.force_kill_requested),
                state.should_stop_on_first_failure,
            )
        };

        // When a stop cuts a phase, the worker surfaces the cut as an error
        // (a killed body's traceback, a cancelled operator prompt's
        // "required input missing"). That error is a consequence of the
        // stop, not a real phase failure — drop it so the outcome resolver
        // classifies the phase as STOP, not ERROR. Without this the UI
        // flickers from "aborted" to "error" when the phase reports back,
        // and a slot whose neighbour phase failed reads ERROR.
        //
        // Cut means the stop reached into the phase: a prompt or a
        // `ui.wait()` it closed, or the worker it killed. A phase that
        // returned on its own while the stop was under way was not cut:
        // its verdict stands, PASS or FAIL, as its measurements say, and
        // so does an error it raised itself, a bug it reports.
        let cut_short = shutdown_requested
            && job_result.error.is_some()
            && (job_result.prompt_closed || event.result.is_err());
        if cut_short {
            job_result.error = None;
        }

        let (mut phase_outcome, is_retry_limit_exceeded) =
            outcome_resolver::resolve_outcome(&job_result, &event.original_job, cut_short);
        // Nothing runs again under a stop: a `phase.retry()` reads FAIL,
        // as a `then: fail: retry` the stop refuses does (`handle_retry`).
        if shutdown_requested && phase_outcome == Outcome::Retry {
            phase_outcome = Outcome::Fail;
        }

        let phase_def = self.get_phase_definition(&event);

        let error_message = outcome_resolver::format_error_message(
            is_retry_limit_exceeded,
            event.original_job.retry_limit,
            &job_result,
        );

        log::debug!(
            "DEBUG Phase '{}': phase_result={:?}, phase_outcome={:?}, retry_count={}, retry_limit={}, can_retry={}",
            event.original_job.phase_name,
            job_result.phase_result,
            phase_outcome,
            event.original_job.retry_count,
            event.original_job.retry_limit,
            event.original_job.can_retry()
        );

        let next_action = next_action::determine_next_action(
            &job_result,
            &phase_outcome,
            phase_def,
            should_stop_on_first_failure,
            self.may_retry(&event.original_job),
        );

        log::debug!(
            "DEBUG Phase '{}': next_action={:?}",
            event.original_job.phase_name,
            next_action
        );

        job_result.phase_outcome = phase_outcome;
        job_result.next_action = Some(next_action.clone());

        event_emitter::log_resource_metrics(&event.original_job, &job_result);
        event_emitter::log_phase_completion(
            &event.original_job,
            &job_result,
            phase_outcome,
            &error_message,
        );

        event_emitter::emit_job_complete_event(
            &self.event_sink,
            &event,
            &job_result,
            phase_outcome,
            error_message.clone(),
            is_retry_limit_exceeded,
        );

        let mut state = self.state.write().await;

        // A setup phase that still has a retry coming is not a setup
        // failure yet: cancelling here would strand the retry on a slot
        // already marked stopped (`handle_retry` refuses to re-enqueue
        // under a stop). `determine_next_action` only answers Retry while
        // the budget allows it, so the exhausted case still lands here.
        // A phase of a sequence called from the procedure's setup fails
        // its sequence first; the sequence's settlement then escalates to
        // the slot (a failed setup stage always does, see
        // `settle_sequences`), so the stage rule is not applied twice.
        let is_setup_failure = event.original_job.sequence_path.is_empty()
            && matches!(
                event.original_job.stage_scope,
                StageScope::SetupAll | StageScope::SetupEach
            )
            && (matches!(
                phase_outcome,
                Outcome::Fail | Outcome::Error | Outcome::Timeout | Outcome::Stop
            ) || is_retry_limit_exceeded)
            && !matches!(next_action, PhaseNextAction::Retry);

        if is_setup_failure {
            self.handle_setup_failure(&mut state, &event, &job_result)
                .await;
        }

        let finished = event.original_job.clone();
        let in_sequence = !event.original_job.sequence_path.is_empty();

        // FAIL_SUBTEST: a phase that fails inside a sequence ends the
        // sequence, whatever `execution.on_first_failure` says. That policy
        // decides what the RUN does with the sequence's outcome, which the
        // settlement applies once the sequence has settled; it does not
        // let the sequence go on past a failure. The phase's own `then:`
        // is the author's explicit word for this phase and still wins,
        // except in the sequence's `setup:`: a failed setup stage always
        // stops its scope, as it does in the procedure.
        let in_sequence_setup = event.original_job.sequence_stages.last()
            == Some(&crate::procedure::schema::PhaseStage::Setup);
        if in_sequence
            && matches!(next_action, PhaseNextAction::Continue)
            && (is_retry_limit_exceeded
                || matches!(
                    phase_outcome,
                    Outcome::Fail | Outcome::Error | Outcome::Timeout
                ))
            && (in_sequence_setup || !explicitly_continues(phase_def, phase_outcome))
        {
            self.cancel_scope_of(
                &mut state,
                &event.original_job,
                &job_result,
                Some(crate::state::ShutdownCause::PhaseFailure),
                "sequence failure",
            )
            .await;
        }

        let should_continue = self
            .apply_next_action(next_action, &mut state, event, job_result)
            .await;

        // After every completion, not only one inside a sequence: a stop or
        // a failure of a procedure phase cancels the queued jobs of calls it
        // never entered, and that cancellation is what finishes them. Left
        // unsettled, those calls would miss from the upload while their
        // phases name them, and the server refuses such a run.
        self.settle_sequences(&mut state).await;

        drop(state);

        // After the next action is applied: a retry of this phase is then
        // queued or waiting out its delay, and counts as outstanding
        // teardown work of the scope.
        self.handle_plug_teardown(&finished).await;

        self.emit_stats().await;

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

    /// Release a teardown scope's plugs once its LAST teardown phase is
    /// done: every TeardownEach phase of the slot, or every TeardownAll
    /// phase. Releasing after the first one left the next teardown phase
    /// (and any retry) with an empty plug map. Whatever is still up when
    /// the run ends is released by `execute_all` / `shutdown()`.
    async fn handle_plug_teardown(&self, job: &crate::job::Job) {
        let slot_id = match job.stage_scope {
            StageScope::TeardownEach => match &job.slot_id {
                Some(slot_id) => Some(slot_id.clone()),
                None => return,
            },
            StageScope::TeardownAll => None,
            _ => return,
        };
        if self
            .state
            .read()
            .await
            .teardown_outstanding(job.stage_scope, slot_id.as_deref(), job.id)
        {
            return;
        }

        // Lock order `state → resource_manager`: the state lock is released
        // above, and the progress events (which take `state`) are emitted
        // after the resource manager is released.
        let released = {
            let resource_manager = self.resource_manager.write().await;
            match &slot_id {
                Some(slot_id) => {
                    if !resource_manager.has_slot_scope_plugs(slot_id).await {
                        return;
                    }
                    log::info!(
                        "Destroying slot-level plugs for {slot_id} after its last teardown phase"
                    );
                    resource_manager
                        .destroy_slot_scope_plugs(slot_id.clone(), &self.event_sink)
                        .await
                }
                None => {
                    if !resource_manager.has_execution_scope_plugs().await {
                        return;
                    }
                    log::info!("Destroying execution-scope plugs after the last TeardownAll phase");
                    resource_manager
                        .destroy_execution_scope_plugs(&self.event_sink)
                        .await
                }
            }
        };

        match released {
            Ok(_) => self.emit_plug_scope_event("pass").await,
            Err(e) => {
                match slot_id.as_deref() {
                    Some(slot) => log::warn!("Cannot stop plugs of slot {slot}: {e}"),
                    None => log::warn!("Cannot stop shared plugs: {e}"),
                }
                self.emit_plug_scope_event("error").await;
            }
        }
        self.emit_stats().await;
    }

    /// A failed setup stage cancels the work that depended on it, whatever
    /// `on_first_failure` says: SetupAll the execution, SetupEach its slot.
    /// Same dispatch as `handle_stop`; only the log line differs.
    async fn handle_setup_failure(
        &self,
        state: &mut crate::state::OrchestratorState,
        event: &JobCompletionEvent,
        job_result: &JobResult,
    ) {
        let job = &event.original_job;
        match job.stage_scope {
            StageScope::SetupAll => {
                log::debug!(
                    "Setup procedure failed: Cancelling all slots and ensuring teardown runs"
                );
                // The shared setup touched the bench for every slot: each
                // queued slot's teardown runs to make it safe (a phase of
                // it that takes a plug the slot never opened reads SKIP).
                // A slot still waiting its turn under slot-first is not
                // queued yet, and is cancelled whole.
                let slots = state.slots.clone();
                state.started_slots.extend(slots);
            }
            StageScope::SetupEach => log::debug!(
                "Setup slot failed for {}: Skipping to teardown slot",
                job.slot_id.as_deref().unwrap_or("null")
            ),
            _ => return,
        }
        self.cancel_scope_of(
            state,
            job,
            job_result,
            Some(crate::state::ShutdownCause::PhaseFailure),
            "setup failure",
        )
        .await;
    }

    /// Stop scope = job scope. A slot job cancels its slot, a shared job
    /// cancels the execution. Emits Skipped for everything removed.
    ///
    /// `stop_reason` is the wire text quoted on the cancelled UI phases;
    /// its shape ("Run aborted by phase '…': …") is what single-slot runs
    /// have always shown, so it is kept verbatim there.
    async fn cancel_scope_of(
        &self,
        state: &mut crate::state::OrchestratorState,
        job: &crate::job::Job,
        job_result: &JobResult,
        cause: Option<crate::state::ShutdownCause>,
        why: &str,
    ) {
        // A phase that finished while a stop was already under way reads
        // STOP without having asked for it: the phases left are cut by
        // that stop, and say so, not by this phase.
        let caught = job_result.phase_outcome == Outcome::Stop
            && !matches!(job_result.phase_result, crate::job::PhaseResult::Stop);
        if caught {
            let reason = job
                .slot_id
                .as_ref()
                .and_then(|slot| state.slot_stops.get(slot))
                .map(|stop| stop.reason.clone())
                .unwrap_or_else(|| state.stop_reason());
            let cancelled = match &job.slot_id {
                Some(slot_id) => state.cancel_slot_jobs(slot_id, cause, &reason),
                None => state.cancel_all_jobs(&reason, cause),
            };
            self.emit_cancelled_work(&cancelled, &crate::state::not_run(&reason))
                .await;
            return;
        }
        let detail = job_result
            .error
            .as_ref()
            .map(|e| format!(": {e}"))
            .unwrap_or_default();
        // A failing phase of a sequence stops THE SEQUENCE, not the slot:
        // its remaining phases skip, its teardown runs, and once it has
        // settled the parent applies its own rule to the sequence's
        // outcome (`settle_sequences`). A stop that is not a failure, an
        // operator's or one the phase asked for (`phase.stop()`, its own
        // `then: pass: stop`), keeps the job's slot or execution as its
        // scope: that is an abort, not a failure to be judged level by
        // level. The request is what counts, not the outcome: `phase.stop()`
        // after a failed measurement reads FAIL (the defective unit enters
        // the failure statistics) and still halts the slot, as it does
        // outside a call.
        let failed = matches!(
            job_result.phase_outcome,
            Outcome::Fail | Outcome::Error | Outcome::Timeout
        );
        if !job.sequence_path.is_empty()
            && failed
            && !matches!(job_result.phase_result, crate::job::PhaseResult::Stop)
        {
            let stop_reason = format!(
                "Sequence '{}' aborted by phase '{}'{detail}",
                job.sequence_path.join("/"),
                job.phase_name
            );
            let cancelled = state.cancel_sequence_jobs(
                job.slot_id.as_deref(),
                &job.sequence_path,
                cause,
                &stop_reason,
            );
            self.emit_cancelled_work(
                &cancelled,
                &crate::state::not_run(&format!("{why} in {}", job.phase_name)),
            )
            .await;
            return;
        }

        let stop_reason = match &job.slot_id {
            Some(slot) if state.slots.len() > 1 => {
                format!("Slot {slot} stopped by phase {}{detail}", job.phase_name)
            }
            _ => format!("Stopped by phase {}{detail}", job.phase_name),
        };

        let cancelled = match &job.slot_id {
            Some(slot_id) => state.cancel_slot_jobs(slot_id, cause, &stop_reason),
            None => state.cancel_all_jobs(&stop_reason, cause),
        };

        self.emit_cancelled_work(
            &cancelled,
            &crate::state::not_run(&format!("{why} in {}", job.phase_name)),
        )
        .await;
    }

    async fn apply_next_action(
        &self,
        next_action: PhaseNextAction,
        state: &mut crate::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
    ) -> bool {
        let outcome = job_result.phase_outcome;

        if matches!(outcome, Outcome::Stop) {
            self.handle_stop(state, event, job_result).await;
            return false;
        }

        match next_action {
            PhaseNextAction::Retry => self.handle_retry(state, event, job_result).await,
            PhaseNextAction::Stop => {
                self.handle_stop(state, event, job_result).await;
                false
            }
            PhaseNextAction::Continue => {
                state.complete_job_with_info(event.job_id, &event.original_job, job_result);
                true
            }
        }
    }

    async fn handle_retry(
        &self,
        state: &mut crate::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
    ) -> bool {
        // No retry of a main phase once its scope is stopping: the slot
        // (`slot_stops`) or the execution (`shutdown_requested`). A job
        // that was running during the cancel still completes after it,
        // and would otherwise re-enqueue a phase the orchestrator gave up
        // on. Teardown phases keep their retry budget: they are exactly
        // the phases that DO run during a stop. A sequence's teardown phase
        // included, whatever stage of the procedure its call sits in.
        let is_teardown = event.original_job.is_teardown_work();
        let shutdown_in_progress =
            !is_teardown && state.is_stopping_for(event.original_job.slot_id.as_deref());
        let should_retry = self.may_retry(&event.original_job) && !shutdown_in_progress;

        if !should_retry {
            state.complete_job_with_info(event.job_id, &event.original_job, job_result);
            // emit_stats is called by handle_job_completion after releasing state lock
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
        } else if let Some(ms) = job_result.timeout_ms {
            format!(
                "timeout after {}",
                crate::procedure::schema::format_duration(ms)
            )
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
            crate::state::JobInfo::from_job(&event.original_job),
        );
        // Record result without satisfying dependencies -- dependents stay blocked until retry resolves
        state.record_retry_attempt(event.job_id, job_result);

        if let Some(delay_ms) = retry_job.retry_delay_ms {
            let state_arc = self.state.clone();
            let phase_key = retry_job.phase_key.clone();
            let phase_name = retry_job.phase_name.clone();
            let function = retry_job.function.clone();
            let slot_id = retry_job.slot_id.clone();
            let retry_job_id = retry_job.id;
            let dependency_id = retry_job.dependency_id;
            let retry_count = retry_job.retry_count;
            let stage_scope = retry_job.stage_scope;
            let sequence_path = retry_job.sequence_path.clone();

            let handle = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let mut state = state_arc.write().await;
                // Belt and braces: a cancel aborts this task, but should
                // it land between the sleep and the lock the scope check
                // still keeps a cancelled slot's retry out of the queue.
                // A teardown retry runs under a stop, like the teardown
                // itself; dropping it here left the run waiting forever on
                // the TeardownAll that depends on it.
                let keep = if retry_job.is_teardown() {
                    !state.force_kill_requested
                } else {
                    !state.is_stopping_for(retry_job.slot_id.as_deref())
                };
                if keep {
                    state.enqueue_retry_job(retry_job);
                }
            });

            state
                .pending_delayed_retry_handles
                .push(crate::state::PendingDelayedRetry {
                    handle,
                    phase_key,
                    phase_name,
                    function,
                    slot_id,
                    job_id: retry_job_id,
                    dependency_id,
                    retry_count,
                    stage_scope,
                    sequence_path,
                });
        } else {
            state.enqueue_retry_job(retry_job);
        }

        true
    }

    async fn handle_stop(
        &self,
        state: &mut crate::state::OrchestratorState,
        event: JobCompletionEvent,
        job_result: JobResult,
    ) {
        let outcome = job_result.phase_outcome;
        let reason = stop_reason(&job_result);

        log::debug!(
            "Phase '{}' resulted in {} - stopping {}",
            event.original_job.phase_name,
            reason,
            match &event.original_job.slot_id {
                Some(slot) => format!("slot {slot}"),
                None => "all execution".to_string(),
            }
        );

        // Only a phase that genuinely failed makes this a `PhaseFailure`. An
        // `Outcome::Stop` lands here too, for two reasons that are both NOT
        // failures: an explicit `phase.stop()` (caught by the aggregation on
        // `phase_result`), and a phase that simply finished while an operator
        // stop was already raised — whoever raised it recorded the cause, and
        // stamping `PhaseFailure` over it would turn that abort into a PASS.
        let cause = match outcome {
            Outcome::Fail | Outcome::Error | Outcome::Timeout => {
                Some(crate::state::ShutdownCause::PhaseFailure)
            }
            _ => None,
        };

        self.cancel_scope_of(state, &event.original_job, &job_result, cause, reason)
            .await;

        state.complete_job_with_info(event.job_id, &event.original_job, job_result);

        // Note: shutdown_requested is set by cancel_all_jobs only if no teardown phases remain
    }
}

/// Why a phase ended its scope, as quoted on the phases it cancelled. An
/// explicit `phase.stop()` is named as such even when a failed measurement
/// made the outcome FAIL: the stop was the phase's own request, not
/// `on_first_failure`.
pub(crate) fn stop_reason(result: &JobResult) -> &'static str {
    if matches!(result.phase_result, crate::job::PhaseResult::Stop) {
        return "stop requested by phase";
    }
    match result.phase_outcome {
        Outcome::Error => "error",
        Outcome::Timeout => "timeout",
        Outcome::Stop => "stop",
        Outcome::Fail => "failure (on_first_failure: stop)",
        _ => "terminal outcome",
    }
}
