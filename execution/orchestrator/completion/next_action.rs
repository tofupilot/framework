use crate::execution::job::{JobResult, Outcome, PhaseResult};
use crate::procedure::schema::{PhaseDefinition, PhaseNextAction};

pub fn determine_next_action(
    job_result: &JobResult,
    phase_outcome: &Outcome,
    phase_def: Option<&PhaseDefinition>,
) -> PhaseNextAction {
    if matches!(job_result.phase_result, PhaseResult::Retry) {
        return PhaseNextAction::Retry;
    }

    let is_terminal = job_result.error.is_some()
        || job_result.timeout_secs.is_some()
        || matches!(job_result.phase_result, PhaseResult::Stop);

    if is_terminal {
        if let Some(def) = phase_def {
            get_next_action_for_terminal(phase_outcome, def)
        } else {
            PhaseNextAction::Stop
        }
    } else if let Some(def) = phase_def {
        get_next_action_for_non_terminal(phase_outcome, def)
    } else {
        match phase_outcome {
            Outcome::Pass
            | Outcome::Skip
            | Outcome::Fail
            | Outcome::Timeout
            | Outcome::Aborted => PhaseNextAction::Continue,
            Outcome::Error => PhaseNextAction::Stop,
        }
    }
}

fn get_next_action_for_terminal(outcome: &Outcome, phase_def: &PhaseDefinition) -> PhaseNextAction {
    if let Some(then_config) = &phase_def.then {
        let configured = match outcome {
            Outcome::Fail => then_config.fail.clone(),
            Outcome::Error => then_config.error.clone(),
            _ => None,
        };

        if let Some(next_action) = configured {
            log::debug!(
                "Phase '{}': Terminal result, using then.{:?}: {:?}",
                phase_def.name,
                outcome,
                next_action
            );
            return next_action;
        }
    }

    log::debug!(
        "Phase '{}': Terminal result, using default: Stop",
        phase_def.name
    );
    PhaseNextAction::Stop
}

fn get_next_action_for_non_terminal(
    outcome: &Outcome,
    phase_def: &PhaseDefinition,
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
            log::debug!(
                "Phase '{}': Non-terminal, using then.{:?}: {:?}",
                phase_def.name,
                outcome,
                next_action
            );
            return next_action;
        }
    }

    let default_action = match outcome {
        Outcome::Pass | Outcome::Skip | Outcome::Fail | Outcome::Timeout | Outcome::Aborted => {
            PhaseNextAction::Continue
        }
        Outcome::Error => PhaseNextAction::Stop,
    };

    log::debug!(
        "Phase '{}': Non-terminal, using default for {:?}: {:?}",
        phase_def.name,
        outcome,
        default_action
    );

    default_action
}
