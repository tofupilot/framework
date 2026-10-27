use crate::constants::scheduling;
use crate::event_sink::ExecutionEvent;
use crate::job::{Job, JobStatus, Outcome};
use crate::procedure::schema::StageScope;

use super::ExecutionStats;
use super::Orchestrator;

impl Orchestrator {
    pub async fn execute_all(&mut self) -> Result<ExecutionStats, String> {
        // Set start time
        self.start_time = Some(chrono::Utc::now());

        // Clear any leftover cancel reason from a prior run so it can't
        // bleed into this one's UI-cancellation messages.
        crate::ui::channels::clear_cancel_reason().await;

        let mut completion_rx = self
            .completion_rx
            .take()
            .ok_or("Completion receiver already taken")?;

        self.emit_stats().await;

        self.schedule_available_jobs().await?;

        // Shutdown events go out once per scope: once for the execution,
        // once per stopped slot.
        let mut shutdown_events_emitted = false;
        let mut slot_events_emitted: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        loop {
            let state = self.state.read().await;

            if state.force_kill_requested {
                log::debug!("Force kill requested, breaking out of execution loop");
                drop(state);
                crate::ui::channels::close_all_ui_channels_with_reason("Killed".to_string()).await;
                break;
            }

            // Compute *why* the loop is about to wind down so any UI phase
            // whose channel gets torn down can quote the cause instead of
            // saying the input was "cancelled or timed out".
            //
            // Stop scope = job scope: only a SHARED job (SetupAll,
            // TeardownAll) stops the execution here. A slot job that
            // stopped cancelled its own slot in `handle_stop` and is
            // handled by the per-slot branch below.
            let stopping_reason: Option<String> = state
                .job_results
                .iter()
                .filter(|(id, r)| {
                    r.should_stop_test()
                        && state.job_info.get(*id).is_some_and(|i| i.slot_id.is_none())
                })
                .map(|(id, r)| {
                    let phase_label = state
                        .job_info
                        .get(id)
                        .map(|i| i.phase_name.clone())
                        .unwrap_or_else(|| "<unknown phase>".to_string());
                    let detail = r
                        .error
                        .clone()
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default();
                    format!("Stopped by phase {phase_label}{detail}")
                })
                .next();
            let has_stop = stopping_reason.is_some();
            let busy_workers = state.worker_state.count_busy();
            let init_reason = state.init_error.clone();
            let shutdown_was_requested = state.shutdown_requested;

            if shutdown_was_requested || has_stop {
                // The operator's stop, or a failure's, worded as the
                // shutdown words it (`stop_reason`).
                let reason = stopping_reason
                    .or(init_reason)
                    .unwrap_or_else(|| state.stop_reason());
                // Close UI channels on every iteration to handle race conditions
                // where a phase registers its channel after the first close.
                // The `_with_reason` variant additionally records the cause so
                // workers waiting on UI input can surface it on their cancelled
                // phase's `error` field. Teardown prompts stay open: the
                // teardown is what runs because of the stop.
                crate::ui::channels::close_stopped_ui_channels_with_reason(reason.clone()).await;

                // Only emit shutdown events once
                if !shutdown_events_emitted {
                    shutdown_events_emitted = true;
                    self.emit_stopping_for(&state, None, &reason);
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
                // Per-slot stops: close that slot's prompts and announce
                // its Stopping/Skipped jobs once. Every other slot keeps
                // running, so the loop ends on completion alone.
                for (slot, stop) in &state.slot_stops {
                    if slot_events_emitted.insert(slot.clone()) {
                        self.emit_stopping_for(&state, Some(slot), &stop.reason);
                    }
                }
                // Same race as above: a phase of the stopped slot may
                // register its channel after the first close, so every
                // stopped slot is re-closed on every iteration.
                let stopped: Vec<(String, String)> = state
                    .slot_stops
                    .iter()
                    .map(|(slot, stop)| (slot.clone(), stop.reason.clone()))
                    .collect();
                if state.is_complete() {
                    break;
                }
                drop(state);
                for (slot, reason) in stopped {
                    crate::ui::channels::close_slot_ui_channels_with_reason(&slot, reason).await;
                }
            }

            // Clean up finished delayed retry task handles
            {
                let mut state = self.state.write().await;
                state.cleanup_finished_retry_handles();
            }

            tokio::select! {
                Some(event) = completion_rx.recv() => {
                    let should_continue = self.handle_job_completion(event).await;
                    if should_continue {
                        self.schedule_available_jobs().await?;
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(scheduling::IDLE_POLL_DELAY_MS)) => {
                    self.schedule_available_jobs().await?;
                }
            }
        }

        // Auto-teardown: Destroy all remaining plugs at procedure end.
        //
        // Not after a stop that left teardown phases queued: the loop
        // ended on `shutdown_requested`, not on the queue draining, and
        // `shutdown()` is about to run those phases. Destroying the
        // execution-scope plugs here handed them an empty plug map, so
        // `power_off(ps1)` errored on every operator stop and the bench
        // stayed powered. `shutdown()` releases every plug itself once
        // the teardown has run.
        //
        // Lock order: take `state.read()` first, then `resource_manager.
        // write()`. Other code paths (`get_next_ready_job`) acquire
        // `state.write()` then `resource_manager.read()`, so reversing
        // the order here would set up a lock-order inversion. By
        // releasing `state` before taking `rm.write()` we keep the
        // global order `state → resource_manager` consistent.
        let (slot_ids, keep_plugs_for_teardown): (Vec<String>, bool) = {
            let state = self.state.read().await;
            (
                state.slot_jobs.keys().cloned().collect(),
                state.shutdown_requested && state.has_queued_teardown(),
            )
        };
        if keep_plugs_for_teardown {
            log::info!("Stop with teardown phases queued: plugs stay up for them");
        }
        let resource_manager = self.resource_manager.write().await;

        for slot_id in slot_ids {
            if keep_plugs_for_teardown {
                break;
            }
            if resource_manager.has_slot_scope_plugs(&slot_id).await {
                match resource_manager
                    .destroy_slot_scope_plugs(slot_id.clone(), &self.event_sink)
                    .await
                {
                    Ok(_) => {
                        self.emit_plug_scope_event("pass").await;
                    }
                    Err(e) => {
                        log::warn!("Cannot stop plugs of slot {slot_id}: {e}");
                        self.emit_plug_scope_event("error").await;
                    }
                }
            }
        }

        // Close the execution-scope plugs
        if !keep_plugs_for_teardown && resource_manager.has_execution_scope_plugs().await {
            match resource_manager
                .destroy_execution_scope_plugs(&self.event_sink)
                .await
            {
                Ok(_) => {
                    self.emit_plug_scope_event("pass").await;
                }
                Err(e) => {
                    log::warn!("Cannot stop shared plugs: {e}");
                    self.emit_plug_scope_event("error").await;
                }
            }
        }

        drop(resource_manager);

        // Every plug scope that will ever start or release has done so,
        // unless `shutdown()` still runs teardown on kept plugs.
        if !keep_plugs_for_teardown {
            self.state.write().await.settle_plug_scope_ops();
        }

        // Set end time
        self.end_time = Some(chrono::Utc::now());

        // Every sequence call gets its outcome before the run announces
        // its own: a force kill completes the remaining jobs without going
        // through the completion handler, so nothing settled the calls
        // they belonged to. With every job done, every scope is complete.
        {
            let mut state = self.state.write().await;
            self.settle_sequences(&mut state).await;
        }

        // Emit final stats after all plug teardown completes
        self.emit_stats().await;

        let stats = self.get_stats().await;

        self.event_sink.emit(&ExecutionEvent::Complete {
            total_jobs: stats.total_jobs,
            completed_jobs: stats.completed_jobs,
            failed_jobs: stats.failed_jobs,
            running_jobs: stats.running_jobs,
            queued_jobs: stats.queued_jobs,
            workers_busy: stats.workers_busy,
            workers_total: stats.workers_total,
            run_outcome: stats.run_outcome,
            run_dir: stats.run_dir.clone(),
            run_id: stats.run_id.clone(),
            slot_outcomes: stats.slot_outcomes.clone(),
            slot_run_ids: stats.slot_run_ids.clone(),
            start_time: stats.start_time,
            end_time: stats.end_time,
        });

        match stats.run_outcome {
            Some(Outcome::Pass) => log::info!("Run PASSED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Fail) => log::info!("Run FAILED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Error) => log::info!("Run ERROR: {} jobs processed", stats.total_jobs),
            Some(Outcome::Skip) => log::info!("Run SKIP: {} jobs processed", stats.total_jobs),
            Some(Outcome::Timeout) => {
                log::info!("Run TIMEOUT: {} jobs processed", stats.total_jobs)
            }
            Some(Outcome::Stop) => log::info!("Run STOPPED: {} jobs processed", stats.total_jobs),
            Some(Outcome::Retry) => log::info!("Run RETRY: {} jobs processed", stats.total_jobs),
            None => log::info!("Execution complete: {} jobs processed", stats.total_jobs),
        }

        Ok(stats)
    }

    /// Stopping for the running jobs and Skipped for the queued main
    /// phases of one scope: `Some(slot)` for that slot only, `None` for
    /// the whole execution. The teardown work the cancellation keeps (the
    /// procedure's, and that of the entered calls) is left out: it runs
    /// after the stop.
    fn emit_stopping_for(
        &self,
        state: &crate::state::OrchestratorState,
        slot: Option<&str>,
        reason: &str,
    ) {
        let in_scope = |job_slot: &Option<String>| match slot {
            Some(s) => job_slot.as_deref() == Some(s),
            None => true,
        };

        for worker_id in 0..state.worker_state.num_workers() {
            if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                if let Some(info) = state.job_info.get(&job_id) {
                    // A running teardown is not stopping: it is the work the
                    // stop waits for, and it reports its own outcome.
                    if !in_scope(&info.slot_id) || info.is_teardown() {
                        continue;
                    }
                    self.event_sink.emit(&ExecutionEvent::JobProgress {
                        job_id: job_id.to_string(),
                        slot_id: info.slot_id.clone(),
                        phase_key: info.phase_key.clone(),
                        phase_name: info.phase_name.clone(),
                        stage_scope: info.stage_scope,
                        status: JobStatus::Stopping,
                        worker_id: Some(worker_id),
                        started_at: None,
                        timeout_ms: None,
                        outcome: None,
                        retry_count: info.retry_count,
                        error: None,
                    });
                }
            }
        }

        for job in state.job_queue.iter() {
            // The same rule the cancellation applies, so a sequence's
            // teardown phase is not announced as skipped and then run.
            if !in_scope(&job.slot_id)
                || crate::state::keeps_on_cancel(&state.entered_sequences, job, 0)
            {
                continue;
            }
            self.event_sink.emit(&ExecutionEvent::JobProgress {
                job_id: job.id.to_string(),
                slot_id: job.slot_id.clone(),
                phase_key: job.phase_key.clone(),
                phase_name: job.phase_name.clone(),
                stage_scope: job.stage_scope,
                status: JobStatus::Skipped,
                worker_id: None,
                started_at: None,
                timeout_ms: job.timeout_ms,
                outcome: Some(Outcome::Skip),
                retry_count: job.retry_count,
                // Why it never runs: the stop's cause, said once here.
                error: Some(crate::state::not_run(reason)),
            });
        }
    }

    /// Record an initialization failure (plugs, worker pool) as the run's
    /// `init_error`, request shutdown, and mark the job it surfaced on.
    /// A start that failed because a force kill landed during it is the
    /// kill's doing, not an init failure: recording one would turn the
    /// run's ABORTED into ERROR. The job was already taken off the
    /// queue, so the kill's own sweep never sees it; it is resolved
    /// here. Returns whether the kill is in effect.
    async fn killed_during_start(&self, job: &Job) -> bool {
        let reason = {
            let state = self.state.read().await;
            if !state.force_kill_requested {
                return false;
            }
            state.kill_reason()
        };
        self.resolve_unstarted(job, &reason).await;
        true
    }

    /// The plug start gave way to an operator stop: the job never ran
    /// and the stop's own sweep (it was off the queue) never saw it.
    /// Not an init failure, the run reads as stopped. Returns whether
    /// that is the case.
    async fn stopped_during_start(&self, job: &Job) -> bool {
        let reason = {
            let state = self.state.read().await;
            if job.is_teardown_work()
                || state.shutdown_cause != Some(crate::state::ShutdownCause::Operator)
            {
                return false;
            }
            state.stop_reason()
        };
        self.resolve_unstarted(job, &reason).await;
        true
    }

    /// Close, as not run, a job taken off the queue whose start failed.
    /// Completed in the state too: a teardown phase depends on every
    /// main job, and one left open kept the teardown from ever starting
    /// (it waited out the teardown cap instead).
    async fn resolve_unstarted(&self, job: &Job, reason: &str) {
        {
            let mut state = self.state.write().await;
            state.starting_main.remove(&job.id);
            state
                .job_info
                .insert(job.id, crate::state::JobInfo::from_job(job));
            state.complete_original_job(
                job.id,
                crate::state::OrchestratorState::skip_result(job.retry_count),
            );
        }
        self.event_sink.emit(&ExecutionEvent::JobProgress {
            job_id: job.id.to_string(),
            slot_id: job.slot_id.clone(),
            phase_key: job.phase_key.clone(),
            phase_name: job.phase_name.clone(),
            stage_scope: job.stage_scope,
            status: JobStatus::Skipped,
            worker_id: None,
            started_at: None,
            timeout_ms: job.timeout_ms,
            outcome: Some(Outcome::Skip),
            retry_count: job.retry_count,
            error: Some(crate::state::not_run(reason)),
        });
    }

    async fn fail_run_init(&self, job: &Job, e: String) {
        {
            let mut state = self.state.write().await;
            state.init_error = Some(e.clone());
            // `init_error` forces ERROR before the cause is read; recorded
            // anyway so the state never lies about why it stopped.
            state.request_shutdown(crate::state::ShutdownCause::InitFailure);
        }
        // Its plugs or workers would not start: it never ran, like every
        // phase after it. The run is ERROR through `init_error`; the
        // phase is not, since it is only the one whose turn came first.
        self.resolve_unstarted(job, &e).await;
    }

    async fn schedule_available_jobs(&mut self) -> Result<(), String> {
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
                // The plug start holds the resource-manager write lock a
                // force kill's teardown waits for, and a plug can take
                // tens of seconds to construct. Race the start against
                // the kill so the kill lands now: dropping the start
                // drops its `ChildProcess`, which kills the half-started
                // interpreter, and nothing is registered for it yet.
                // A forced phase does not run: it creates no
                // plug, so a missing instrument does not fail the run for it.
                let forced = self.forced_result(&job.phase_key).is_some();
                let plug_result = if forced {
                    Ok(())
                } else {
                    let plugs = self.ensure_plugs_created_for_job(&job);
                    tokio::pin!(plugs);
                    let state = self.state.clone();
                    // An operator stop too, not only a kill: the run would
                    // otherwise wait out a slow `__init__`, then start the
                    // setup it was asked not to run.
                    tokio::select! {
                        result = &mut plugs => result,
                        _ = async {
                            loop {
                                {
                                    let s = state.read().await;
                                    // A teardown phase is what the stop
                                    // runs: its plug start is not cut.
                                    if s.force_kill_requested
                                        || (!job.is_teardown_work()
                                            && s.shutdown_cause
                                                == Some(crate::state::ShutdownCause::Operator))
                                    {
                                        break;
                                    }
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                        } => Err("Killed by operator".to_string()),
                    }
                };
                if let Err(e) = plug_result {
                    // A stop dropped the plug start mid-way: its plugs
                    // still read `initializing` otherwise.
                    // State before the resource manager: the lock order.
                    let slots = self.state.read().await.slots.clone();
                    let opened: Vec<String> = {
                        let rm = self.resource_manager.read().await;
                        rm.abandon_opening(&self.event_sink).await;
                        let mut opened = Vec::new();
                        for slot in slots {
                            if rm.has_slot_scope_plugs(&slot).await {
                                opened.push(slot);
                            }
                        }
                        opened
                    };
                    // A slot whose plugs had opened runs its teardown.
                    self.state.write().await.started_slots.extend(opened);
                    if self.killed_during_start(&job).await {
                        drop(permit);
                        break;
                    }
                    if self.stopped_during_start(&job).await {
                        drop(permit);
                        break;
                    }
                    log::debug!("Plug initialization failed: {}", e);
                    self.fail_run_init(&job, e).await;
                    drop(permit);
                    break;
                }

                // The pool spawns in the background from `initialize()`
                // so it overlaps the plug startup above. A spawn that
                // failed is an environment failure like a plug that
                // failed: stop the run as `init_error`, not as a phase
                // error that retries and teardown would keep hitting.
                if let Err(e) = worker.await_background_start().await {
                    if self.killed_during_start(&job).await {
                        drop(permit);
                        break;
                    }
                    log::debug!("Worker initialization failed: {}", e);
                    self.fail_run_init(&job, e).await;
                    drop(permit);
                    break;
                }
                // A stop that landed while the plugs or the worker started:
                // the phase was not running yet, it does not start now.
                if self.stopped_during_start(&job).await {
                    drop(permit);
                    break;
                }

                {
                    let mut state = self.state.write().await;
                    if let Err(e) = state.mark_job_active(job.id, worker_id) {
                        state.starting_main.remove(&job.id);
                        log::debug!("Failed to mark job active: {}", e);
                        drop(permit);
                        break;
                    }
                }
                self.spawn_job_execution(job, worker_id, worker, permit)
                    .await?;
                self.emit_stats().await;
            } else {
                drop(permit);
                break;
            }
        }

        {
            let mut state = self.state.write().await;
            if state.check_and_queue_next_slot() {
                drop(state);
                Box::pin(self.schedule_available_jobs()).await?;
            }
        }

        Ok(())
    }

    async fn get_next_ready_job(&self) -> Option<Job> {
        let mut state = self.state.write().await;

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
                            job.phase_name,
                            slot_id
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

                    let has_running_slot_jobs = running_jobs_info
                        .iter()
                        .any(|(_, _, slot)| slot.as_ref() == Some(slot_id));

                    if has_running_slot_jobs {
                        checked_jobs.push(job);
                        continue;
                    }
                }
            }

            state.note_dispatched(&job);
            if !job.is_teardown_work() {
                state.starting_main.insert(job.id);
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
