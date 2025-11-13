use crate::execution::job::{Job, JobResult};
use crate::execution::worker_state::WorkerStateTracker;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

/// Centralized state for the orchestrator to reduce lock complexity
///
/// Lock ordering convention:
/// 1. OrchestratorState (this struct)
/// 2. ResourceManager (if needed)
/// 3. Individual Workers (if needed)
///
/// Never acquire locks in reverse order to prevent deadlocks
#[derive(Debug)]
pub struct OrchestratorState {
    pub job_queue: VecDeque<Job>,
    pub completed_jobs: HashSet<Uuid>,
    pub job_results: HashMap<Uuid, JobResult>,
    pub job_info: HashMap<Uuid, (String, String, Option<String>)>, // Map job ID to (phase_key, phase_name, slot_id where None = shared)
    pub worker_state: WorkerStateTracker,
    pub total_jobs_submitted: usize, // Track the initial total job count (not repeats)
    pub original_jobs_completed: usize, // Track completed original jobs (not repeats)
    pub job_to_slot: HashMap<Uuid, String>, // Map job IDs to slot IDs
    pub slot_jobs: HashMap<String, HashSet<Uuid>>, // Map slot IDs to their job IDs
    pub shutdown_requested: bool,    // Flag to signal shutdown
    pub force_kill_requested: bool,  // Flag to signal immediate force kill
    pub should_stop_on_first_failure: bool, // Stop execution on first phase failure
    pub pending_slot_jobs: Vec<(String, Vec<Job>)>, // For slot-first: remaining slots to process
    pub teardown_procedure_jobs: Vec<Job>, // Teardown procedure jobs to run after all slots
}

impl OrchestratorState {
    pub fn new(num_workers: usize) -> Self {
        Self {
            job_queue: VecDeque::new(),
            completed_jobs: HashSet::new(),
            job_results: HashMap::new(),
            job_info: HashMap::new(),
            worker_state: WorkerStateTracker::new(num_workers),
            total_jobs_submitted: 0,
            original_jobs_completed: 0,
            job_to_slot: HashMap::new(),
            slot_jobs: HashMap::new(),
            shutdown_requested: false,
            force_kill_requested: false,
            should_stop_on_first_failure: false,
            pending_slot_jobs: Vec::new(),
            teardown_procedure_jobs: Vec::new(),
        }
    }

    /// Check if execution is complete
    pub fn is_complete(&self) -> bool {
        (self.job_queue.is_empty() && self.worker_state.count_busy() == 0)
            || self.shutdown_requested
    }

    /// Get the next ready job from the queue
    pub fn pop_ready_job(&mut self, check_deps: impl Fn(&Job) -> bool) -> Option<Job> {
        let mut checked_jobs = Vec::new();
        let mut ready_job = None;

        // Find first job with satisfied dependencies
        while let Some(job) = self.job_queue.pop_front() {
            if job.dependencies_satisfied(&self.completed_jobs) && check_deps(&job) {
                ready_job = Some(job);
                break;
            } else {
                checked_jobs.push(job);
            }
        }

        // Put non-ready jobs back
        for job in checked_jobs.into_iter().rev() {
            self.job_queue.push_front(job);
        }

        ready_job
    }

    /// Mark a job as active
    pub fn mark_job_active(&mut self, job_id: Uuid, worker_id: usize) -> Result<(), String> {
        self.worker_state.assign_job(worker_id, job_id)
    }

    /// Complete a job
    pub fn complete_job(&mut self, job_id: Uuid, result: JobResult) {
        self.completed_jobs.insert(job_id);
        self.job_results.insert(job_id, result);
        self.worker_state.release_by_job(&job_id);
    }

    /// Complete an original (non-repeat) job
    pub fn complete_original_job(&mut self, job_id: Uuid, result: JobResult) {
        self.complete_job(job_id, result);
        self.original_jobs_completed += 1;
    }

    pub fn complete_job_with_info(
        &mut self,
        job_id: Uuid,
        phase_key: String,
        phase_name: String,
        slot_id: Option<String>,
        result: JobResult,
    ) {
        self.job_info
            .insert(job_id, (phase_key, phase_name, slot_id));
        self.complete_original_job(job_id, result);
    }

    /// Remove a job from active status without completing it (for repeats)
    pub fn remove_active_job(&mut self, job_id: &Uuid) {
        self.worker_state.release_by_job(job_id);
    }

    /// Cancel all jobs for a specific slot
    pub fn cancel_slot_jobs(&mut self, slot_id: &str) -> Vec<Job> {
        let mut cancelled_jobs = Vec::new();

        // Remove from queue and collect cancelled jobs, but NEVER cancel teardown phases
        self.job_queue.retain(|job| {
            if job.slot_id.as_deref() == Some(slot_id)
                && !matches!(
                    job.stage_scope,
                    crate::schema::procedure::StageScope::TeardownEach
                )
                && !matches!(
                    job.stage_scope,
                    crate::schema::procedure::StageScope::TeardownAll
                )
            {
                cancelled_jobs.push(job.clone());
                false
            } else {
                true
            }
        });

        // Mark cancelled jobs as completed with skipped status
        for job in &cancelled_jobs {
            let result = JobResult {
                phase_result: crate::execution::job::PhaseResult::Skip,
                phase_outcome: None, // Will be computed as Skip
                next_action: None, // Will be computed in completion handler
                timeout_secs: None,
                error: None,
                exit_code: None,
                measurements: vec![],
                logs: vec![],
                started_at: chrono::Utc::now(),
                completed_at: chrono::Utc::now(),
                resource_metrics: None,
                unit: None,
                retry_count: job.retry_count,
            };
            // Cancelled jobs from queue are original jobs (not yet started)
            if job.retry_count == 0 {
                self.complete_original_job(job.id, result);
            } else {
                self.complete_job(job.id, result);
            }
        }

        cancelled_jobs
    }

    /// Cancel all remaining jobs (for stop_on_first_failure)
    pub fn cancel_all_jobs(&mut self, reason: &str) -> Vec<Job> {
        let mut cancelled_jobs = Vec::new();

        // Collect and clear all jobs from queue
        while let Some(job) = self.job_queue.pop_front() {
            cancelled_jobs.push(job);
        }

        // Mark all cancelled jobs as completed with error
        for job in &cancelled_jobs {
            let result = JobResult {
                phase_result: crate::execution::job::PhaseResult::Continue,
                phase_outcome: None, // Will be computed as Error
                next_action: None, // Will be computed in completion handler
                timeout_secs: None,
                error: Some(reason.to_string()),
                exit_code: None,
                measurements: vec![],
                logs: vec![],
                started_at: chrono::Utc::now(),
                completed_at: chrono::Utc::now(),
                resource_metrics: None,
                unit: None,
                retry_count: job.retry_count,
            };
            // Cancelled jobs from queue are original jobs (not yet started)
            if job.retry_count == 0 {
                self.complete_original_job(job.id, result);
            } else {
                self.complete_job(job.id, result);
            }
        }

        // Set shutdown flag to stop execution
        self.shutdown_requested = true;

        cancelled_jobs
    }

    /// Add a job to the queue
    pub fn enqueue_job(&mut self, job: Job) {
        // Track job-slot relationship
        let slot_id = job.slot_id.clone();
        let job_id = job.id;

        if let Some(slot_id_str) = slot_id {
            self.job_to_slot.insert(job_id, slot_id_str.clone());
            self.slot_jobs
                .entry(slot_id_str)
                .or_default()
                .insert(job_id);
        }

        self.job_queue.push_back(job);
        self.total_jobs_submitted += 1;
    }

    /// Add a high-priority job to the front of the queue
    pub fn enqueue_priority_job(&mut self, job: Job) {
        // Track job-slot relationship
        let slot_id = job.slot_id.clone();
        let job_id = job.id;

        if let Some(slot_id_str) = slot_id {
            self.job_to_slot.insert(job_id, slot_id_str.clone());
            self.slot_jobs
                .entry(slot_id_str)
                .or_default()
                .insert(job_id);
        }

        self.job_queue.push_front(job);
        self.total_jobs_submitted += 1;
    }

    /// Add a retry job to the front of the queue without incrementing total count
    pub fn enqueue_retry_job(&mut self, job: Job) {
        // Track job-slot relationship
        let slot_id = job.slot_id.clone();
        let job_id = job.id;

        if let Some(slot_id_str) = slot_id {
            self.job_to_slot.insert(job_id, slot_id_str.clone());
            self.slot_jobs
                .entry(slot_id_str)
                .or_default()
                .insert(job_id);
        }

        self.job_queue.push_front(job);
        // Don't increment total_jobs_submitted for retries
    }

    /// Get all workers currently processing jobs for a specific slot
    pub fn get_workers_for_slot(&self, slot_id: &str) -> Vec<usize> {
        let mut workers = Vec::new();

        if let Some(job_ids) = self.slot_jobs.get(slot_id) {
            for worker_id in 0..self.worker_state.num_workers() {
                if let Some(job_id) = self.worker_state.get_worker_job(worker_id) {
                    if job_ids.contains(&job_id) {
                        workers.push(worker_id);
                    }
                }
            }
        }

        workers
    }

    /// Check if a slot is complete and queue next slot if using slot-first execution
    pub fn check_and_queue_next_slot(&mut self) -> bool {
        // Don't queue new slots if shutdown was requested
        if self.shutdown_requested {
            return false;
        }

        // Check if there are pending slots to queue
        if self.pending_slot_jobs.is_empty() {
            // Check if we need to queue teardown procedure jobs
            if !self.teardown_procedure_jobs.is_empty()
                && self.job_queue.is_empty()
                && self.worker_state.count_busy() == 0
            {
                crate::execution::cli_output::verbose(
                    "📋 All slots complete, enqueueing teardown procedure phases",
                );
                // Collect jobs first to avoid borrow issues
                let teardown_jobs: Vec<Job> = self.teardown_procedure_jobs.drain(..).collect();
                for job in teardown_jobs {
                    self.enqueue_job(job);
                }
                return true;
            }
            return false;
        }

        // Check if current slot work is complete (no jobs in queue and no busy workers)
        if self.job_queue.is_empty() && self.worker_state.count_busy() == 0 {
            // Queue the next slot's jobs
            if !self.pending_slot_jobs.is_empty() {
                let (slot_id, jobs) = self.pending_slot_jobs.remove(0);
                crate::execution::cli_output::verbose(format!(
                    "📦 Slot complete, starting next slot: {}",
                    slot_id
                ));
                for job in jobs {
                    self.enqueue_job(job);
                }
                return true;
            }
        }

        false
    }
}
