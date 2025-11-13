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
use tauri::{AppHandle, Emitter};
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::execution::job::{Job, JobResult, JobStatus, Outcome};
use crate::execution::state::OrchestratorState;
use crate::execution::worker::Worker;
use crate::schema::procedure::StageScope;

use super::Orchestrator;

impl Orchestrator {
    /// Force kill a worker immediately without graceful shutdown
    pub async fn force_kill_worker(&mut self, worker_id: usize) -> Result<(), String> {
        let mut state = self.state.write().await;

        // Get job ID assigned to this worker
        if let Some(job_id) = state.worker_state.get_worker_job(worker_id) {
            // Mark job as terminated
            state.complete_job(
                job_id,
                JobResult::new_error("Force killed by user".to_string()),
            );
        }

        // Release lock before calling workers
        drop(state);

        // Force kill the worker immediately
        let mut workers = self.workers.write().await;
        if let Some(worker) = workers.get_mut(worker_id) {
            worker.force_shutdown().await?;
        }

        Ok(())
    }

    /// Stop a specific worker with automatic escalation from graceful to force
    pub async fn stop_worker(&mut self, worker_id: usize) -> Result<(), String> {
        // Kill the worker process
        {
            let mut workers = self.workers.write().await;
            if let Some(worker) = workers.get_mut(worker_id) {
                worker.shutdown_with_timeout(2000).await?;
            }
        }

        let mut new_worker = Worker::new(worker_id, self.procedure_dir.clone());
        new_worker.start(self.app_handle.as_ref()).await?;
        {
            let mut workers = self.workers.write().await;
            if worker_id < workers.len() {
                workers[worker_id] = new_worker;
            } else {
                return Err(format!(
                    "Cannot replace worker {} - orchestrator already shut down",
                    worker_id
                ));
            }
        }

        Ok(())
    }

    /// Stop all jobs for a specific slot
    pub async fn stop_slot(
        &mut self,
        slot_id: &str,
        app_handle: Option<&AppHandle>,
    ) -> Result<(), String> {
        let mut state = self.state.write().await;

        // Cancel queued jobs for this slot
        let cancelled_jobs = state.cancel_slot_jobs(slot_id);

        self.emit_cancelled_jobs(
            &cancelled_jobs,
            &format!("Slot {} cancelled", slot_id),
            JobStatus::Skipped,
            Outcome::Skip,
            app_handle,
        )
        .await;

        // Get workers processing this slot's jobs
        let workers_to_interrupt = state.get_workers_for_slot(slot_id);

        // Mark running jobs as cancelled
        for worker_id in &workers_to_interrupt {
            if let Some(job_id) = state.worker_state.get_worker_job(*worker_id) {
                state.complete_job(
                    job_id,
                    JobResult::new_error(format!("Slot {} cancelled by user", slot_id)),
                );
            }
        }

        drop(state); // Release lock before calling workers

        // Stop workers processing this slot
        let mut workers = self.workers.write().await;
        for worker_id in workers_to_interrupt {
            if let Some(worker) = workers.get_mut(worker_id) {
                // Try graceful interrupt first
                if let Err(_) = worker.interrupt_current_job().await {
                    // If interrupt fails, try shutdown with timeout
                    let _ = worker.shutdown_with_timeout(2000).await;
                }
            }
        }

        Ok(())
    }

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
                if let Some((phase_key, phase_name, slot_id)) = state.job_info.get(&job_id) {
                    running_jobs_info.push((
                        worker_id,
                        job_id,
                        phase_key.clone(),
                        phase_name.clone(),
                        slot_id.clone().unwrap_or_else(|| "<shared>".to_string()),
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
        execution_id: &Option<String>,
        job_id: uuid::Uuid,
        slot_id: &str,
        phase_key: &str,
        phase_name: &str,
        status: &str,
        outcome: Option<&str>,
        error: Option<&str>,
        app_handle: Option<&AppHandle>,
    ) {
        if let (Some(app), Some(exec_id)) = (app_handle, execution_id) {
            let _ = app.emit(
                "job-progress",
                serde_json::json!({
                    "execution_id": exec_id,
                    "job_id": job_id.to_string(),
                    "slot_id": slot_id,
                    "phase_key": phase_key,
                    "phase_name": phase_name,
                    "status": status,
                    "outcome": outcome,
                    "error": error,
                    "worker_id": None::<usize>,
                }),
            );
        }
    }

    fn emit_job_events(
        execution_id: &Option<String>,
        jobs: &[(uuid::Uuid, String, String, String)],
        status: &str,
        outcome: Option<&str>,
        error: Option<&str>,
        app_handle: Option<&AppHandle>,
    ) {
        if let Some(app) = app_handle {
            for (job_id, phase_key, phase_name, slot_id) in jobs {
                Self::emit_job_event(
                    execution_id,
                    *job_id,
                    slot_id,
                    phase_key,
                    phase_name,
                    status,
                    outcome,
                    error,
                    Some(app),
                );
            }
        }
    }

    async fn shutdown_workers_gracefully(
        workers: &mut [Worker],
        running_jobs_info: &[(usize, uuid::Uuid, String, String, String)],
        execution_id: &Option<String>,
        app_handle: Option<&AppHandle>,
    ) {
        use std::collections::HashMap;

        let job_map: HashMap<usize, (uuid::Uuid, String, String, String)> = running_jobs_info
            .iter()
            .map(|(worker_id, job_id, phase_key, phase_name, slot_id)| {
                (*worker_id, (*job_id, phase_key.clone(), phase_name.clone(), slot_id.clone()))
            })
            .collect();

        // Step 1: Emit "stopping" events for all workers with jobs immediately
        for (worker_id, _) in workers.iter().enumerate() {
            if let Some((job_id, phase_key, phase_name, slot_id)) = job_map.get(&worker_id) {
                if let Some(app) = app_handle {
                    crate::cli_output::debug(format!(
                        "Emitting status=stopping for phase={}, slot={}",
                        phase_name, slot_id
                    ));
                    Self::emit_job_event(
                        execution_id,
                        *job_id,
                        slot_id,
                        phase_key,
                        phase_name,
                        "stopping",
                        None, // Don't send outcome - stopping is just a status
                        None,
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

        // Step 3: Emit "aborted" outcome for all workers with jobs
        for (worker_id, _) in workers.iter().enumerate() {
            if let Some((job_id, phase_key, phase_name, slot_id)) = job_map.get(&worker_id) {
                if let Some(app) = app_handle {
                    crate::cli_output::debug(format!(
                        "Emitting outcome=aborted for phase={}, slot={}",
                        phase_name, slot_id
                    ));
                    Self::emit_job_event(
                        execution_id,
                        *job_id,
                        slot_id,
                        phase_key,
                        phase_name,
                        "completed",
                        Some("aborted"),
                        Some("Execution stopped by user"),
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
                    crate::cli_output::debug(format!("Force killing worker {}", idx));
                    let result = worker_clone.force_shutdown().await;
                    match &result {
                        Ok(_) => {}
                        Err(e) => {
                            crate::cli_output::error(format!("Worker {} kill failed: {}", idx, e))
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
                        let _ = app.emit(
                            "job-progress",
                            serde_json::json!({
                                "execution_id": self.execution_id,
                                "job_id": job.id.to_string(),
                                "slot_id": job.slot_id.clone().unwrap_or_else(|| "<shared>".to_string()),
                                "phase_name": job.phase_name.clone(),
                                "status": "error",
                                "error": "Teardown timeout during shutdown",
                                "worker_id": None::<usize>,
                            }),
                        );
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

        let (running_jobs_info, regular_jobs_info, teardown_jobs) = {
            let mut state = self.state.write().await;
            state.shutdown_requested = true;
            Self::collect_and_complete_jobs(
                &mut state,
                "Execution stopped by user".to_string(),
                None,
                true,
            )
        };

        let mut workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };

        Self::shutdown_workers_gracefully(
            &mut workers,
            &running_jobs_info,
            &self.execution_id,
            app_handle,
        )
        .await;

        Self::emit_job_events(
            &self.execution_id,
            &regular_jobs_info,
            "skipped",
            Some("skip"),
            Some("Execution stopped by user"),
            app_handle,
        );

        // Execute teardown jobs if any
        if !teardown_jobs.is_empty() {
            crate::cli_output::print_section(
                crate::cli_output::Section::System,
                format!(
                    "Executing {} teardown phases before shutdown",
                    teardown_jobs.len()
                ),
            );

            if let Err(e) = self.execute_teardown_jobs(teardown_jobs, app_handle).await {
                crate::cli_output::error(format!("Failed to execute teardown jobs: {}", e));
            }
        }

        // shutdown plug services
        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            "Shutting down plug services during orchestrator teardown",
        );

        let plug_service_manager = {
            let resource_manager = self.resource_manager.read().await;
            Arc::clone(resource_manager.get_plug_service_manager())
        };
        if let Err(e) = plug_service_manager.stop_all_services().await {
            crate::cli_output::error(format!(
                "Failed to stop plug services during shutdown: {}",
                e
            ));
        }

        Ok(())
    }

    pub async fn force_kill(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            "Force killing execution - no teardown phases will run",
        );

        let (running_jobs_info, queued_jobs_info, _) = {
            let mut state = self.state.write().await;
            state.shutdown_requested = true;
            Self::collect_and_complete_jobs(
                &mut state,
                "Force killed by user".to_string(),
                None,
                false,
            )
        };

        let running_jobs_for_emit: Vec<(uuid::Uuid, String, String, String)> = running_jobs_info
            .iter()
            .map(|(_, job_id, phase_key, phase_name, slot_id)| (*job_id, phase_key.clone(), phase_name.clone(), slot_id.clone()))
            .collect();

        Self::emit_job_events(
            &self.execution_id,
            &running_jobs_for_emit,
            "stopping",
            None,
            None,
            app_handle,
        );

        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            format!(
                "Force killing {} workers ({} running, {} queued)",
                self.workers.read().await.len(),
                running_jobs_info.len(),
                queued_jobs_info.len()
            ),
        );

        let workers = {
            let mut guard = self.workers.write().await;
            std::mem::take(&mut *guard)
        };

        Self::force_kill_workers_parallel(workers).await;

        Self::emit_job_events(
            &self.execution_id,
            &running_jobs_for_emit,
            "error",
            Some("error"),
            Some("Force killed by user"),
            app_handle,
        );

        Self::emit_job_events(
            &self.execution_id,
            &queued_jobs_info,
            "skipped",
            Some("skip"),
            Some("Force killed by user"),
            app_handle,
        );

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            "Force killing all plug services",
        );

        let resource_manager = self.resource_manager.read().await;
        if let Err(e) = resource_manager.force_destroy_all_plugs(app_handle).await {
            crate::cli_output::warning(format!("Failed to force destroy plugs: {}", e));
        }
        drop(resource_manager);

        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            "Execution force killed - all processes terminated",
        );

        Ok(())
    }

    pub async fn force_kill_immediate(
        state: Arc<RwLock<OrchestratorState>>,
        workers: Arc<RwLock<Vec<Worker>>>,
        resource_manager: Arc<RwLock<crate::plugs::manager::ResourceManager>>,
        execution_id: Option<String>,
        app_handle: Option<AppHandle>,
    ) -> Result<(), String> {
        // Set shutdown flags FIRST to prevent new jobs from being scheduled
        {
            let mut state_guard = state.write().await;
            state_guard.shutdown_requested = true;
            state_guard.force_kill_requested = true;
        }

        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            "Force killing all workers immediately",
        );

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
            .map(|(_, job_id, phase_key, phase_name, slot_id)| (*job_id, phase_key.clone(), phase_name.clone(), slot_id.clone()))
            .collect();

        if let (Some(app), Some(exec_id)) = (app_handle.as_ref(), execution_id.as_ref()) {
            Self::emit_job_events(
                &Some(exec_id.clone()),
                &running_jobs_for_emit,
                "aborted",
                Some("aborted"),
                Some("Force killed by user"),
                Some(app),
            );

            Self::emit_job_events(
                &Some(exec_id.clone()),
                &queued_jobs_info,
                "skipped",
                Some("skip"),
                Some("Force killed by user"),
                Some(app),
            );
        }

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            "Force killing all plug services",
        );

        let resource_manager_guard = resource_manager.read().await;
        if let Err(e) = resource_manager_guard.force_destroy_all_plugs(app_handle.as_ref()).await {
            crate::cli_output::warning(format!("Failed to force destroy plugs: {}", e));
        }
        drop(resource_manager_guard);

        crate::cli_output::print_section(
            crate::cli_output::Section::System,
            "Execution force killed - all processes terminated",
        );

        Ok(())
    }
}
