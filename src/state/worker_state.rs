use std::collections::HashMap;
use uuid::Uuid;

/// Tracks worker state and job assignments
/// Single source of truth for worker availability
#[derive(Debug)]
pub struct WorkerStateTracker {
    /// Maps worker_id to currently assigned job_id (if any)
    worker_assignments: HashMap<usize, Option<Uuid>>,
    /// Reverse mapping: job_id to worker_id
    job_to_worker: HashMap<Uuid, usize>,
}

impl WorkerStateTracker {
    pub fn new(num_workers: usize) -> Self {
        let mut worker_assignments = HashMap::new();
        for i in 0..num_workers {
            worker_assignments.insert(i, None);
        }

        Self {
            worker_assignments,
            job_to_worker: HashMap::new(),
        }
    }

    /// Check if a worker is idle
    pub fn is_worker_idle(&self, worker_id: usize) -> bool {
        self.worker_assignments
            .get(&worker_id)
            .map(|assignment| assignment.is_none())
            .unwrap_or(false)
    }

    /// Get an idle worker ID
    pub fn get_idle_worker(&self) -> Option<usize> {
        self.worker_assignments
            .iter()
            .find(|(_, assignment)| assignment.is_none())
            .map(|(id, _)| *id)
    }

    /// Assign a job to a worker
    pub fn assign_job(&mut self, worker_id: usize, job_id: Uuid) -> Result<(), String> {
        match self.worker_assignments.get_mut(&worker_id) {
            Some(assignment) => {
                if assignment.is_some() {
                    return Err(format!("Worker {} is already busy", worker_id));
                }
                *assignment = Some(job_id);
                self.job_to_worker.insert(job_id, worker_id);
                Ok(())
            }
            None => Err(format!("Worker {} not found", worker_id)),
        }
    }

    /// Release a worker by job ID
    pub fn release_by_job(&mut self, job_id: &Uuid) -> Option<usize> {
        if let Some(worker_id) = self.job_to_worker.remove(job_id) {
            if let Some(assignment) = self.worker_assignments.get_mut(&worker_id) {
                *assignment = None;
            }
            return Some(worker_id);
        }
        None
    }

    /// Count busy workers
    pub fn count_busy(&self) -> usize {
        self.worker_assignments
            .values()
            .filter(|assignment| assignment.is_some())
            .count()
    }

    /// Get the job assigned to a worker
    pub fn get_worker_job(&self, worker_id: usize) -> Option<Uuid> {
        self.worker_assignments
            .get(&worker_id)
            .and_then(|assignment| *assignment)
    }

    /// Get the total number of workers
    pub fn num_workers(&self) -> usize {
        self.worker_assignments.len()
    }
}
