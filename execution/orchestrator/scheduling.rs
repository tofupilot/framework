use serde_json;
use std::collections::HashMap;
use tauri::{AppHandle, Emitter};

use crate::execution::cli_output;
use crate::execution::constants::scheduling;
use crate::execution::job::Outcome;
use crate::execution::job::{Job, JobResult};
use crate::schema::procedure::StageScope;

use super::ExecutionStats;
use super::Orchestrator;

impl Orchestrator {
    pub async fn execute_all(
        &mut self,
        app_handle: Option<AppHandle>,
    ) -> Result<ExecutionStats, String> {
        // Set start time
        self.start_time = Some(chrono::Utc::now());

        let mut completion_rx = self
            .completion_rx
            .take()
            .ok_or("Completion receiver already taken")?;

        self.emit_stats(app_handle.as_ref()).await;

        self.schedule_available_jobs(app_handle.clone()).await?;

        loop {
            let state = self.state.read().await;

            if state.force_kill_requested {
                cli_output::error("Force kill requested, breaking out of execution loop".to_string());
                drop(state);
                break;
            }

            let has_stop = state.job_results.values().any(|r| r.should_stop_test());

            if state.shutdown_requested || has_stop {
                for worker_id in 0..state.worker_state.num_workers() {
                    if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                        if let Some((phase_key, phase_name, slot_id)) = state.job_info.get(&job_id) {
                            let slot = slot_id.clone().unwrap_or_else(|| "<shared>".to_string());

                            if let Some(ref app) = app_handle {
                                let _ = app.emit(
                                    "job-progress",
                                    serde_json::json!({
                                        "execution_id": self.execution_id,
                                        "job_id": job_id.to_string(),
                                        "slot_id": slot,
                                        "phase_key": phase_key,
                                        "phase_name": phase_name,
                                        "status": "stopping",
                                        "outcome": None::<String>,
                                        "error": None::<String>,
                                        "worker_id": Some(worker_id),
                                    }),
                                );
                            }
                        }
                    }
                }

                // Emit skipped status for queued jobs, but NOT for teardown phases
                for job in state.job_queue.iter() {
                    // Skip teardown phases - they will run after shutdown
                    if matches!(
                        job.stage_scope,
                        crate::schema::procedure::StageScope::TeardownEach
                            | crate::schema::procedure::StageScope::TeardownAll
                    ) {
                        continue;
                    }

                    let slot = job
                        .slot_id
                        .clone()
                        .unwrap_or_else(|| "<shared>".to_string());

                    if let Some(ref app) = app_handle {
                        let _ = app.emit(
                            "job-progress",
                            serde_json::json!({
                                "execution_id": self.execution_id,
                                "job_id": job.id.to_string(),
                                "slot_id": slot,
                                "phase_name": job.phase_name,
                                "status": "skipped",
                                "outcome": "skip",
                                "error": None::<String>,
                                "worker_id": None::<usize>,
                            }),
                        );
                    }
                }

                break;
            }

            if state.is_complete() {
                break;
            }
            drop(state);

            tokio::select! {
                Some(event) = completion_rx.recv() => {
                    let should_continue = self.handle_job_completion(event, app_handle.clone()).await;
                    if should_continue {
                        self.schedule_available_jobs(app_handle.clone()).await?;
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(scheduling::IDLE_POLL_DELAY_MS)) => {
                    self.schedule_available_jobs(app_handle.clone()).await?;
                }
            }
        }

        // Auto-teardown: Destroy all remaining plugs at procedure end
        let resource_manager = self.resource_manager.write().await;

        // Destroy all slot-level plugs
        let state = self.state.read().await;
        let slot_ids: Vec<String> = state.slot_jobs.keys().cloned().collect();
        drop(state);

        for slot_id in slot_ids {
            if resource_manager.has_each_scope_plugs(&slot_id).await {
                match resource_manager
                    .destroy_each_scope_plugs(slot_id.clone(), app_handle.as_ref())
                    .await
                {
                    Ok(_) => {
                        self.emit_plug_scope_event("pass").await;
                    }
                    Err(e) => {
                        crate::cli_output::warning(format!(
                            "Failed to auto-destroy each-scope plugs for {}: {}",
                            slot_id, e
                        ));
                        self.emit_plug_scope_event("error").await;
                    }
                }
            }
        }

        // Destroy all all-scope plugs
        if resource_manager.has_all_scope_plugs().await {
            match resource_manager
                .destroy_all_scope_plugs(app_handle.as_ref())
                .await
            {
                Ok(_) => {
                    self.emit_plug_scope_event("pass").await;
                }
                Err(e) => {
                    crate::cli_output::warning(format!(
                        "Failed to auto-destroy all-scope plugs: {}",
                        e
                    ));
                    self.emit_plug_scope_event("error").await;
                }
            }
        }

        drop(resource_manager);

        // Set end time
        self.end_time = Some(chrono::Utc::now());

        // Emit final stats after all plug teardown completes
        self.emit_stats(app_handle.as_ref()).await;

        let stats = self.get_stats().await;

        {
            let mut report_managers = self.report_managers.write().await;
            let state = self.state.read().await;

            let shared_job_results: HashMap<uuid::Uuid, JobResult> = state
                .job_results
                .iter()
                .filter(|(job_id, _)| {
                    state
                        .job_info
                        .get(job_id)
                        .map(|(_, _, sid)| sid.is_none())
                        .unwrap_or(false)
                })
                .map(|(id, result)| (*id, result.clone()))
                .collect();

            for (slot_id, report_manager) in report_managers.iter_mut() {
                // Only process actual slot reports (shared phases are included in each slot's report)

                let mut slot_job_results: HashMap<uuid::Uuid, JobResult> = state
                    .job_results
                    .iter()
                    .filter(|(job_id, _)| {
                        state
                            .job_info
                            .get(job_id)
                            .map(|(_, _, sid)| sid.as_deref() == Some(slot_id))
                            .unwrap_or(false)
                    })
                    .map(|(id, result)| (*id, result.clone()))
                    .collect();

                for (job_id, result) in &shared_job_results {
                    slot_job_results.insert(*job_id, result.clone());
                }

                let slot_stats = ExecutionStats {
                    total_jobs: slot_job_results.len(),
                    completed_jobs: slot_job_results
                        .iter()
                        .filter(|(_, r)| r.error.is_none() && r.timeout_secs.is_none())
                        .count(),
                    failed_jobs: slot_job_results
                        .iter()
                        .filter(|(_, r)| r.error.is_some() || r.timeout_secs.is_some())
                        .count(),
                    running_jobs: 0,
                    queued_jobs: 0,
                    workers_busy: 0,
                    workers_total: stats.workers_total,
                    run_outcome: stats.run_outcome,
                    run_dir: None,
                    run_id: None,
                    slot_outcomes: HashMap::new(),
                    slot_run_ids: HashMap::new(),
                    start_time: stats.start_time,
                    end_time: stats.end_time,
                };

                if let Err(e) =
                    report_manager.finalize_run(&slot_stats, &slot_job_results, &state.job_info)
                {
                    cli_output::print_section(
                        cli_output::Section::Error,
                        format!("Failed to generate test report for slot {}: {}", slot_id, e),
                    );
                }
            }
        }

        if let Some(ref app) = app_handle {
            let _ = app.emit("execution-complete", &stats);
        }

        match stats.run_outcome {
            Some(Outcome::Pass) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run PASSED: {} jobs processed", stats.total_jobs),
            ),
            Some(Outcome::Fail) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run FAILED: {} jobs processed", stats.total_jobs),
            ),
            Some(Outcome::Error) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run ERROR: {} jobs processed", stats.total_jobs),
            ),
            Some(Outcome::Skip) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run SKIP: {} jobs processed", stats.total_jobs),
            ),
            Some(Outcome::Timeout) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run TIMEOUT: {} jobs processed", stats.total_jobs),
            ),
            Some(Outcome::Aborted) => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Run ABORTED: {} jobs processed", stats.total_jobs),
            ),
            None => cli_output::print_section(
                cli_output::Section::Summary,
                format!("Execution complete: {} jobs processed", stats.total_jobs),
            ),
        }

        Ok(stats)
    }

    async fn schedule_available_jobs(
        &mut self,
        app_handle: Option<AppHandle>,
    ) -> Result<(), String> {
        loop {
            let state = self.state.read().await;
            if state.shutdown_requested {
                return Ok(());
            }

            let has_stop = state.job_results.values().any(|r| r.should_stop_test());
            drop(state);

            if has_stop {
                return Ok(());
            }

            let permit = match self.job_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => break,
            };

            let worker_to_use = {
                let state = self.state.read().await;
                if let Some(idle_worker_id) = state.worker_state.get_idle_worker() {
                    let workers = self.workers.read().await;
                    workers
                        .get(idle_worker_id)
                        .map(|w| (idle_worker_id, w.clone()))
                } else {
                    None
                }
            };

            let Some((worker_id, worker)) = worker_to_use else {
                drop(permit);
                break;
            };

            let job = self.get_next_ready_job().await;

            if let Some(job) = job {
                self.ensure_plugs_created_for_job(&job, app_handle.as_ref())
                    .await?;

                {
                    let mut state = self.state.write().await;
                    if let Err(e) = state.mark_job_active(job.id, worker_id) {
                        cli_output::print_section(
                            cli_output::Section::Error,
                            format!("Failed to mark job active: {}", e),
                        );
                        drop(permit);
                        break;
                    }
                }
                self.spawn_job_execution(job, worker_id, worker, app_handle.clone(), permit)
                    .await?;
                self.emit_stats(app_handle.as_ref()).await;
            } else {
                drop(permit);
                break;
            }
        }

        {
            let mut state = self.state.write().await;
            if state.check_and_queue_next_slot() {
                drop(state);
                Box::pin(self.schedule_available_jobs(app_handle)).await?;
            }
        }

        Ok(())
    }

    async fn get_next_ready_job(&self) -> Option<Job> {
        let mut state = self.state.write().await;
        let resource_manager = self.resource_manager.read().await;

        let mut checked_jobs = Vec::new();
        let mut ready_job = None;

        while let Some(job) = state.job_queue.pop_front() {
            if !job.dependencies_satisfied(&state.completed_jobs) {
                checked_jobs.push(job);
                continue;
            }

            if matches!(job.stage_scope, StageScope::TeardownEach) {
                if let Some(slot_id) = &job.slot_id {
                    let has_pending_main_phases = state.job_queue.iter().any(|queued_job| {
                        queued_job.slot_id.as_ref() == Some(slot_id)
                            && matches!(queued_job.stage_scope, StageScope::Main)
                    });

                    let has_pending_main_in_checked = checked_jobs.iter().any(|queued_job| {
                        queued_job.slot_id.as_ref() == Some(slot_id)
                            && matches!(queued_job.stage_scope, StageScope::Main)
                    });

                    if has_pending_main_phases || has_pending_main_in_checked {
                        crate::cli_output::debug(format!(
                            "Teardown phase '{}' waiting for main phases in slot {} to complete",
                            job.phase_name, slot_id
                        ));
                        checked_jobs.push(job);
                        continue;
                    }

                    let has_running_slot_jobs =
                        (0..state.worker_state.num_workers()).any(|worker_id| {
                            if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                                state
                                    .job_to_slot
                                    .get(&job_id)
                                    .map(|running_slot| running_slot == slot_id)
                                    .unwrap_or(false)
                            } else {
                                false
                            }
                        });

                    if has_running_slot_jobs {
                        checked_jobs.push(job);
                        continue;
                    }
                }
            }

            if !job.required_plugs.is_empty() {
                let available = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        resource_manager
                            .can_allocate_resources(&job.required_plugs)
                            .await
                    })
                });
                if !available {
                    checked_jobs.push(job);
                    continue;
                } else {
                    crate::cli_output::debug(format!(
                        "Job {} plugs {:?} available",
                        job.phase_name, job.required_plugs
                    ));
                }
            }

            ready_job = Some(job);
            break;
        }

        for job in checked_jobs.into_iter().rev() {
            state.job_queue.push_front(job);
        }

        ready_job
    }
}
