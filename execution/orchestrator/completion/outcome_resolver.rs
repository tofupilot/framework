use crate::execution::job::{Job, JobResult, Outcome, PhaseResult};

pub fn resolve_outcome(
    job_result: &JobResult,
    job: &Job,
    shutdown_requested: bool,
) -> (Outcome, bool) {
    let is_retry_limit_exceeded =
        job_result.phase_result == PhaseResult::Retry && !job.can_retry();

    log_measurement_validation(job, job_result);

    let measurements_pass =
        crate::measurements::evaluation::check_all_measurements_pass(&job_result.measurements);

    log::debug!(
        "Phase '{}' measurements_pass = {}",
        job.phase_name, measurements_pass
    );

    if !measurements_pass {
        log::warn!(
            "Phase '{}' measurements failed critical validation",
            job.phase_name
        );
    }

    let phase_outcome = Outcome::from_execution(
        &job_result.phase_result,
        job_result.timeout_secs,
        job_result.error.as_ref(),
        measurements_pass,
        shutdown_requested,
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
                    validator.operator, validator.outcome
                );
            }
        }
    }
}

pub fn format_error_message(
    is_retry_limit_exceeded: bool,
    job_result: &JobResult,
) -> Option<String> {
    if is_retry_limit_exceeded {
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
    }
}
