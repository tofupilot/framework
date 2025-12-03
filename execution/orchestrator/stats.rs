//! Statistics calculation and outcome determination

use std::collections::HashMap;

use tauri::AppHandle;
use tauri_specta::Event;
use uuid::Uuid;
use crate::execution::job::{JobResult, Outcome};
use crate::execution::state::JobInfo;

use super::orchestrator::{ExecutionStats, Orchestrator};

/// Key to identify a unique phase instance (phase_key + slot_id)
#[derive(Hash, Eq, PartialEq, Clone)]
struct PhaseInstanceKey {
    phase_key: String,
    slot_id: Option<String>,
}

impl Orchestrator {
    /// Get only the final attempt for each phase (highest retry_count per phase_key+slot_id)
    fn get_final_attempts<'a>(
        job_results: &'a HashMap<Uuid, JobResult>,
        job_info: &HashMap<Uuid, JobInfo>,
    ) -> Vec<&'a JobResult> {
        let mut best_per_phase: HashMap<PhaseInstanceKey, (usize, &'a JobResult)> = HashMap::new();

        for (job_id, result) in job_results {
            if let Some(info) = job_info.get(job_id) {
                let key = PhaseInstanceKey {
                    phase_key: info.phase_key.clone(),
                    slot_id: info.slot_id.clone(),
                };

                let dominated = best_per_phase
                    .get(&key)
                    .map(|(count, _)| result.retry_count <= *count)
                    .unwrap_or(false);

                if !dominated {
                    best_per_phase.insert(key, (result.retry_count, result));
                }
            }
        }

        best_per_phase.into_values().map(|(_, r)| r).collect()
    }

    /// Filter job results to only include jobs for a specific slot, returning final attempts only
    fn get_final_attempts_for_slot<'a>(
        job_results: &'a HashMap<Uuid, JobResult>,
        job_info: &HashMap<Uuid, JobInfo>,
        job_to_slot: &HashMap<Uuid, String>,
        slot_id: &str,
    ) -> Vec<&'a JobResult> {
        let mut best_per_phase: HashMap<String, (usize, &'a JobResult)> = HashMap::new();

        for (job_id, result) in job_results {
            if job_to_slot.get(job_id) != Some(&slot_id.to_string()) {
                continue;
            }

            if let Some(info) = job_info.get(job_id) {
                let dominated = best_per_phase
                    .get(&info.phase_key)
                    .map(|(count, _)| result.retry_count <= *count)
                    .unwrap_or(false);

                if !dominated {
                    best_per_phase.insert(info.phase_key.clone(), (result.retry_count, result));
                }
            }
        }

        best_per_phase.into_values().map(|(_, r)| r).collect()
    }

    pub async fn get_stats(&self) -> ExecutionStats {
        let state = self.state.read().await;
        let workers = self.workers.read().await;

        let failed_jobs = state
            .job_results
            .values()
            .filter(|r| r.is_failure())
            .count();

        let busy_workers = state.worker_state.count_busy();
        let running_jobs = busy_workers;

        let run_outcome = if state.is_complete() {
            let final_attempts = Self::get_final_attempts(&state.job_results, &state.job_info);
            Some(self.determine_aggregate_outcome(&final_attempts, state.shutdown_requested, &state.init_error))
        } else {
            None
        };

        let run_dir = {
            let report_managers_lock = self.report_managers.read().await;
            report_managers_lock
                .values()
                .next()
                .and_then(|manager| manager.get_current_report_dir_name())
        };

        let (slot_outcomes, slot_run_ids) = if state.is_complete() {
            let mut outcomes = HashMap::new();
            let mut run_ids = HashMap::new();

            let report_managers_lock = self.report_managers.read().await;

            for (slot_id, manager) in report_managers_lock.iter() {
                if let Some(run_id) = manager.get_current_run_id() {
                    run_ids.insert(slot_id.clone(), run_id);
                }

                let slot_final_attempts = Self::get_final_attempts_for_slot(
                    &state.job_results,
                    &state.job_info,
                    &state.job_to_slot,
                    slot_id,
                );

                let slot_outcome =
                    self.determine_aggregate_outcome(&slot_final_attempts, state.shutdown_requested, &state.init_error);
                outcomes.insert(slot_id.clone(), slot_outcome);
            }

            (outcomes, run_ids)
        } else {
            (HashMap::new(), HashMap::new())
        };

        ExecutionStats {
            total_jobs: state.total_jobs_submitted,
            completed_jobs: state.original_jobs_completed,
            failed_jobs,
            running_jobs,
            queued_jobs: state.job_queue.len(),
            workers_busy: busy_workers,
            workers_total: workers.len(),
            run_outcome,
            run_dir,
            run_id: Some(self.run_id.clone()),
            slot_outcomes,
            slot_run_ids,
            start_time: self.start_time,
            end_time: self.end_time,
        }
    }

    fn determine_aggregate_outcome(
        &self,
        job_results: &[&JobResult],
        shutdown_requested: bool,
        init_error: &Option<String>,
    ) -> Outcome {
        // Priority order: ERROR → STOP → TIMEOUT → FAIL → PASS
        // Only considers final attempts per phase (intermediate retries are excluded)

        if init_error.is_some() {
            return Outcome::Error;
        }

        let has_error = job_results.iter().any(|r| r.error.is_some());
        if has_error {
            return Outcome::Error;
        }

        if shutdown_requested {
            return Outcome::Stop;
        }

        let has_stop = job_results.iter().any(|r| r.should_stop_test());
        if has_stop {
            return Outcome::Stop;
        }

        let has_timeout = job_results.iter().any(|r| r.timeout_secs.is_some());
        if has_timeout {
            return Outcome::Timeout;
        }

        // Check phase_outcome for failures (includes retry limit exceeded)
        let has_failure = job_results
            .iter()
            .any(|r| r.is_failure() || matches!(r.phase_outcome, Outcome::Fail));
        if has_failure {
            return Outcome::Fail;
        }

        Outcome::Pass
    }

    pub(super) async fn emit_stats(&self, app_handle: Option<&AppHandle>) {
        if let Some(app) = app_handle {
            let stats = self.get_stats().await;

            // Calculate percentage progress
            let percentage = if stats.total_jobs > 0 {
                ((stats.completed_jobs as f32 / stats.total_jobs as f32) * 100.0) as u32
            } else {
                0
            };

            // Emit both execution-stats and execution-progress events
            let _ = super::orchestrator::ExecutionStatsEvent(stats.clone()).emit(app);
            let _ = super::orchestrator::OrchestratorExecutionProgressEvent {
                execution_id: "current".to_string(),
                stats: serde_json::json!({
                    "total_jobs": stats.total_jobs,
                    "completed_jobs": stats.completed_jobs,
                    "failed_jobs": stats.failed_jobs,
                    "running_jobs": stats.running_jobs,
                    "queued_jobs": stats.queued_jobs,
                    "percentage": percentage,
                    "workers_busy": stats.workers_busy,
                    "workers_total": stats.workers_total,
                    "start_time": stats.start_time,
                    "end_time": stats.end_time,
                }),
            }
            .emit(app);
        }
    }
}

