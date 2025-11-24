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
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::execution::job::{Job, JobResult, JobStatus, Outcome};
use crate::execution::state::OrchestratorState;
use crate::execution::worker::Worker;
use crate::procedure::schema::StageScope;

use super::super::orchestrator::Orchestrator;

impl Orchestrator {
    fn is_teardown_job(job: &Job) -> bool {
        matches!(
            job.stage_scope,
            StageScope::TeardownEach | StageScope::TeardownAll
        )
    }

    fn collect_and_complete_jobs(
        state: &mut OrchestratorState,
        running_error_msg: String,
        queued_error_msg: Option<String>,
        partition_teardown: bool,
    ) -> (
        Vec<(usize, uuid::Uuid, String, String, String)>,
        Vec<(uuid::Uuid, String, String, String)>,
        Vec<Job>,
    ) {
        let mut running_jobs_info = Vec::new();
        let mut queued_jobs_info = Vec::new();

        for worker_id in 0..state.worker_state.num_workers() {
            if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
                if let Some(info) = state.job_info.get(&job_id) {
                    running_jobs_info.push((
                        worker_id,
                        job_id,
                        info.phase_key.clone(),
                        info.phase_name.clone(),
                        info.slot_id.clone().unwrap_or_else(|| "<shared>".to_string()),
                    ));
                }
                state.complete_job(job_id, JobResult::new_error(running_error_msg.clone()));
            }
        }

        let (mut teardown_jobs, regular_jobs): (Vec<Job>, Vec<Job>) = if partition_teardown {
            state.job_queue.drain(..).partition(Self::is_teardown_job)
        } else {
            (Vec::new(), state.job_queue.drain(..).collect())
        };

        let pending_slot_jobs: Vec<Job> = state
            .pending_slot_jobs
            .drain(..)
            .flat_map(|(_, jobs)| jobs)
            .collect();

        if partition_teardown {
            teardown_jobs.append(&mut state.teardown_procedure_jobs);
        } else {
            let teardown_procedure_jobs: Vec<Job> =
                state.teardown_procedure_jobs.drain(..).collect();
            for job in teardown_procedure_jobs {
                queued_jobs_info.push((
                    job.id,
                    job.phase_key.clone(),
                    job.phase_name.clone(),
                    job.slot_id
                        .clone()
                        .unwrap_or_else(|| "<shared>".to_string()),
                ));
                state.complete_job(job.id, JobResult::new_skip());
            }
        }

        for job in &regular_jobs {
            queued_jobs_info.push((
                job.id,
                job.phase_key.clone(),
                job.phase_name.clone(),
                job.slot_id
                    .clone()
                    .unwrap_or_else(|| "<shared>".to_string()),
            ));
            let result = if queued_error_msg.is_some() {
                JobResult::new_skip()
            } else {
                JobResult::new_skip()
            };
            state.complete_job(job.id, result);
        }

        for job in &pending_slot_jobs {
            queued_jobs_info.push((
                job.id,
                job.phase_key.clone(),
                job.phase_name.clone(),
                job.slot_id
                    .clone()
                    .unwrap_or_else(|| "<shared>".to_string()),
            ));
            state.complete_job(job.id, JobResult::new_skip());
        }

        (running_jobs_info, queued_jobs_info, teardown_jobs)
    }

    fn emit_job_event(
        job_id: uuid::Uuid,
        slot_id: Option<String>,
        phase_key: &str,
        phase_name: &str,
        stage_scope: StageScope,
        status: JobStatus,
        outcome: Option<Outcome>,
        error: Option<String>,
        worker_id: Option<usize>,
        app_handle: Option<&AppHandle>,
    ) {
        if let Some(app) = app_handle {
            let progress = super::super::orchestrator::JobProgress {
                job_id: job_id.to_string(),
                slot_id,
                phase_key: phase_key.to_string(),
                phase_name: phase_name.to_string(),
                stage_scope,
                status,
                worker_id,
                started_at: None,
                timeout_ms: None,
                outcome,
                retry_count: 0,
                error,
            };
            let _ = super::super::orchestrator::JobProgressEvent(progress).emit(app);
        }
    }

    fn emit_job_events(
        jobs: &[(uuid::Uuid, String, String, String)],
        status: JobStatus,
        outcome: Option<Outcome>,
        error: Option<String>,
        app_handle: Option<&AppHandle>,
    ) {
        if let Some(app) = app_handle {
            for (job_id, phase_key, phase_name, slot_id) in jobs {
                Self::emit_job_event(
                    *job_id,
                    if slot_id == "<shared>" { None } else { Some(slot_id.to_string()) },
                    phase_key,
                    phase_name,
                    StageScope::Main,
                    status,
                    outcome,
                    error.clone(),
                    None,
                    Some(app),
                );
            }
        }
    }

    async fn shutdown_workers_gracefully(
        workers: &mut [Worker],
        running_jobs_info: &[(usize, uuid::Uuid, String, String, String)],
        app_handle: Option<&AppHandle>,
    ) {
        use std::collections::HashMap;

        let job_map: HashMap<usize, (uuid::Uuid, String, String, String)> = running_jobs_info
            .iter()
            .map(|(worker_id, job_id, phase_key, phase_name, slot_id)| {
                (
                    *worker_id,
                    (
                        *job_id,
                        phase_key.clone(),
                        phase_name.clone(),
                        slot_id.clone(),
                    ),
                )
            })
            .collect();

        // Step 1: Emit "stopping" events for all workers with jobs immediately
        for (worker_id, _) in workers.iter().enumerate() {
            if let Some((job_id, phase_key, phase_name, slot_id)) = job_map.get(&worker_id) {
                if let Some(app) = app_handle {
                    log::debug!(
                        "Emitting status=stopping for phase={}, slot={}",
                        phase_name, slot_id
                    );
                    Self::emit_job_event(
                        *job_id,
                        if slot_id == "<shared>" { None } else { Some(slot_id.to_string()) },
                        phase_key,
                        phase_name,
                        StageScope::Main,
                        JobStatus::Stopping,
                        None,
                        None,
                        Some(worker_id),
                        Some(app),
                    );
                }
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
            if let Some((job_id, phase_key, phase_name, slot_id)) = job_map.get(&worker_id) {
                if let Some(app) = app_handle {
                    log::debug!(
                        "Emitting outcome=stop for phase={}, slot={}",
                        phase_name, slot_id
                    );
                    Self::emit_job_event(
                        *job_id,
                        if slot_id == "<shared>" { None } else { Some(slot_id.to_string()) },
                        phase_key,
                        phase_name,
                        StageScope::Main,
                        JobStatus::Completed,
                        Some(Outcome::Stop),
                        Some("Execution stopped by user".to_string()),
                        None,
                        Some(app),
                    );
                }
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
                            log::error!("Worker {} kill failed: {}", idx, e)
                        }
                    }
                    result
                }
            })
            .collect();

        futures::future::join_all(kill_futures).await;
    }

    async fn execute_teardown_jobs(
        &mut self,
        teardown_jobs: Vec<Job>,
        app_handle: Option<&AppHandle>,
    ) -> Result<(), String> {
        const TEARDOWN_TIMEOUT_SECS: u64 = 30;
        const NUM_TEARDOWN_WORKERS: usize = 2;

        let mut teardown_workers = Vec::new();
        for i in 0..NUM_TEARDOWN_WORKERS {
            let mut worker = Worker::new(i, self.procedure_dir.clone());
            worker.start(app_handle).await?;
            teardown_workers.push(worker);
        }

        // Re-populate state with teardown jobs (phases stay pending until actually started)
        {
            let mut state = self.state.write().await;
            for job in teardown_jobs {
                state.enqueue_job(job);
            }
            state.shutdown_requested = false; // Temporarily allow execution
        }

        // Store teardown workers
        {
            let mut workers = self.workers.write().await;
            *workers = teardown_workers;
        }

        // Execute teardown jobs with timeout
        let teardown_result = tokio::time::timeout(
            Duration::from_secs(TEARDOWN_TIMEOUT_SECS),
            self.run_teardown_loop(app_handle),
        )
        .await;

        // Shutdown teardown workers
        let mut workers = self.workers.write().await;
        for worker in workers.iter_mut() {
            let _ = worker.shutdown_with_timeout(1000).await;
        }
        workers.clear();

        match teardown_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("Teardown execution failed: {}", e)),
            Err(_) => {
                // Timeout - force complete remaining jobs
                let mut state = self.state.write().await;
                while let Some(job) = state.job_queue.pop_front() {
                    state.complete_job(
                        job.id,
                        JobResult::new_error("Teardown timeout during shutdown".to_string()),
                    );

                    // Emit timeout event for this job
                    if let Some(app) = app_handle {
                        let progress = super::super::orchestrator::JobProgress {
                            job_id: job.id.to_string(),
                            slot_id: job.slot_id.clone(),
                            phase_key: job.phase_key.clone(),
                            phase_name: job.phase_name.clone(),
                            stage_scope: job.stage_scope.clone(),
                            status: JobStatus::Completed,
                            worker_id: None,
                            started_at: None,
                            timeout_ms: job.timeout_ms,
                            outcome: Some(Outcome::Error),
                            retry_count: job.retry_count,
                            error: Some("Teardown timeout during shutdown".to_string()),
                        };
                        let _ = super::super::orchestrator::JobProgressEvent(progress).emit(app);
                    }
                }
                Err(format!(
                    "Teardown execution timed out after {}s",
                    TEARDOWN_TIMEOUT_SECS
                ))
            }
        }
    }

    async fn run_teardown_loop(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        // Create a new channel for teardown job completions
        let (teardown_tx, mut teardown_rx) = tokio::sync::mpsc::unbounded_channel();

        // Temporarily swap the completion_tx to use our teardown channel
        let original_tx = std::mem::replace(&mut self.completion_tx, teardown_tx);

        loop {
            let is_complete = {
                let state = self.state.read().await;
                state.job_queue.is_empty() && state.worker_state.count_busy() == 0
            };

            if is_complete {
                break;
            }

            // Schedule available teardown jobs (reuse existing scheduling logic)
            self.schedule_teardown_jobs(app_handle.cloned()).await?;

            // Process completion events
            tokio::select! {
                Some(event) = teardown_rx.recv() => {
                    self.handle_job_completion(event, app_handle.cloned()).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }

        // Restore the original completion_tx
        self.completion_tx = original_tx;

        Ok(())
    }

    async fn schedule_teardown_jobs(&self, app_handle: Option<AppHandle>) -> Result<(), String> {
        let jobs_to_spawn = {
            let mut state = self.state.write().await;
            let num_workers = state.worker_state.num_workers();
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

            // Spawn execution with app_handle so teardown phases emit "started" events
            let permit = self.job_semaphore.clone().acquire_owned().await.unwrap();
            self.spawn_job_execution(job, worker_id, worker, app_handle.clone(), permit)
                .await?;
        }

        Ok(())
    }
    /// Enhanced shutdown with graceful-to-force escalation
    pub async fn shutdown(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        // Check if force kill was requested
        {
            let state = self.state.read().await;
            if state.force_kill_requested {
                drop(state);
                return self.force_kill(app_handle).await;
            }

            // Check if already shut down
            if state.shutdown_requested && self.workers.read().await.is_empty() {
                return Ok(());
            }
        }

        let (running_jobs_info, regular_jobs_info, teardown_jobs, pending_retry_handles) = {
            let mut state = self.state.write().await;
            state.shutdown_requested = true;
            let handles = std::mem::take(&mut state.pending_delayed_retry_handles);
            let result = Self::collect_and_complete_jobs(
                &mut state,
                "Execution stopped by user".to_string(),
                None,
                true,
            );
            (result.0, result.1, result.2, handles)
        };

        // Abort all pending delayed retry tasks and emit stop events
        for pending in pending_retry_handles.iter() {
            pending.handle.abort();

            // Emit stop event for the aborted retry
            if let Some(ref app) = app_handle {
                use super::super::orchestrator::{JobProgress, JobProgressEvent};
                use crate::execution::job::{JobStatus, Outcome};
                use crate::procedure::schema::StageScope;

                let progress = JobProgress {
                    job_id: pending.job_id.to_string(),
                    slot_id: pending.slot_id.clone(),
                    phase_key: pending.phase_key.clone(),
                    phase_name: pending.phase_name.clone(),
                    stage_scope: StageScope::Main,
                    status: JobStatus::Skipped,
                    worker_id: None,
                    started_at: None,
                    timeout_ms: None,
                    outcome: Some(Outcome::Stop),
                    retry_count: 0,
                    error: Some("Retry cancelled due to shutdown".to_string()),
                };
                let _ = JobProgressEvent(progress).emit(&**app);
            }
        }

        let mut workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };

        Self::shutdown_workers_gracefully(
            &mut workers,
            &running_jobs_info,
            app_handle,
        )
        .await;

        Self::emit_job_events(
            &regular_jobs_info,
            JobStatus::Skipped,
            Some(Outcome::Skip),
            Some("Execution stopped by user".to_string()),
            app_handle,
        );

        // Execute teardown jobs if any
        if !teardown_jobs.is_empty() {
            log::info!(
                "Executing {} teardown phases before shutdown",
                teardown_jobs.len()
            );

            if let Err(e) = self.execute_teardown_jobs(teardown_jobs, app_handle).await {
                log::error!("Failed to execute teardown jobs: {}", e);
            }
        }

        // shutdown plug services
        let plug_service_manager = {
            let resource_manager = self.resource_manager.read().await;
            Arc::clone(resource_manager.get_plug_service_manager())
        };
        if let Err(e) = plug_service_manager.stop_all_services().await {
            log::error!(
                "Failed to stop plug services during shutdown: {}",
                e
            );
        }

        Ok(())
    }

    pub async fn force_kill(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        log::info!("Force killing execution - no teardown phases will run");

        let (running_jobs_info, queued_jobs_info, _, pending_retry_handles) = {
            let mut state = self.state.write().await;
            state.shutdown_requested = true;
            let handles = std::mem::take(&mut state.pending_delayed_retry_handles);
            let result = Self::collect_and_complete_jobs(
                &mut state,
                "Force killed by user".to_string(),
                None,
                false,
            );
            (result.0, result.1, result.2, handles)
        };

        // Abort all pending delayed retry tasks and emit error events
        for pending in pending_retry_handles.iter() {
            pending.handle.abort();

            // Emit stop event for the aborted retry
            if let Some(ref app) = app_handle {
                use super::super::orchestrator::{JobProgress, JobProgressEvent};
                use crate::execution::job::{JobStatus, Outcome};
                use crate::procedure::schema::StageScope;

                let progress = JobProgress {
                    job_id: pending.job_id.to_string(),
                    slot_id: pending.slot_id.clone(),
                    phase_key: pending.phase_key.clone(),
                    phase_name: pending.phase_name.clone(),
                    stage_scope: StageScope::Main,
                    status: JobStatus::Completed,
                    worker_id: None,
                    started_at: None,
                    timeout_ms: None,
                    outcome: Some(Outcome::Error),
                    retry_count: 0,
                    error: Some("Force killed by user".to_string()),
                };
                let _ = JobProgressEvent(progress).emit(&**app);
            }
        }

        let running_jobs_for_emit: Vec<(uuid::Uuid, String, String, String)> = running_jobs_info
            .iter()
            .map(|(_, job_id, phase_key, phase_name, slot_id)| {
                (
                    *job_id,
                    phase_key.clone(),
                    phase_name.clone(),
                    slot_id.clone(),
                )
            })
            .collect();

        Self::emit_job_events(
            &running_jobs_for_emit,
            JobStatus::Stopping,
            None,
            None,
            app_handle,
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

        Self::emit_job_events(
            &running_jobs_for_emit,
            JobStatus::Completed,
            Some(Outcome::Error),
            Some("Force killed by user".to_string()),
            app_handle,
        );

        Self::emit_job_events(
            &queued_jobs_info,
            JobStatus::Skipped,
            Some(Outcome::Skip),
            Some("Force killed by user".to_string()),
            app_handle,
        );

        log::info!("Force killing all plug services");

        let resource_manager = self.resource_manager.read().await;
        if let Err(e) = resource_manager.force_destroy_all_plugs(app_handle).await {
            log::warn!("Failed to force destroy plugs: {}", e);
        }
        drop(resource_manager);

        log::info!("Execution force killed - all processes terminated");

        Ok(())
    }

    pub async fn force_kill_immediate(
        state: Arc<RwLock<OrchestratorState>>,
        workers: Arc<RwLock<Vec<Worker>>>,
        resource_manager: Arc<RwLock<crate::plugs::manager::ResourceManager>>,
        _execution_id: Option<String>,
        app_handle: Option<AppHandle>,
    ) -> Result<(), String> {
        // Set shutdown flags FIRST to prevent new jobs from being scheduled
        {
            let mut state_guard = state.write().await;
            state_guard.shutdown_requested = true;
            state_guard.force_kill_requested = true;
        }

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
            Self::collect_and_complete_jobs(
                &mut state_guard,
                "Force killed by user".to_string(),
                None,
                false,
            )
        };

        let running_jobs_for_emit: Vec<(uuid::Uuid, String, String, String)> = running_jobs_info
            .iter()
            .map(|(_, job_id, phase_key, phase_name, slot_id)| {
                (
                    *job_id,
                    phase_key.clone(),
                    phase_name.clone(),
                    slot_id.clone(),
                )
            })
            .collect();

        if let Some(app) = app_handle.as_ref() {
            Self::emit_job_events(
                &running_jobs_for_emit,
                JobStatus::Completed,
                Some(Outcome::Stop),
                Some("Force killed by user".to_string()),
                Some(app),
            );

            Self::emit_job_events(
                &queued_jobs_info,
                JobStatus::Skipped,
                Some(Outcome::Skip),
                Some("Force killed by user".to_string()),
                Some(app),
            );
        }

        log::info!("Force killing all plug services");

        let resource_manager_guard = resource_manager.read().await;
        if let Err(e) = resource_manager_guard
            .force_destroy_all_plugs(app_handle.as_ref())
            .await
        {
            log::warn!("Failed to force destroy plugs: {}", e);
        }
        drop(resource_manager_guard);

        log::info!("Execution force killed - all processes terminated");

        Ok(())
    }
}
