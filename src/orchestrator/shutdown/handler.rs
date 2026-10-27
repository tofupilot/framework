//! Worker and slot control operations
//!
//! This module handles granular control over individual workers and slots:
//! - Force killing workers (immediate termination)
//! - Graceful worker stopping
//! - Slot-level stopping (all workers for a slot)
//! - System shutdown coordination

use futures::future::join_all;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::event_sink::{EventSink, ExecutionEvent};
use crate::job::{Job, JobResult, JobStatus, Outcome};
use crate::orchestrator::JobCompletionEvent;
use crate::procedure::schema::StageScope;
use crate::state::{JobInfo, OrchestratorState};
use crate::worker::Worker;

/// A job a shutdown settles, with what its events must carry: its stage
/// (a teardown is not a main phase) and its attempt number.
#[derive(Debug, Clone)]
struct JobRef {
    job_id: uuid::Uuid,
    phase_key: String,
    phase_name: String,
    slot_id: Option<String>,
    stage_scope: StageScope,
    retry_count: usize,
}

impl JobRef {
    fn from_info(job_id: uuid::Uuid, info: &JobInfo) -> Self {
        Self {
            job_id,
            phase_key: info.phase_key.clone(),
            phase_name: info.phase_name.clone(),
            slot_id: info.slot_id.clone(),
            stage_scope: info.stage_scope,
            retry_count: info.retry_count,
        }
    }

    fn from_job(job: &Job) -> Self {
        Self::from_info(job.id, &JobInfo::from_job(job))
    }
}

/// A job that was running on `worker_id` when the shutdown drained it.
#[derive(Debug, Clone)]
struct RunningJob {
    worker_id: usize,
    job: JobRef,
}

use super::super::Orchestrator;

/// Cap on the teardown phases `shutdown()` runs after a stop. Exported so
/// the CLI's signal ladder can size its own escalation from it.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// The workers the teardown phases `shutdown()` runs get: one per
/// teardown phase, so every slot powers down at once, up to the run's own
/// pool (`execution.workers`).
fn teardown_width(teardown_jobs: usize, pool: usize) -> usize {
    teardown_jobs.clamp(1, pool.max(1))
}

/// How much time `shutdown()` may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownMode {
    /// Graceful worker stop, fresh teardown workers, Cleanup RPC on
    /// every plug.
    Normal,
    /// An OS deadline of seconds is behind the stop (Windows console
    /// close). Workers are killed outright, the teardown runs on a
    /// pool worker that is already idle (one spawn only if none is),
    /// and the plugs are killed without their Cleanup RPC: the exit
    /// hook (Unix) or the Job Object (Windows) reaps them anyway.
    Hurried,
}

impl Orchestrator {
    fn collect_and_complete_jobs(
        state: &mut OrchestratorState,
        reason: String,
        partition_teardown: bool,
    ) -> (Vec<RunningJob>, Vec<JobRef>, Vec<Job>) {
        let mut running = Vec::new();
        let mut queued = Vec::new();

        for worker_id in 0..state.worker_state.num_workers() {
            if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                let mut result = JobResult::new_interrupted(reason.clone());
                if let Some(info) = state.job_info.get(&job_id) {
                    result.retry_count = info.retry_count;
                    running.push(RunningJob {
                        worker_id,
                        job: JobRef::from_info(job_id, info),
                    });
                }
                // An operator interruption, not a failure: see `new_interrupted`.
                // Counted done like every job this drain settles, so the
                // run's progress closes after a stop.
                state.complete_original_job(job_id, result);
            }
        }

        let (mut teardown_jobs, mut skipped): (Vec<Job>, Vec<Job>) = if partition_teardown {
            // Same rule as a cancellation: the procedure's teardown, and
            // the teardown of the sequence calls that were entered.
            let entered = state.entered_sequences.clone();
            state
                .job_queue
                .drain(..)
                .partition(|job| crate::state::keeps_on_cancel(&entered, job, 0))
        } else {
            (Vec::new(), state.job_queue.drain(..).collect())
        };

        // Work deferred under slot-first (the slots not started yet, the
        // parked shared teardown) was never enqueued, so it is not in the
        // submitted total yet.
        let deferred: Vec<Job> = state
            .pending_slot_jobs
            .drain(..)
            .flat_map(|(_, jobs)| jobs)
            .collect();
        state.total_jobs_submitted += deferred.len() + state.teardown_procedure_jobs.len();
        skipped.extend(deferred);
        if partition_teardown {
            teardown_jobs.append(&mut state.teardown_procedure_jobs);
            // A slot the stop caught before it started has no plugs and
            // nothing to undo; slot-first already skips its whole slot.
            let (nothing_to_undo, kept): (Vec<Job>, Vec<Job>) = teardown_jobs
                .into_iter()
                .partition(|job| state.teardown_has_nothing_to_undo(job));
            teardown_jobs = kept;
            skipped.extend(nothing_to_undo);
        } else {
            skipped.append(&mut state.teardown_procedure_jobs);
        }

        for job in &skipped {
            queued.push(JobRef::from_job(job));
            // Populate job_info so complete_job can resolve dependency_id
            state.job_info.insert(job.id, JobInfo::from_job(job));
            let mut result = JobResult::new_skip();
            result.retry_count = job.retry_count;
            state.complete_original_job(job.id, result);
        }

        (running, queued, teardown_jobs)
    }

    /// A delayed retry the shutdown aborted never ran: Skipped, STOP.
    fn emit_aborted_retries(
        retries: &[crate::state::PendingDelayedRetry],
        reason: &str,
        event_sink: &Arc<dyn EventSink>,
    ) {
        for pending in retries {
            event_sink.emit(&ExecutionEvent::JobProgress {
                job_id: pending.job_id.to_string(),
                slot_id: pending.slot_id.clone(),
                phase_key: pending.phase_key.clone(),
                phase_name: pending.phase_name.clone(),
                stage_scope: pending.stage_scope,
                status: JobStatus::Skipped,
                worker_id: None,
                started_at: None,
                timeout_ms: None,
                outcome: Some(Outcome::Stop),
                retry_count: pending.retry_count,
                error: Some(reason.to_string()),
            });
        }
    }

    fn emit_job_event(
        job: &JobRef,
        status: JobStatus,
        outcome: Option<Outcome>,
        error: Option<String>,
        worker_id: Option<usize>,
        event_sink: &Arc<dyn EventSink>,
    ) {
        event_sink.emit(&ExecutionEvent::JobProgress {
            job_id: job.job_id.to_string(),
            slot_id: job.slot_id.clone(),
            phase_key: job.phase_key.clone(),
            phase_name: job.phase_name.clone(),
            stage_scope: job.stage_scope,
            status,
            worker_id,
            started_at: None,
            timeout_ms: None,
            outcome,
            retry_count: job.retry_count,
            error,
        });
    }

    fn emit_job_events(
        jobs: &[JobRef],
        status: JobStatus,
        outcome: Option<Outcome>,
        error: Option<String>,
        event_sink: &Arc<dyn EventSink>,
    ) {
        for job in jobs {
            Self::emit_job_event(job, status, outcome, error.clone(), None, event_sink);
        }
    }

    fn jobs_of(running: &[RunningJob]) -> Vec<JobRef> {
        running.iter().map(|r| r.job.clone()).collect()
    }

    async fn shutdown_workers_gracefully(
        workers: &mut [Worker],
        running: &[RunningJob],
        reason: &str,
        event_sink: &Arc<dyn EventSink>,
    ) {
        use std::collections::HashMap;

        let job_map: HashMap<usize, &JobRef> =
            running.iter().map(|r| (r.worker_id, &r.job)).collect();

        // Step 1: Emit "stopping" events for all workers with jobs immediately
        for (worker_id, _) in workers.iter().enumerate() {
            if let Some(job) = job_map.get(&worker_id) {
                log::debug!(
                    "Emitting status=stopping for phase={}, slot={:?}",
                    job.phase_name,
                    job.slot_id
                );
                Self::emit_job_event(
                    job,
                    JobStatus::Stopping,
                    None,
                    None,
                    Some(worker_id),
                    event_sink,
                );
            }
        }

        // Step 2: Stop all workers in parallel
        let shutdown_futures: Vec<_> = workers
            .iter_mut()
            .enumerate()
            .map(|(worker_id, worker)| {
                let has_job = job_map.contains_key(&worker_id);
                async move {
                    if has_job {
                        let _ = worker.interrupt_current_job().await;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }

                    let res = timeout(
                        Duration::from_millis(1000),
                        worker.shutdown_with_timeout(1000),
                    )
                    .await;

                    match res {
                        Ok(Ok(())) => {}
                        _ => {
                            let _ = worker.force_shutdown().await;
                        }
                    }
                }
            })
            .collect();

        join_all(shutdown_futures).await;

        // Step 3: Emit "stop" outcome for all workers with jobs
        for (worker_id, _) in workers.iter().enumerate() {
            if let Some(job) = job_map.get(&worker_id) {
                log::debug!(
                    "Emitting outcome=stop for phase={}, slot={:?}",
                    job.phase_name,
                    job.slot_id
                );
                Self::emit_job_event(
                    job,
                    JobStatus::Completed,
                    Some(Outcome::Stop),
                    Some(reason.to_string()),
                    None,
                    event_sink,
                );
            }
        }
    }

    async fn force_kill_workers_parallel(workers: Vec<Worker>) {
        let kill_futures: Vec<_> = workers
            .iter()
            .enumerate()
            .map(|(idx, worker)| {
                let mut worker_clone = worker.clone();
                async move {
                    log::debug!("Force killing worker {}", idx);
                    let result = worker_clone.force_shutdown().await;
                    match &result {
                        Ok(_) => {}
                        Err(e) => {
                            log::debug!("Worker {} kill failed: {}", idx, e)
                        }
                    }
                    result
                }
            })
            .collect();

        futures::future::join_all(kill_futures).await;
    }

    /// Run the teardown phases on `teardown_workers`, spawning fresh
    /// ones when none was handed over. `Hurried` skips every grace
    /// period on the way out.
    async fn execute_teardown_jobs(
        &mut self,
        teardown_jobs: Vec<Job>,
        mut teardown_workers: Vec<Worker>,
        mode: ShutdownMode,
        width: usize,
    ) -> Result<(), String> {
        {
            let mut state = self.state.write().await;
            if state.force_kill_requested {
                self.cancel_unrun_jobs(&mut state, teardown_jobs);
                drop(state);
                Self::force_kill_workers_parallel(teardown_workers).await;
                return Err("Killed: teardown skipped".to_string());
            }
        }

        // Hurried: what the stop kept, or one; a spawn is the slowest
        // step of a shutdown that has seconds. Spawned together.
        let want = match mode {
            ShutdownMode::Normal => width,
            ShutdownMode::Hurried => 1,
        };
        if teardown_workers.len() < want {
            let first = teardown_workers.len();
            let mut spawned: Vec<Worker> = (first..want)
                .map(|i| {
                    Worker::new_with_python(i, self.procedure_dir.clone(), self.python_path.clone())
                        .with_procedure_env(self.procedure_env.clone())
                })
                .collect();
            let started =
                futures::future::join_all(spawned.iter_mut().map(|w| w.start(&self.event_sink)))
                    .await;
            for (worker, result) in spawned.into_iter().zip(started) {
                // One that will not start is not fatal while another did.
                match result {
                    Ok(()) => teardown_workers.push(worker),
                    Err(e) if teardown_workers.is_empty() && want == 1 => return Err(e),
                    Err(e) => log::warn!("Teardown worker did not start: {e}"),
                }
            }
            if teardown_workers.is_empty() {
                return Err("No teardown worker started".to_string());
            }
        }

        // Re-populate state with teardown jobs (phases stay pending until
        // actually started). A force kill that landed while the workers
        // were spawning has already drained the queue and killed the
        // plugs: nothing left to run the teardown against.
        {
            let mut state = self.state.write().await;
            if state.force_kill_requested {
                self.cancel_unrun_jobs(&mut state, teardown_jobs);
                drop(state);
                Self::force_kill_workers_parallel(teardown_workers).await;
                return Err("Killed: teardown skipped".to_string());
            }
            for job in teardown_jobs {
                state.requeue_job(job);
            }
            state.shutdown_requested = false; // Temporarily allow execution
        }

        // Store teardown workers
        {
            let mut workers = self.workers.write().await;
            *workers = teardown_workers;
        }

        // Execute teardown jobs with timeout. The completion channel is
        // swapped here, not inside the loop: a timeout drops the loop
        // future before any restore it would do, and the orchestrator
        // would be left sending completions into a closed channel.
        let (teardown_tx, teardown_rx) = tokio::sync::mpsc::channel(64);
        let original_tx = std::mem::replace(&mut self.completion_tx, teardown_tx);
        let teardown_timeout = self.teardown_timeout;
        let teardown_result =
            tokio::time::timeout(teardown_timeout, self.run_teardown_loop(teardown_rx)).await;
        self.completion_tx = original_tx;

        // The flag was lowered above so teardown phases could run; raise it
        // again so the state does not claim the run is still live. Bare
        // flag on purpose: `shutdown()` is also the normal end-of-run
        // teardown, so it must not invent an `Operator` cause. Whoever
        // actually asked for the stop recorded the cause before this.
        self.state.write().await.shutdown_requested = true;

        // Shutdown teardown workers
        let workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };
        match mode {
            ShutdownMode::Normal => {
                for mut worker in workers {
                    let _ = worker.shutdown_with_timeout(1000).await;
                }
            }
            ShutdownMode::Hurried => Self::force_kill_workers_parallel(workers).await,
        }

        match teardown_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("Teardown failed: {}", e)),
            Err(_) => {
                // Timeout: record what was still running, then what never
                // started. Both read ERROR with the same message; without
                // the first loop a teardown cut by the cap simply vanished
                // from the results.
                let mut state = self.state.write().await;
                let message = "Teardown timed out".to_string();
                let running: Vec<(uuid::Uuid, crate::state::JobInfo)> =
                    (0..state.worker_state.num_workers())
                        .filter_map(|worker_id| state.worker_state.get_worker_job(worker_id))
                        .filter_map(|job_id| {
                            state
                                .job_info
                                .get(&job_id)
                                .map(|info| (job_id, info.clone()))
                        })
                        .collect();
                for (job_id, info) in running {
                    let mut result = JobResult::new_error(message.clone());
                    result.retry_count = info.retry_count;
                    state.complete_original_job(job_id, result);
                    self.event_sink.emit(&ExecutionEvent::JobProgress {
                        job_id: job_id.to_string(),
                        slot_id: info.slot_id.clone(),
                        phase_key: info.phase_key.clone(),
                        phase_name: info.phase_name.clone(),
                        stage_scope: info.stage_scope,
                        status: JobStatus::Completed,
                        worker_id: None,
                        started_at: None,
                        timeout_ms: None,
                        outcome: Some(Outcome::Error),
                        retry_count: info.retry_count,
                        error: Some(message.clone()),
                    });
                }
                for pending in state.fail_pending_teardown_retries(&message) {
                    self.event_sink.emit(&ExecutionEvent::JobProgress {
                        job_id: pending.job_id.to_string(),
                        slot_id: pending.slot_id.clone(),
                        phase_key: pending.phase_key.clone(),
                        phase_name: pending.phase_name.clone(),
                        stage_scope: pending.stage_scope,
                        status: JobStatus::Completed,
                        worker_id: None,
                        started_at: None,
                        timeout_ms: None,
                        outcome: Some(Outcome::Error),
                        retry_count: pending.retry_count,
                        error: Some(message.clone()),
                    });
                }
                while let Some(job) = state.job_queue.pop_front() {
                    state
                        .job_info
                        .insert(job.id, crate::state::JobInfo::from_job(&job));
                    let mut result = JobResult::new_error(message.clone());
                    result.retry_count = job.retry_count;
                    state.complete_original_job(job.id, result);

                    // Emit timeout event for this job
                    self.event_sink.emit(&ExecutionEvent::JobProgress {
                        job_id: job.id.to_string(),
                        slot_id: job.slot_id.clone(),
                        phase_key: job.phase_key.clone(),
                        phase_name: job.phase_name.clone(),
                        stage_scope: job.stage_scope,
                        status: JobStatus::Completed,
                        worker_id: None,
                        started_at: None,
                        timeout_ms: job.timeout_ms,
                        outcome: Some(Outcome::Error),
                        retry_count: job.retry_count,
                        error: Some(message.clone()),
                    });
                }
                Err(format!(
                    "Teardown timed out after {}s",
                    teardown_timeout.as_secs()
                ))
            }
        }
    }

    /// Complete, as cancelled, teardown jobs a kill kept from running.
    /// They were drained from the queue before the kill landed, so no
    /// other path closes them or the sequence scopes they belong to.
    fn cancel_unrun_jobs(&self, state: &mut OrchestratorState, jobs: Vec<Job>) {
        for job in jobs {
            state
                .job_info
                .insert(job.id, crate::state::JobInfo::from_job(&job));
            state.complete_original_job(job.id, OrchestratorState::skip_result(job.retry_count));
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
                error: Some("Force killed by user".to_string()),
            });
        }
    }

    /// Drive the teardown jobs to completion, reading their completions
    /// from `teardown_rx` (the caller swapped `completion_tx` to its
    /// sender, and restores it whether this returns or is dropped).
    async fn run_teardown_loop(
        &mut self,
        mut teardown_rx: tokio::sync::mpsc::Receiver<JobCompletionEvent>,
    ) -> Result<(), String> {
        loop {
            let is_complete = {
                let state = self.state.read().await;
                // A force kill during the teardown (`force_kill_immediate`
                // from the CLI's second signal) killed the workers and
                // drained the queue under us; do not schedule the rest.
                state.force_kill_requested
                    || (state.job_queue.is_empty()
                        && state.worker_state.count_busy() == 0
                        && !state.has_pending_teardown_retry())
            };
            self.state.write().await.cleanup_finished_retry_handles();

            if is_complete {
                break;
            }

            // Schedule available teardown jobs (reuse existing scheduling logic)
            self.schedule_teardown_jobs().await?;

            // Process completion events
            tokio::select! {
                Some(event) = teardown_rx.recv() => {
                    self.handle_job_completion(event).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }

        Ok(())
    }

    async fn schedule_teardown_jobs(&self) -> Result<(), String> {
        // The tracker still counts the pool's workers; only the first
        // `teardown_pool` of those ids have a worker behind them now.
        // Scheduling on a higher id used to fail the whole teardown
        // ("Worker not found") as soon as a procedure had more teardown
        // phases than teardown workers.
        let teardown_pool = self.workers.read().await.len();
        let jobs_to_spawn = {
            let mut state = self.state.write().await;
            let num_workers = state.worker_state.num_workers().min(teardown_pool);
            let mut jobs = Vec::new();

            for worker_id in 0..num_workers {
                if !state.worker_state.is_worker_idle(worker_id) {
                    continue;
                }

                // Get next ready teardown job
                let job = match state.pop_ready_job(|_| true) {
                    Some(j) => j,
                    None => continue,
                };

                // Mark as active
                state.mark_job_active(job.id, worker_id)?;
                jobs.push((job, worker_id));
            }

            jobs
        };

        // Spawn jobs outside the lock
        for (job, worker_id) in jobs_to_spawn {
            // Get worker
            let worker = {
                let workers = self.workers.read().await;
                workers.get(worker_id).ok_or("Worker not found")?.clone()
            };

            // Spawn execution so teardown phases emit "started" events
            let permit = self.job_semaphore.clone().acquire_owned().await.unwrap();
            self.spawn_job_execution(job, worker_id, worker, permit)
                .await?;
        }

        Ok(())
    }
    /// Enhanced shutdown with graceful-to-force escalation
    pub async fn shutdown(&mut self) -> Result<(), String> {
        self.shutdown_with(ShutdownMode::Normal).await
    }

    /// `shutdown` with its time budget spelled out, see [`ShutdownMode`].
    pub async fn shutdown_with(&mut self, mode: ShutdownMode) -> Result<(), String> {
        // Check if force kill was requested
        {
            let state = self.state.read().await;
            if state.force_kill_requested {
                drop(state);
                return self.force_kill().await;
            }

            // Check if already shut down
            if state.shutdown_requested && self.workers.read().await.is_empty() {
                return Ok(());
            }
        }

        let (running_jobs_info, regular_jobs_info, teardown_jobs, pending_retry_handles, reason) = {
            let mut state = self.state.write().await;
            // Bare flag, no cause: the CLI calls `shutdown()` after EVERY
            // run to reap workers and plugs, a clean pass included. An
            // operator stop recorded its `Operator` cause before reaching
            // here (`request_shutdown`, first cause wins).
            state.shutdown_requested = true;
            let reason = state.stop_reason();
            // Before anything is drained: once running jobs read as
            // interrupted and queued ones as skipped, a slot that was cut
            // is indistinguishable from one that finished (`SlotStop`).
            state.mark_outstanding_slots_stopped(&reason);
            // The pending retries of the sequence scopes are cut with the
            // run: their calls settle as interrupted, not as still running.
            state.mark_pending_retry_scopes_stopped(&reason);
            // A teardown retry waiting out its delay is teardown work like
            // the queued teardown: it stays, enqueues itself, and the
            // teardown loop below waits for it.
            let handles = state.abort_retries_for_shutdown(|p| {
                !matches!(
                    p.stage_scope,
                    StageScope::TeardownEach | StageScope::TeardownAll
                )
            });
            let result = Self::collect_and_complete_jobs(&mut state, reason.clone(), true);
            // The queued jobs just completed as cancelled finish the
            // scopes they belonged to; nothing else would settle those
            // calls, and an upload must name every call its phases do.
            self.settle_sequences(&mut state).await;
            (result.0, result.1, result.2, handles, reason)
        };

        Self::emit_aborted_retries(
            &pending_retry_handles,
            "Retry cancelled: run stopping",
            &self.event_sink,
        );

        let mut workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };
        let width = teardown_width(teardown_jobs.len(), workers.len());

        let teardown_workers = match mode {
            ShutdownMode::Normal => {
                Self::shutdown_workers_gracefully(
                    &mut workers,
                    &running_jobs_info,
                    &reason,
                    &self.event_sink,
                )
                .await;
                Vec::new()
            }
            ShutdownMode::Hurried => {
                Self::stop_workers_hurried(
                    workers,
                    &running_jobs_info,
                    &reason,
                    &self.event_sink,
                    width,
                )
                .await
            }
        };

        Self::emit_job_events(
            &regular_jobs_info,
            JobStatus::Skipped,
            Some(Outcome::Skip),
            Some(crate::state::not_run(&reason)),
            &self.event_sink,
        );

        // Execute teardown jobs if any
        let teardown_retry_pending = self.state.read().await.has_pending_teardown_retry();
        if !teardown_jobs.is_empty() || teardown_retry_pending {
            log::info!(
                "Executing {} teardown phases before shutdown",
                teardown_jobs.len()
            );

            if let Err(e) = self
                .execute_teardown_jobs(teardown_jobs, teardown_workers, mode, width)
                .await
            {
                log::error!("Teardown failed: {e}");
            }
        } else {
            Self::force_kill_workers_parallel(teardown_workers).await;
        }

        let completed_before = self.state.read().await.original_jobs_completed;
        // The teardown jobs (or their kill) completed more scopes. A job
        // of a call that is still not completed here (a teardown phase
        // running at the timeout, one a kill caught on its way to a
        // worker) is closed as cancelled, so every entered call settles
        // before the upload is built.
        {
            let mut state = self.state.write().await;
            // A kill during the shutdown teardown lands here too.
            let reason = if state.force_kill_requested {
                state.kill_reason()
            } else {
                state.stop_reason()
            };
            self.cancel_unfinished_sequence_jobs(&mut state, &reason);
            self.settle_sequences(&mut state).await;
        }

        self.release_scope_plugs(mode).await;
        let progressed = {
            let mut state = self.state.write().await;
            state.settle_plug_scope_ops();
            state.original_jobs_completed != completed_before
        };
        // The progress the UI last saw is short of what was just counted.
        if progressed {
            self.emit_stats().await;
        }

        // Catch-all for what the scope release above does not own
        // (manual plugs, a service whose instance entry is gone).
        let plug_service_manager = {
            let resource_manager = self.resource_manager.read().await;
            Arc::clone(resource_manager.get_plug_service_manager())
        };
        let stopped = match mode {
            ShutdownMode::Normal => plug_service_manager.stop_all_services().await,
            ShutdownMode::Hurried => plug_service_manager.force_kill_all_services().await,
        };
        if let Err(e) = stopped {
            log::warn!("Cannot stop plugs: {e}");
        }

        Ok(())
    }

    /// Release the scope plugs a stop kept up for its teardown phases
    /// (`execute_all` leaves them when teardown work is still queued).
    /// Counts each release toward the run's progress exactly like the
    /// end-of-run release does, so an operator stop still reaches 100%.
    /// A run whose plugs were already released at the end of
    /// `execute_all` finds nothing here.
    async fn release_scope_plugs(&self, mode: ShutdownMode) {
        let slot_ids: Vec<String> = {
            let state = self.state.read().await;
            state.slot_jobs.keys().cloned().collect()
        };
        let mut statuses: Vec<&'static str> = Vec::new();
        {
            let resource_manager = self.resource_manager.write().await;
            match mode {
                ShutdownMode::Normal => {
                    for slot_id in slot_ids {
                        if !resource_manager.has_slot_scope_plugs(&slot_id).await {
                            continue;
                        }
                        match resource_manager
                            .destroy_slot_scope_plugs(slot_id.clone(), &self.event_sink)
                            .await
                        {
                            Ok(_) => statuses.push("pass"),
                            Err(e) => {
                                log::warn!("Cannot stop plugs of slot {slot_id}: {e}");
                                statuses.push("error");
                            }
                        }
                    }
                    if resource_manager.has_execution_scope_plugs().await {
                        match resource_manager
                            .destroy_execution_scope_plugs(&self.event_sink)
                            .await
                        {
                            Ok(_) => statuses.push("pass"),
                            Err(e) => {
                                log::warn!("Cannot stop shared plugs: {e}");
                                statuses.push("error");
                            }
                        }
                    }
                }
                ShutdownMode::Hurried => {
                    for slot_id in &slot_ids {
                        if resource_manager.has_slot_scope_plugs(slot_id).await {
                            statuses.push("pass");
                        }
                    }
                    if resource_manager.has_execution_scope_plugs().await {
                        statuses.push("pass");
                    }
                    if let Err(e) = resource_manager
                        .force_destroy_all_plugs_now(&self.event_sink)
                        .await
                    {
                        log::warn!("Cannot kill plugs: {e}");
                    }
                }
            }
        }
        // Emitted with the resource manager released: the counter takes
        // `state`, and the lock order is state before resource manager.
        for status in statuses {
            self.emit_plug_scope_event(status).await;
        }
    }

    /// The hurried counterpart of `shutdown_workers_gracefully`: same
    /// events, no grace. Idle workers whose interpreter is still up are
    /// handed back for the teardown phases (a spawn is the slowest step
    /// of a shutdown that has seconds); the rest are killed in parallel.
    async fn stop_workers_hurried(
        workers: Vec<Worker>,
        running_jobs: &[RunningJob],
        reason: &str,
        event_sink: &Arc<dyn EventSink>,
        width: usize,
    ) -> Vec<Worker> {
        let running = Self::jobs_of(running_jobs);
        let busy: std::collections::HashSet<usize> =
            running_jobs.iter().map(|r| r.worker_id).collect();

        Self::emit_job_events(&running, JobStatus::Stopping, None, None, event_sink);

        let mut keep = Vec::new();
        let mut kill = Vec::new();
        for (worker_id, worker) in workers.into_iter().enumerate() {
            if keep.len() < width && !busy.contains(&worker_id) && worker.is_alive().await {
                keep.push(worker);
            } else {
                kill.push(worker);
            }
        }
        log::info!(
            "Stopping: {} workers killed, {} kept for teardown",
            kill.len(),
            keep.len()
        );
        Self::force_kill_workers_parallel(kill).await;

        Self::emit_job_events(
            &running,
            JobStatus::Completed,
            Some(Outcome::Stop),
            Some(reason.to_string()),
            event_sink,
        );
        keep
    }

    pub async fn force_kill(&mut self) -> Result<(), String> {
        log::info!("Force killing execution - no teardown phases will run");

        let (running_jobs_info, queued_jobs_info, _, pending_retry_handles, reason) = {
            let mut state = self.state.write().await;
            state.request_shutdown(crate::state::ShutdownCause::Operator);
            let reason = state.kill_reason();
            state.mark_outstanding_slots_stopped(&reason);
            state.mark_pending_retry_scopes_stopped(&reason);
            let handles = state.abort_retries_for_shutdown(|_| true);
            let result = Self::collect_and_complete_jobs(&mut state, reason.clone(), false);
            self.cancel_unfinished_sequence_jobs(&mut state, &reason);
            self.settle_sequences(&mut state).await;
            (result.0, result.1, result.2, handles, reason)
        };

        Self::emit_aborted_retries(&pending_retry_handles, &reason, &self.event_sink);

        let running_jobs_for_emit = Self::jobs_of(&running_jobs_info);

        Self::emit_job_events(
            &running_jobs_for_emit,
            JobStatus::Stopping,
            None,
            None,
            &self.event_sink,
        );

        log::info!(
            "Force killing {} workers ({} running, {} queued)",
            self.workers.read().await.len(),
            running_jobs_info.len(),
            queued_jobs_info.len()
        );

        let workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };

        Self::force_kill_workers_parallel(workers).await;

        // STOP, as the state records it (`new_interrupted`) and as
        // `force_kill_immediate` reports it: the kill cut the phase, the
        // phase did not fail.
        Self::emit_job_events(
            &running_jobs_for_emit,
            JobStatus::Completed,
            Some(Outcome::Stop),
            Some(reason.clone()),
            &self.event_sink,
        );

        Self::emit_job_events(
            &queued_jobs_info,
            JobStatus::Skipped,
            Some(Outcome::Skip),
            Some(crate::state::not_run(&reason)),
            &self.event_sink,
        );

        log::info!("Force killing all plug services");

        let resource_manager = self.resource_manager.read().await;
        if let Err(e) = resource_manager
            .force_destroy_all_plugs(&self.event_sink)
            .await
        {
            log::warn!("Cannot kill plugs: {e}");
        }
        drop(resource_manager);
        self.state.write().await.settle_plug_scope_ops();

        log::info!("Execution force killed - all processes terminated");

        Ok(())
    }

    /// Stop the phases running right now without giving up the teardown.
    ///
    /// Raises the operator stop, then kills every worker busy with a
    /// main or setup phase, in parallel: each interrupted job completes
    /// as stopped under the raised flag (no replacement worker is
    /// started), `execute_all` drains, and the caller's `shutdown()`
    /// runs the teardown phases before the plugs are released. A worker
    /// already running a teardown phase is left alone: killing it mid
    /// power-off would leave the bench on with nothing to re-run it, and
    /// `execute_all` waits for it. For stops with an external deadline
    /// (console close, SIGTERM): a graceful stop waits for an hours-long
    /// phase, a force kill skips powering the bench down.
    pub async fn interrupt_running_jobs(
        state: Arc<RwLock<OrchestratorState>>,
        workers: Arc<RwLock<Vec<Worker>>>,
    ) {
        let (busy, spared) = {
            let mut state = state.write().await;
            state.request_shutdown(crate::state::ShutdownCause::Operator);
            let mut busy = Vec::new();
            let mut spared = 0usize;
            for id in 0..state.worker_state.num_workers() {
                let Some(job_id) = state.worker_state.get_worker_job(id) else {
                    continue;
                };
                if state
                    .job_info
                    .get(&job_id)
                    .is_some_and(|info| info.is_teardown())
                {
                    spared += 1;
                } else {
                    busy.push(id);
                }
            }
            (busy, spared)
        };
        if spared > 0 {
            log::info!(
                "{} teardown phase(s) already running are left to finish",
                spared
            );
        }
        if busy.is_empty() {
            return;
        }
        log::info!("Interrupting {} running phase(s)", busy.len());
        let targets: Vec<(usize, Worker)> = {
            let workers = workers.read().await;
            busy.into_iter()
                .filter_map(|id| workers.get(id).cloned().map(|w| (id, w)))
                .collect()
        };
        join_all(targets.into_iter().map(|(id, mut worker)| async move {
            if let Err(e) = worker.interrupt_current_job().await {
                log::debug!("Worker {} interrupt failed: {}", id, e);
            }
        }))
        .await;
    }

    pub async fn force_kill_immediate(
        state: Arc<RwLock<OrchestratorState>>,
        workers: Arc<RwLock<Vec<Worker>>>,
        resource_manager: Arc<RwLock<crate::plugs::manager::ResourceManager>>,
        _execution_id: Option<String>,
        event_sink: Arc<dyn EventSink>,
    ) -> Result<(), String> {
        // Set shutdown flags and take pending retry handles atomically
        let (pending_retry_handles, reason) = {
            let mut state_guard = state.write().await;
            state_guard.request_shutdown(crate::state::ShutdownCause::Operator);
            let reason = state_guard.kill_reason();
            state_guard.mark_outstanding_slots_stopped(&reason);
            state_guard.mark_pending_retry_scopes_stopped(&reason);
            state_guard.force_kill_requested = true;
            (state_guard.abort_retries_for_shutdown(|_| true), reason)
        };
        Self::emit_aborted_retries(&pending_retry_handles, &reason, &event_sink);

        log::info!("Force killing all workers immediately");

        // Kill all workers FIRST, in parallel for maximum speed
        // This prevents workers from completing teardown phases before we mark them as skipped
        let kill_tasks: Vec<_> = {
            let workers_guard = workers.read().await;
            workers_guard
                .iter()
                .map(|worker| {
                    let mut worker_clone = worker.clone();
                    tokio::spawn(async move {
                        let result = worker_clone.force_shutdown().await;
                        result
                    })
                })
                .collect()
        };

        // Wait for all kills to complete (truly in parallel)
        let _ = join_all(kill_tasks).await;

        // NOW collect and mark jobs as complete, after workers are dead
        let (running_jobs_info, queued_jobs_info, _) = {
            let mut state_guard = state.write().await;
            Self::collect_and_complete_jobs(&mut state_guard, reason.clone(), false)
        };

        let running_jobs_for_emit = Self::jobs_of(&running_jobs_info);

        Self::emit_job_events(
            &running_jobs_for_emit,
            JobStatus::Completed,
            Some(Outcome::Stop),
            Some(reason.clone()),
            &event_sink,
        );

        Self::emit_job_events(
            &queued_jobs_info,
            JobStatus::Skipped,
            Some(Outcome::Skip),
            Some(crate::state::not_run(&reason)),
            &event_sink,
        );

        log::info!("Force killing all plug services");

        let resource_manager_guard = resource_manager.read().await;
        if let Err(e) = resource_manager_guard
            .force_destroy_all_plugs(&event_sink)
            .await
        {
            log::warn!("Cannot kill plugs: {e}");
        }
        drop(resource_manager_guard);

        log::info!("Execution force killed - all processes terminated");

        Ok(())
    }
}

#[cfg(test)]
mod interrupted_job_tests {
    use super::*;
    use crate::procedure::schema::StageScope;
    use crate::state::{JobInfo, OrchestratorState};

    use crate::test_support::job;

    /// The operator paths record the phase they interrupt as `Stop` with
    /// no `error`. Recording it as `new_error` once made every force kill
    /// with a phase in flight aggregate to ERROR (TP-957, protocol run 4).
    /// The reason survives as a log line on the phase.
    #[test]
    fn running_job_is_recorded_as_interrupted_not_as_an_error() {
        let mut state = OrchestratorState::new(1);
        let running = job(StageScope::Main);
        let queued = job(StageScope::Main);
        state
            .job_info
            .insert(running.id, JobInfo::from_job(&running));
        state.worker_state.assign_job(0, running.id).unwrap();
        state.enqueue_job(queued);

        let (running_info, queued_info, _) = Orchestrator::collect_and_complete_jobs(
            &mut state,
            "Killed by operator".to_string(),
            false,
        );
        assert_eq!(running_info.len(), 1);
        assert_eq!(queued_info.len(), 1);

        let result = state
            .job_results
            .get(&running.id)
            .expect("running job completed");
        assert_eq!(result.phase_outcome, Outcome::Stop);
        assert!(result.error.is_none(), "an interruption is not an error");
        assert!(!result.is_failure());
        assert_eq!(result.logs.len(), 1);
        assert_eq!(result.logs[0].message, "Killed by operator");
    }

    /// Every job the drain settles counts done once and the total counts
    /// it once: a running job, a queued one, a deferred slot's job, the
    /// parked shared teardown. Requeued teardown is not counted again.
    #[test]
    fn drain_closes_progress_and_requeue_does_not_count_twice() {
        let mut state = OrchestratorState::new(1);
        let running = job(StageScope::Main);
        state.note_dispatched(&running);
        state.enqueue_job(running.clone());
        state.job_queue.clear();
        state
            .job_info
            .insert(running.id, JobInfo::from_job(&running));
        state.worker_state.assign_job(0, running.id).unwrap();
        state.enqueue_job(job(StageScope::Main));
        let teardown = job(StageScope::TeardownEach);
        state.enqueue_job(teardown);
        state
            .pending_slot_jobs
            .push(("b".into(), vec![job(StageScope::Main)]));
        state
            .teardown_procedure_jobs
            .push(job(StageScope::TeardownAll));

        let (_, _, teardown_jobs) =
            Orchestrator::collect_and_complete_jobs(&mut state, "stop".into(), true);
        assert_eq!(teardown_jobs.len(), 2, "the queued and the parked teardown");
        for job in teardown_jobs {
            state.requeue_job(job);
        }
        assert_eq!(state.total_jobs_submitted, 5);
        assert_eq!(
            state.original_jobs_completed, 3,
            "running, queued, deferred"
        );
        // The two teardowns complete later, one count each.
        assert_eq!(state.job_queue.len(), 2);
    }

    /// A retry the drain settles keeps its attempt number: as attempt 0 it
    /// tied with the attempt that asked for it.
    #[test]
    fn drained_retry_keeps_its_attempt_number() {
        let mut state = OrchestratorState::new(1);
        let retry = job(StageScope::Main).create_retry_job();
        state.enqueue_retry_job(retry.clone());

        let (_, queued, _) =
            Orchestrator::collect_and_complete_jobs(&mut state, "stop".into(), false);
        assert_eq!(queued[0].retry_count, 1);
        assert_eq!(state.job_results[&retry.id].retry_count, 1);
    }

    #[derive(Default)]
    struct Sink(std::sync::Mutex<Vec<ExecutionEvent>>);

    impl EventSink for Sink {
        fn emit(&self, event: &ExecutionEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    /// A force kill reports the phase it cut as STOP, with its own stage,
    /// like the state records it. One kill path used to say ERROR.
    #[tokio::test]
    async fn force_kill_reports_the_cut_phase_as_stop_with_its_stage() {
        let mut state = OrchestratorState::new(1);
        let running = job(StageScope::TeardownEach);
        state
            .job_info
            .insert(running.id, JobInfo::from_job(&running));
        state.worker_state.assign_job(0, running.id).unwrap();
        let sink = Arc::new(Sink::default());
        let event_sink: Arc<dyn EventSink> = sink.clone();

        Orchestrator::force_kill_immediate(
            Arc::new(RwLock::new(state)),
            Arc::new(RwLock::new(Vec::new())),
            Arc::new(RwLock::new(crate::plugs::manager::ResourceManager::new(
                std::env::temp_dir(),
            ))),
            None,
            event_sink,
        )
        .await
        .unwrap();

        let events = sink.0.lock().unwrap();
        let completed = events
            .iter()
            .find_map(|e| match e {
                ExecutionEvent::JobProgress {
                    job_id,
                    status: JobStatus::Completed,
                    outcome,
                    stage_scope,
                    ..
                } if *job_id == running.id.to_string() => Some((*outcome, *stage_scope)),
                _ => None,
            })
            .expect("the cut phase is reported");
        assert_eq!(completed, (Some(Outcome::Stop), StageScope::TeardownEach));
    }
}
