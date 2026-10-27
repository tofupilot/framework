use crate::job::{JobResult, Outcome, PhaseResult};
use crate::procedure::schema::{PhaseDefinition, PhaseNextAction};

/// `can_retry` is the job's remaining retry budget. A `then: …: retry`
/// is only honoured while it is true; once the budget is spent the
/// outcome takes the same path as any other failure (`on_first_failure`,
/// or the phase's other `then` entries), instead of continuing the run
/// unconditionally.
pub fn determine_next_action(
    job_result: &JobResult,
    phase_outcome: &Outcome,
    phase_def: Option<&PhaseDefinition>,
    should_stop_on_first_failure: bool,
    can_retry: bool,
) -> PhaseNextAction {
    // An explicit `phase.stop()` always ends the scope, even when a failed
    // measurement made the outcome FAIL rather than STOP.
    if matches!(phase_outcome, Outcome::Stop)
        || matches!(job_result.phase_result, PhaseResult::Stop)
    {
        return PhaseNextAction::Stop;
    }

    if matches!(job_result.phase_result, PhaseResult::Retry)
        && matches!(phase_outcome, Outcome::Retry)
    {
        return PhaseNextAction::Retry;
    }

    if let Some(def) = phase_def {
        get_next_action_for_phase(phase_outcome, def, should_stop_on_first_failure, can_retry)
    } else {
        match phase_outcome {
            Outcome::Pass | Outcome::Skip | Outcome::Retry => PhaseNextAction::Continue,
            // Fail / Error / Timeout all respect `on_first_failure`. A crash in
            // one phase shouldn't silently abort sibling phases (especially
            // ones waiting on operator input) when the procedure asked to
            // continue past failures. `Stop` is the explicit operator-cancel
            // outcome and always halts.
            Outcome::Fail | Outcome::Error | Outcome::Timeout => {
                if should_stop_on_first_failure {
                    PhaseNextAction::Stop
                } else {
                    PhaseNextAction::Continue
                }
            }
            Outcome::Stop => PhaseNextAction::Stop,
        }
    }
}

fn get_next_action_for_phase(
    outcome: &Outcome,
    phase_def: &PhaseDefinition,
    should_stop_on_first_failure: bool,
    can_retry: bool,
) -> PhaseNextAction {
    // Check for explicit then.* configuration first
    if let Some(then_config) = &phase_def.then {
        let configured = match outcome {
            Outcome::Pass => then_config.pass.clone(),
            Outcome::Fail => then_config.fail.clone(),
            Outcome::Timeout => then_config.timeout.clone(),
            Outcome::Error => then_config.error.clone(),
            Outcome::Skip => None,
            Outcome::Retry => None,
            Outcome::Stop => None,
        };

        if let Some(next_action) =
            configured.filter(|a| can_retry || !matches!(a, PhaseNextAction::Retry))
        {
            log::debug!(
                "Phase '{}': Using configured then.{:?}: {:?}",
                phase_def.name,
                outcome,
                next_action
            );
            return next_action;
        }
    }

    // Apply default behavior, respecting global on_first_failure setting.
    // `Error` and `Timeout` go through the same `on_first_failure` gate as
    // `Fail`: a crashed or timed-out phase shouldn't silently abort sibling
    // phases that are still in flight (e.g. waiting on operator input) when
    // the procedure asked to keep running. `Stop` is the explicit cancel
    // outcome from `phase.stop()` and always halts.
    let default_action = match outcome {
        Outcome::Pass | Outcome::Skip => PhaseNextAction::Continue,
        Outcome::Fail | Outcome::Error | Outcome::Timeout => {
            if should_stop_on_first_failure {
                PhaseNextAction::Stop
            } else {
                PhaseNextAction::Continue
            }
        }
        Outcome::Retry => PhaseNextAction::Retry,
        Outcome::Stop => PhaseNextAction::Stop,
    };

    log::debug!(
        "Phase '{}': Using default for {:?}: {:?} (stop_on_first_failure={})",
        phase_def.name,
        outcome,
        default_action,
        should_stop_on_first_failure
    );

    default_action
}
