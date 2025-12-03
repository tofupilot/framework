use std::collections::HashMap;
use tauri::AppHandle;
use tauri_specta::Event;

use crate::execution::constants::scheduling;
use crate::execution::job::{Job, JobResult, JobStatus, Outcome};
use crate::procedure::schema::StageScope;

use super::orchestrator::ExecutionStats;
use super::orchestrator::Orchestrator;

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

        // Track whether we've already emitted shutdown events (to avoid duplicates)
        let mut shutdown_events_emitted = false;

        loop {
            let state = self.state.read().await;

            if state.force_kill_requested {
                log::error!("Force kill requested, breaking out of execution loop");
                drop(state);
                break;
            }

            let has_stop = state.job_results.values().any(|r| r.should_stop_test());
            let busy_workers = state.worker_state.count_busy();

            if state.shutdown_requested || has_stop {
                // Only emit shutdown events once
                if !shutdown_events_emitted {
                    shutdown_events_emitted = true;

                    // Emit stopping status for running jobs
                    for worker_id in 0..state.worker_state.num_workers() {
                        if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                            if let Some(info) = state.job_info.get(&job_id) {
                                if let Some(ref app) = app_handle {
                                    let progress = super::orchestrator::JobProgress {
                                        job_id: job_id.to_string(),
                                        slot_id: info.slot_id.clone(),
                                        phase_key: info.phase_key.clone(),
                                        phase_name: info.phase_name.clone(),
                                        stage_scope: StageScope::Main,
                                        status: JobStatus::Stopping,
                                        worker_id: Some(worker_id),
                                        started_at: None,
                                        timeout_ms: None,
                                        outcome: None,
                                        retry_count: 0,
                                        error: None,
                                    };
                                    let _ = super::orchestrator::JobProgressEvent(progress).emit(app);
                                }
                            }
                        }
                    }

                    // Emit skipped status for queued jobs, but NOT for teardown phases
                    for job in state.job_queue.iter() {
                        // Skip teardown phases - they will run after shutdown
                        if matches!(
                            job.stage_scope,
                            crate::procedure::schema::StageScope::TeardownEach
                                | crate::procedure::schema::StageScope::TeardownAll
                        ) {
                            continue;
                        }

                        if let Some(ref app) = app_handle {
                            let progress = super::orchestrator::JobProgress {
                                job_id: job.id.to_string(),
                                slot_id: job.slot_id.clone(),
                                phase_key: job.phase_key.clone(),
                                phase_name: job.phase_name.clone(),
                                stage_scope: job.stage_scope.clone(),
                                status: JobStatus::Skipped,
                                worker_id: None,
                                started_at: None,
                                timeout_ms: job.timeout_ms,
                                outcome: Some(Outcome::Skip),
                                retry_count: job.retry_count,
                                error: None,
                            };
                            let _ = super::orchestrator::JobProgressEvent(progress).emit(app);
                        }
                    }
                }

                // If there are still running jobs, wait for them to complete before breaking
                // This ensures their results are recorded in the report
                // Break when execution is complete (queue empty, no busy workers)
                if busy_workers == 0 && state.is_complete() {
                    break;
                }
                // Don't check is_complete() here - we need to wait for running jobs
                drop(state);
            } else {
                if state.is_complete() {
                    break;
                }
                drop(state);
            }

            // Clean up finished delayed retry task handles
            {
                let mut state = self.state.write().await;
                state.cleanup_finished_retry_handles();
            }

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
                        log::warn!(
                            "Failed to auto-destroy each-scope plugs for {}: {}",
                            slot_id, e
                        );
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
                    log::warn!(
                        "Failed to auto-destroy all-scope plugs: {}",
                        e
                    );
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

            let mut shared_jobs = Vec::new();
            let mut slot_jobs: HashMap<&str, Vec<(uuid::Uuid, JobResult)>> = HashMap::new();

            for (job_id, result) in &state.job_results {
                if let Some(info) = state.job_info.get(job_id) {
                    if let Some(slot_id) = &info.slot_id {
                        slot_jobs.entry(slot_id.as_str())
                            .or_default()
                            .push((*job_id, result.clone()));
                    } else {
                        shared_jobs.push((*job_id, result.clone()));
                    }
                }
            }

            for (slot_id, report_manager) in report_managers.iter_mut() {
                let slot_job_results: HashMap<uuid::Uuid, JobResult> = shared_jobs.iter()
                    .chain(slot_jobs.get(slot_id.as_str()).map(|v| v.as_slice()).unwrap_or(&[]))
                    .map(|(id, result)| (*id, result.clone()))
                    .collect();

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
                    report_manager.finalize_report(&slot_stats, &slot_job_results, &state.job_info)
                {
                    log::error!("Failed to generate test report for slot {}: {}", slot_id, e);
                }
            }
        }

        if let Some(ref app) = app_handle {
            let _ = super::orchestrator::ExecutionCompleteEvent(stats.clone()).emit(app);
        }

        match stats.run_outcome {
            Some(Outcome::Pass) => log::info!("Run PASSED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Fail) => log::info!("Run FAILED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Error) => log::info!("Run ERROR: {} jobs processed", stats.total_jobs),
            Some(Outcome::Skip) => log::info!("Run SKIP: {} jobs processed", stats.total_jobs),
            Some(Outcome::Timeout) => log::info!("Run TIMEOUT: {} jobs processed", stats.total_jobs),
            Some(Outcome::Stop) => log::info!("Run STOPPED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Retry) => log::info!("Run RETRY: {} jobs processed", stats.total_jobs),
            None => log::info!("Execution complete: {} jobs processed", stats.total_jobs),
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
            drop(state);

            // Note: We don't check has_stop here because teardown phases should still run
            // after a stop is triggered. cancel_all_jobs preserves teardown phases in the queue.

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
                if let Err(e) = self.ensure_plugs_created_for_job(&job, app_handle.as_ref()).await {
                    log::error!("Plug initialization failed: {}", e);
                    {
                        let mut state = self.state.write().await;
                        state.init_error = Some(e.clone());
                        state.shutdown_requested = true;
                    }
                    if let Some(ref app) = app_handle {
                        let progress = super::orchestrator::JobProgress {
                            job_id: job.id.to_string(),
                            slot_id: job.slot_id.clone(),
                            phase_key: job.phase_key.clone(),
                            phase_name: job.phase_name.clone(),
                            stage_scope: job.stage_scope.clone(),
                            status: JobStatus::Skipped,
                            worker_id: None,
                            started_at: None,
                            timeout_ms: job.timeout_ms,
                            outcome: Some(Outcome::Error),
                            retry_count: job.retry_count,
                            error: Some(e),
                        };
                        let _ = super::orchestrator::JobProgressEvent(progress).emit(app);
                    }
                    drop(permit);
                    break;
                }

                {
                    let mut state = self.state.write().await;
                    if let Err(e) = state.mark_job_active(job.id, worker_id) {
                        log::error!("Failed to mark job active: {}", e);
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
                        log::debug!(
                            "Teardown phase '{}' waiting for main phases in slot {} to complete",
                            job.phase_name, slot_id
                        );
                        checked_jobs.push(job);
                        continue;
                    }

                    let running_jobs_info: Vec<_> = (0..state.worker_state.num_workers())
                        .filter_map(|worker_id| {
                            state.worker_state.get_worker_job(worker_id).map(|job_id| {
                                let slot = state.job_to_slot.get(&job_id).cloned();
                                (worker_id, job_id, slot)
                            })
                        })
                        .collect();

                    let has_running_slot_jobs = running_jobs_info.iter().any(|(_, _, slot)| {
                        slot.as_ref() == Some(slot_id)
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
                    log::debug!(
                        "Job {} plugs {:?} available",
                        job.phase_name, job.required_plugs
                    );
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
