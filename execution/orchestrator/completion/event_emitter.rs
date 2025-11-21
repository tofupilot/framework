use tauri::AppHandle;
use tauri_specta::Event;
use uuid::Uuid;

use crate::execution::job::{Job, JobResult, Outcome};

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
        log::info!("{} ({:.1}s)", phase_label, duration_secs);
    } else {
        log::error!("{} ({:.1}s)", phase_label, duration_secs);
    }

    if !success {
        if let Some(ref error_msg) = error_message {
            log::error!("  {}", error_msg);
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
    app: &AppHandle,
    job_id: Uuid,
    job: &Job,
    job_result: &JobResult,
    phase_outcome: Outcome,
    error_message: Option<String>,
    worker_id: usize,
    attachments: Vec<String>,
    is_retry_limit_exceeded: bool,
) {
    let duration_ms = (job_result.completed_at - job_result.started_at)
        .num_milliseconds()
        .max(0) as u64;

    log::debug!(
        "Emitting job-complete for {}: outcome={:?}, is_retry_limit_exceeded={}",
        job.phase_name, phase_outcome, is_retry_limit_exceeded
    );

    let job_complete_event = crate::execution::orchestrator::orchestrator::JobCompleteEvent {
        job_id: job_id.to_string(),
        slot_id: job.slot_id.clone(),
        phase_key: job.phase_key.clone(),
        phase_name: job.phase_name.clone(),
        stage_scope: job.stage_scope.clone(),
        outcome: phase_outcome,
        action: format!("{:?}", job_result.phase_result),
        next_action: job_result.next_action.as_ref().map(|a| format!("{:?}", a)),
        measurements: job_result.measurements.clone(),
        attachments,
        logs: job_result.logs.clone(),
        resource_metrics: job_result.resource_metrics.clone(),
        retry_count: job.retry_count,
        retry_limit: job.retry_limit,
        started_at: job_result.started_at.to_rfc3339(),
        completed_at: job_result.completed_at.to_rfc3339(),
        duration_ms,
        worker_id,
        error: error_message,
    };

    let _ = crate::execution::orchestrator::orchestrator::JobCompleteEventWrapper(
        job_complete_event,
    )
    .emit(app);
}
