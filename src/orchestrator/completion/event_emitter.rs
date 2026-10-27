use std::sync::Arc;

use crate::event_sink::{EventSink, ExecutionEvent};
use crate::job::{Job, JobResult, Outcome};

pub fn log_phase_completion(
    job: &Job,
    job_result: &JobResult,
    phase_outcome: Outcome,
    error_message: &Option<String>,
) {
    let duration_ms = (job_result.completed_at - job_result.started_at)
        .num_milliseconds()
        .max(0) as u64;
    let duration_secs = duration_ms as f64 / 1000.0;
    let success = phase_outcome == Outcome::Pass;

    let phase_label = if let Some(ref slot_id) = job.slot_id {
        format!("{} [{}]", job.phase_name, slot_id)
    } else {
        job.phase_name.clone()
    };

    if success {
        log::debug!("{} ({:.1}s)", phase_label, duration_secs);
    } else {
        // The run's own output reports the failure; this is the trace.
        log::debug!("{} failed ({:.1}s)", phase_label, duration_secs);
    }

    if !success {
        if let Some(ref error_msg) = error_message {
            log::debug!("  {}", error_msg);
        }
    }
}

pub fn log_resource_metrics(job: &Job, job_result: &JobResult) {
    if let Some(ref metrics) = job_result.resource_metrics {
        log::debug!(
            "Resource usage for '{}': CPU: {:.1}%, Memory: {:.1}MB peak, {:.1}MB avg, Processes: {}",
            job.phase_name,
            metrics.cpu_usage_percent,
            metrics.memory_peak_mb,
            metrics.memory_avg_mb,
            metrics.process_count
        );
    }
}

pub fn emit_job_complete_event(
    event_sink: &Arc<dyn EventSink>,
    event: &crate::orchestrator::JobCompletionEvent,
    job_result: &JobResult,
    phase_outcome: Outcome,
    error_message: Option<String>,
    is_retry_limit_exceeded: bool,
) {
    let (job_id, job, worker_id) = (event.job_id, &event.original_job, event.worker_id);
    let duration_ms = (job_result.completed_at - job_result.started_at)
        .num_milliseconds()
        .max(0) as u64;

    log::debug!(
        "Emitting job-complete for {}: outcome={:?}, is_retry_limit_exceeded={}",
        job.phase_name,
        phase_outcome,
        is_retry_limit_exceeded
    );

    event_sink.emit(&ExecutionEvent::JobComplete {
        job_id: job_id.to_string(),
        slot_id: job.slot_id.clone(),
        phase_key: job.phase_key.clone(),
        phase_name: job.phase_name.clone(),
        stage_scope: job.stage_scope,
        outcome: phase_outcome,
        action: format!("{:?}", job_result.phase_result),
        next_action: job_result.next_action.as_ref().map(|a| format!("{:?}", a)),
        requested_stop: job_result.requested_stop(),
        measurements: job_result.measurements.clone(),
        logs: job_result.logs.clone(),
        resource_metrics: job_result.resource_metrics.clone(),
        retry_count: job.retry_count,
        retry_limit: job.retry_limit,
        started_at: job_result.started_at.to_rfc3339(),
        completed_at: job_result.completed_at.to_rfc3339(),
        duration_ms,
        worker_id,
        error: error_message,
        run_metadata: job_result.run_metadata.clone(),
        unit_metadata: job_result.unit_metadata.clone(),
        sequence_path: job.sequence_path.clone(),
        failure_locals: job_result.failure_locals.clone(),
    });
}
