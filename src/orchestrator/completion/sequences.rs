//! Settlement of sequence calls: when every job of a call in one scope is
//! done, the call gets an outcome, announces it, and its parent applies
//! its rule to it. The level-by-level propagation of a failure, TestStand's
//! cleanup stack and OpenHTF's `FAIL_SUBTEST` in one mechanism.
//!
//! The outcome is the run's own aggregation (`determine_aggregate_outcome`,
//! the TP-957 rule) applied to the call's phases under the call's stop
//! state, plus Skip when none of them ran. The parent's rule is the
//! call's `then:` for that outcome, else "a failed setup stage stops its
//! scope", else `execution.on_first_failure`.

use crate::event_sink::ExecutionEvent;
use crate::job::{JobResult, Outcome};
use crate::procedure::schema::{PhaseNextAction, PhaseStage, SequenceInstance};
use crate::state::{OrchestratorState, SequenceScope, ShutdownCause};

use super::super::stats::{determine_aggregate_outcome, final_attempts_where};
use super::super::Orchestrator;

/// A job completed as cancelled never ran: the skip the cancellation
/// recorded carries no `next_action`, while a phase that chose to skip
/// (`phase.skip()`) went through the handler and has one.
fn was_cancelled(result: &JobResult) -> bool {
    result.phase_outcome == Outcome::Skip && result.next_action.is_none()
}

fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Fail => "failure",
        Outcome::Error => "error",
        Outcome::Timeout => "timeout",
        Outcome::Stop => "stop",
        Outcome::Pass => "pass",
        Outcome::Skip => "skip",
        Outcome::Retry => "retry",
    }
}

impl Orchestrator {
    /// Settle every sequence scope whose jobs are all done, deepest calls
    /// first, and loop while a settlement cancels work that completes
    /// another scope. Called under the state lock after every job
    /// completion, after a stop or kill completed the queued jobs, once
    /// after submit for calls with no job, and once before the run
    /// completes: a call must never be left unsettled, the upload names
    /// it through its phases.
    pub(in crate::orchestrator) async fn settle_sequences(&self, state: &mut OrchestratorState) {
        let mut order: Vec<&SequenceInstance> =
            self.procedure_definition.sequences.iter().collect();
        order.sort_by_key(|i| std::cmp::Reverse(i.path.len()));

        loop {
            let mut progressed = false;
            for inst in &order {
                let path = inst.path_str();
                // Every scope this call runs in (one per slot, or the shared
                // one): a cancellation at execution scope finishes calls in
                // slots other than the completing job's.
                let mut scopes: Vec<SequenceScope> = state
                    .sequence_jobs
                    .keys()
                    .filter(|(_, p)| *p == path)
                    .cloned()
                    .collect();
                scopes.sort();
                for scope in scopes {
                    if state.settled_sequences.contains_key(&scope) {
                        continue;
                    }
                    if state.sequence_complete(&scope) != Some(true) {
                        continue;
                    }
                    let outcome = self.settle_one(state, inst, &scope).await;
                    self.escalate(state, inst, &scope, outcome).await;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    /// Close, as cancelled, every job of a sequence scope that is not
    /// completed. For the end of a stop: the paths that finish without
    /// running a job (a kill between the queue drain and the teardown
    /// workers, a teardown phase still running at the timeout) otherwise
    /// leave the job's scope open for good, and the call never settles.
    pub(crate) fn cancel_unfinished_sequence_jobs(
        &self,
        state: &mut OrchestratorState,
        reason: &str,
    ) {
        let unfinished: Vec<uuid::Uuid> = state
            .sequence_jobs
            .values()
            .flatten()
            .filter(|id| !state.sequence_job_done(id))
            .copied()
            .collect();
        for id in unfinished {
            let attempt = state.job_info.get(&id).map_or(0, |info| info.retry_count);
            state.complete_job(id, OrchestratorState::skip_result(attempt));
            if let Some(info) = state.job_info.get(&id) {
                self.event_sink.emit(&ExecutionEvent::JobProgress {
                    job_id: id.to_string(),
                    slot_id: info.slot_id.clone(),
                    phase_key: info.phase_key.clone(),
                    phase_name: info.phase_name.clone(),
                    stage_scope: info.stage_scope,
                    status: crate::job::JobStatus::Skipped,
                    worker_id: None,
                    started_at: None,
                    timeout_ms: None,
                    outcome: Some(Outcome::Skip),
                    retry_count: attempt,
                    error: Some(reason.to_string()),
                });
            }
        }
    }

    /// Compute, record and announce one scope's outcome.
    async fn settle_one(
        &self,
        state: &mut OrchestratorState,
        inst: &SequenceInstance,
        scope: &SequenceScope,
    ) -> Outcome {
        let path_keys = inst.path.clone();
        let results = final_attempts_where(&state.job_results, &state.job_info, |info| {
            info.slot_id == scope.0 && info.in_sequence_scope(&path_keys)
        });
        let had_cancelled_work = results.iter().any(|r| was_cancelled(r));
        // A phase that ended as Stop (interrupted by the operator, or
        // `phase.stop()`) is as much the stop's mark as a cancelled job:
        // without it, a call whose only phase was cut would aggregate to
        // Pass under the raised flag.
        let interrupted = results.iter().any(|r| r.phase_outcome == Outcome::Stop);
        let ran: Vec<&&JobResult> = results.iter().filter(|r| !was_cancelled(r)).collect();
        let (stopped, cause) = state.sequence_stop_state(scope, had_cancelled_work || interrupted);
        // The window spans every attempt that ran, not the final ones only:
        // a retried phase's earlier attempts are uploaded under the call
        // too, and the run's own window counts them.
        let attempts: Vec<&JobResult> = state
            .job_results
            .iter()
            .filter(|(id, r)| {
                !was_cancelled(r)
                    && state.job_info.get(id).is_some_and(|info| {
                        info.slot_id == scope.0 && info.in_sequence_scope(&path_keys)
                    })
            })
            .map(|(_, r)| r)
            .collect();
        let started_at = attempts.iter().map(|r| r.started_at).min();
        let ended_at = attempts.iter().map(|r| r.completed_at).max();
        // A stop while a retry waits out its delay cancels the retry, and
        // that cancelled attempt is then the phase's final one: nothing in
        // `ran`, yet the call did run (its first attempt) and was cut. It
        // is interrupted, not skipped.
        let outcome = if ran.is_empty() {
            if stopped && !attempts.is_empty() {
                Outcome::Stop
            } else {
                Outcome::Skip
            }
        } else {
            determine_aggregate_outcome(&results, stopped, cause, &None, false)
        };

        log::info!(
            "Sequence '{}' ({}) settled: {:?}",
            inst.path_str(),
            scope.0.as_deref().unwrap_or("<shared>"),
            outcome
        );
        state.settled_sequences.insert(scope.clone(), outcome);
        self.event_sink.emit(&ExecutionEvent::SequenceComplete {
            slot_id: scope.0.clone(),
            sequence_path: inst.path.clone(),
            key: inst.key().to_string(),
            name: inst.name.clone(),
            source: inst.source.clone(),
            outcome,
            started_at: started_at.map(|t| t.to_rfc3339()),
            ended_at: ended_at.map(|t| t.to_rfc3339()),
        });
        outcome
    }

    /// Apply the parent's rule to a settled call: on a failing outcome,
    /// the call's `then:`, else the stage rule, else `on_first_failure`.
    /// A stop cancels the parent scope (the enclosing call, or the slot or
    /// execution for a call made in the procedure), teardown work kept.
    async fn escalate(
        &self,
        state: &mut OrchestratorState,
        inst: &SequenceInstance,
        scope: &SequenceScope,
        outcome: Outcome,
    ) {
        if !matches!(outcome, Outcome::Fail | Outcome::Error | Outcome::Timeout) {
            return;
        }
        let configured = inst.then.as_ref().and_then(|t| match outcome {
            Outcome::Fail => t.fail.clone(),
            Outcome::Error => t.error.clone(),
            Outcome::Timeout => t.timeout.clone(),
            _ => None,
        });
        let nested = inst.path.len() > 1;
        // A nested call is to its parent what a phase is to a sequence:
        // its failure ends the parent (FAIL_SUBTEST), whatever
        // `on_first_failure` says, unless the call's own `then:` continues.
        let stops_by_rule = nested || state.should_stop_on_first_failure;
        // A failed setup stage always stops its scope, the procedure's or
        // a sequence's: what `main` relies on did not happen, and a `then:`
        // on the entry does not change that (the rule a setup phase of the
        // procedure follows, see `handle_setup_failure`).
        let action = if inst.stage == PhaseStage::Setup {
            PhaseNextAction::Stop
        } else {
            configured.unwrap_or(if stops_by_rule {
                PhaseNextAction::Stop
            } else {
                PhaseNextAction::Continue
            })
        };
        // Retry is a phase action; on a call it reads as continue.
        if action != PhaseNextAction::Stop {
            return;
        }

        let word = outcome_word(outcome);
        let path = inst.path_str();
        let parent = inst.parent_path();
        let cause = Some(ShutdownCause::PhaseFailure);
        let (reason, cancelled) = if !parent.is_empty() {
            let reason = format!(
                "Sequence '{}' aborted by sequence '{path}' ({word})",
                parent.join("/")
            );
            let cancelled = state.cancel_sequence_jobs(scope.0.as_deref(), parent, cause, &reason);
            (reason, cancelled)
        } else {
            match &scope.0 {
                Some(slot) => {
                    let reason = if state.slots.len() > 1 {
                        format!("Slot '{slot}' aborted by sequence '{path}' ({word})")
                    } else {
                        format!("Run aborted by sequence '{path}' ({word})")
                    };
                    let cancelled = state.cancel_slot_jobs(slot, cause, &reason);
                    (reason, cancelled)
                }
                None => {
                    let reason = format!("Run aborted by sequence '{path}' ({word})");
                    let cancelled = state.cancel_all_jobs(&reason, cause);
                    (reason, cancelled)
                }
            }
        };
        log::warn!("{reason}");
        let why = crate::state::not_run(&format!("{word} of sequence {path}"));
        self.emit_cancelled_work(&cancelled, &why).await;
    }
}
