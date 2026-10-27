use crate::job::{Job, JobResult, Outcome, PhaseResult};

/// `cut_short`: the stop of this job's scope (the execution, or its own
/// slot) cut the phase: it ended with the cut's error. It reads Stop. A phase that returned on its
/// own under the stop is not cut and keeps its verdict; a sibling slot's
/// phase is untouched.
pub fn resolve_outcome(job_result: &JobResult, job: &Job, cut_short: bool) -> (Outcome, bool) {
    let is_retry_limit_exceeded = job_result.phase_result == PhaseResult::Retry && !job.can_retry();

    log_measurement_validation(job, job_result);

    let measurements_pass =
        crate::measurements::evaluation::check_all_measurements_pass(&job_result.measurements);

    log::debug!(
        "Phase '{}' measurements_pass = {}",
        job.phase_name,
        measurements_pass
    );

    if !measurements_pass {
        log::debug!(
            "Phase '{}' measurements failed critical validation",
            job.phase_name
        );
    }

    let phase_outcome = Outcome::from_execution(
        &job_result.phase_result,
        job_result.timeout_secs,
        job_result.error.as_ref(),
        measurements_pass,
        cut_short,
        job.can_retry(),
    );

    (phase_outcome, is_retry_limit_exceeded)
}

fn log_measurement_validation(job: &Job, job_result: &JobResult) {
    log::debug!(
        "Phase '{}' has {} measurements to validate",
        job.phase_name,
        job_result.measurements.len()
    );

    for measurement in &job_result.measurements {
        if let Some(validators) = &measurement.validators {
            log::debug!(
                "  Measurement '{}': {} validators",
                measurement.name,
                validators.len()
            );
            for validator in validators {
                log::debug!(
                    "    Validator operator={:?}, outcome={:?}",
                    validator.operator,
                    validator.outcome
                );
            }
        }
    }
}

pub fn format_error_message(
    is_retry_limit_exceeded: bool,
    retry_limit: usize,
    job_result: &JobResult,
) -> Option<String> {
    if is_retry_limit_exceeded {
        Some(format!(
            "Phase exceeded retry limit ({} retries)",
            retry_limit
        ))
    } else if let Some(ref e) = job_result.error {
        Some(e.clone())
    } else {
        match (job_result.timeout_ms, job_result.timeout_secs) {
            (Some(ms), _) => Some(format!(
                "Timed out after {}",
                crate::procedure::schema::format_duration(ms)
            )),
            (None, Some(secs)) => Some(format!("Timed out after {secs}s")),
            (None, None) if job_result.phase_outcome == crate::job::Outcome::Timeout => {
                Some("Timed out".to_string())
            }
            (None, None) => None,
        }
    }
}
