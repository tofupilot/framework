use crate::execution::job::{JobResult, Outcome, PhaseResult};
use crate::procedure::schema::{PhaseDefinition, PhaseNextAction};

pub fn determine_next_action(
    job_result: &JobResult,
    phase_outcome: &Outcome,
    phase_def: Option<&PhaseDefinition>,
    should_stop_on_first_failure: bool,
) -> PhaseNextAction {
    if matches!(phase_outcome, Outcome::Error | Outcome::Timeout | Outcome::Stop) {
        return PhaseNextAction::Stop;
    }

    if matches!(job_result.phase_result, PhaseResult::Retry) && matches!(phase_outcome, Outcome::Retry) {
        return PhaseNextAction::Retry;
    }

    if let Some(def) = phase_def {
        get_next_action_for_phase(phase_outcome, def, should_stop_on_first_failure)
    } else {
        match phase_outcome {
            Outcome::Pass | Outcome::Skip | Outcome::Retry => PhaseNextAction::Continue,
            Outcome::Fail => {
                if should_stop_on_first_failure {
                    PhaseNextAction::Stop
                } else {
                    PhaseNextAction::Continue
                }
            }
            Outcome::Error | Outcome::Timeout | Outcome::Stop => PhaseNextAction::Stop,
        }
    }
}

fn get_next_action_for_phase(
    outcome: &Outcome,
    phase_def: &PhaseDefinition,
    should_stop_on_first_failure: bool,
) -> PhaseNextAction {
    // Check for explicit then.* configuration first
    if let Some(then_config) = &phase_def.then {
        let configured = match outcome {
            Outcome::Pass => then_config.pass.clone(),
            Outcome::Fail => then_config.fail.clone(),
            Outcome::Timeout => then_config.timeout.clone().or(then_config.error.clone()),
            Outcome::Skip => None,
            Outcome::Retry => None,
            Outcome::Error | Outcome::Stop => None,
        };

        if let Some(next_action) = configured {
            log::debug!(
                "Phase '{}': Using configured then.{:?}: {:?}",
                phase_def.name,
                outcome,
                next_action
            );
            return next_action;
        }
    }

    // Apply default behavior, respecting global on_first_failure setting
    let default_action = match outcome {
        Outcome::Pass | Outcome::Skip => PhaseNextAction::Continue,
        Outcome::Fail => {
            if should_stop_on_first_failure {
                PhaseNextAction::Stop
            } else {
                PhaseNextAction::Continue
            }
        }
        Outcome::Retry => PhaseNextAction::Retry,
        Outcome::Error | Outcome::Timeout | Outcome::Stop => PhaseNextAction::Stop,
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
