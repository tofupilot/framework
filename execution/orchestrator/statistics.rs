//! Statistics calculation and outcome determination
//!
//! This module handles:
//! - Execution statistics aggregation
//! - Per-slot outcome calculation
//! - Overall run outcome determination (following OpenHTF priority)
//! - Statistics event emission to UI

use std::collections::HashMap;
use tauri::{AppHandle, Emitter};

use super::{ExecutionStats, Orchestrator};
use crate::execution::job::JobResult;
use crate::execution::job::Outcome;

impl Orchestrator {
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
            let all_jobs: Vec<_> = state.job_results.values().collect();
            Some(self.determine_aggregate_outcome(&all_jobs, state.shutdown_requested))
        } else {
            None
        };

        let run_dir = {
            let report_managers_lock = self.report_managers.read().await;
            report_managers_lock
                .values()
                .next()
                .and_then(|manager| manager.get_current_run_dir_name())
        };

        let (slot_outcomes, slot_run_ids) = if state.is_complete() {
            let mut outcomes = HashMap::new();
            let mut run_ids = HashMap::new();

            let report_managers_lock = self.report_managers.read().await;

            for (slot_id, manager) in report_managers_lock.iter() {
                // Only process actual slot reports

                if let Some(run_id) = manager.get_current_run_id() {
                    run_ids.insert(slot_id.clone(), run_id);
                }

                let slot_jobs: Vec<_> = state
                    .job_results
                    .iter()
                    .filter(|(job_id, _)| state.job_to_slot.get(*job_id) == Some(slot_id))
                    .map(|(_, result)| result)
                    .collect();

                let slot_outcome =
                    self.determine_aggregate_outcome(&slot_jobs, state.shutdown_requested);
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
            run_id: self.run_id.clone(),
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
    ) -> Outcome {
        // Priority order: ERROR → ABORTED → TIMEOUT → FAIL → PASS

        let has_error = job_results.iter().any(|r| r.error.is_some());
        if has_error {
            return Outcome::Error;
        }

        if shutdown_requested {
            return Outcome::Aborted;
        }

        let has_stop = job_results.iter().any(|r| r.should_stop_test());
        if has_stop {
            return Outcome::Aborted;
        }

        let has_timeout = job_results.iter().any(|r| r.timeout_secs.is_some());
        if has_timeout {
            return Outcome::Timeout;
        }

        let has_failure = job_results.iter().any(|r| r.is_failure());
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
            let _ = app.emit("execution-stats", &stats);
            let _ = app.emit(
                "execution-progress",
                serde_json::json!({
                    "execution_id": "current",
                    "stats": {
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
                    }
                }),
            );
        }
    }
}
