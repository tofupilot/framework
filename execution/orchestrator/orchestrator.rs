use serde::Serialize;
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri_specta::Event;
use tokio::sync::{mpsc, RwLock, Semaphore};
use uuid::Uuid;

use crate::execution::constants::limits;
use crate::execution::job::{JobResult, JobStatus, Outcome, PhaseResult};
use crate::execution::reports::ReportManager;
use crate::execution::state::OrchestratorState;
use crate::execution::worker::Worker;
use crate::plugs::manager::ResourceManager;
use crate::procedure::schema::ProcedureDefinition;

// Re-export ExecutionStrategy from schema instead of duplicating
pub use crate::procedure::schema::ExecutionStrategy;

#[derive(Debug)]
pub struct JobCompletionEvent {
    pub job_id: Uuid,
    pub result: Result<JobResult, String>,
    pub original_job: crate::execution::job::Job,
    pub worker_id: usize,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct ExecutionStats {
    #[specta(type = u32)]
    pub total_jobs: usize,
    #[specta(type = u32)]
    pub completed_jobs: usize,
    #[specta(type = u32)]
    pub failed_jobs: usize,
    #[specta(type = u32)]
    pub running_jobs: usize,
    #[specta(type = u32)]
    pub queued_jobs: usize,
    #[specta(type = u32)]
    pub workers_busy: usize,
    #[specta(type = u32)]
    pub workers_total: usize,
    pub run_outcome: Option<Outcome>,
    pub run_dir: Option<String>,
    pub run_id: Option<String>,
    pub slot_outcomes: HashMap<String, Outcome>,
    pub slot_run_ids: HashMap<String, String>,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct JobProgress {
    pub job_id: String,
    pub slot_id: Option<String>,
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: crate::procedure::schema::StageScope,
    pub status: JobStatus,
    #[specta(type = Option<u32>)]
    pub worker_id: Option<usize>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[specta(type = Option<f64>)]
    pub timeout_ms: Option<u64>,
    pub outcome: Option<Outcome>,
    #[specta(type = u32)]
    pub retry_count: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct JobCompleteEvent {
    pub job_id: String,
    pub slot_id: Option<String>,
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: crate::procedure::schema::StageScope,
    pub outcome: Outcome,
    pub action: String,
    pub next_action: Option<String>,
    pub measurements: Vec<crate::measurements::Measurement>,
    pub attachments: Vec<String>,
    pub logs: Vec<crate::execution::log::LogEntry>,
    pub resource_metrics: Option<crate::execution::job::ResourceMetrics>,
    #[specta(type = u32)]
    pub retry_count: usize,
    #[specta(type = u32)]
    pub retry_limit: usize,
    pub started_at: String,
    pub completed_at: String,
    #[specta(type = f64)]
    pub duration_ms: u64,
    #[specta(type = u32)]
    pub worker_id: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct PlannedPhase {
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: crate::procedure::schema::StageScope,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct PlannedPlug {
    pub plug_key: String,
    pub plug_name: String,
    pub scope: String, // "all" or "each"
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct ExecutionPlan {
    pub phases: Vec<PlannedPhase>,
    pub plugs_all: Vec<PlannedPlug>,
    pub plugs_each: Vec<PlannedPlug>,
    pub slots: Vec<String>,
    pub total_expected_jobs: u32,
}

// Specta event wrappers for type-safe event emission
#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct ExecutionCompleteEvent(pub ExecutionStats);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct ExecutionPlanEvent(pub ExecutionPlan);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct ExecutionStatsEvent(pub ExecutionStats);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct JobCompleteEventWrapper(pub JobCompleteEvent);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct JobProgressEvent(pub JobProgress);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct OrchestratorExecutionProgressEvent {
    pub execution_id: String,
    #[specta(skip)]
    pub stats: serde_json::Value,
}

#[derive(Debug)]
pub struct Orchestrator {
    pub state: Arc<RwLock<OrchestratorState>>,
    pub workers: Arc<RwLock<Vec<Worker>>>,
    pub resource_manager: Arc<RwLock<ResourceManager>>,
    pub(super) report_managers: Arc<RwLock<HashMap<String, ReportManager>>>,
    pub(super) job_semaphore: Arc<Semaphore>,
    pub(super) procedure_dir: std::path::PathBuf,
    pub(super) completion_tx: mpsc::UnboundedSender<JobCompletionEvent>,
    pub(super) completion_rx: Option<mpsc::UnboundedReceiver<JobCompletionEvent>>,
    pub(super) run_id: String,
    pub execution_id: String,
    pub(super) procedure_definition: ProcedureDefinition,
    pub(super) procedure_plugs_created: Arc<RwLock<bool>>,
    pub(super) slot_plugs_created: Arc<RwLock<HashSet<String>>>,
    pub(super) app_handle: Option<tauri::AppHandle>,
    pub(super) start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) initial_unit_infos: HashMap<String, crate::execution::types::UnitInfo>,
    /// Debug run: pool workers get TP_DEBUG and the Rust-side phase
    /// timeout wrapper is skipped so breakpoint pauses aren't killed.
    pub(super) debug: bool,
}

pub struct JobStatusResolver;

impl JobStatusResolver {
    pub fn resolve(
        job_result: &JobResult,
        is_retry_limit_exceeded: bool,
    ) -> (String, String, Option<String>) {
        if is_retry_limit_exceeded {
            return (
                "failed".to_string(),
                "error".to_string(),
                Some(format!(
                    "Phase exceeded retry limit ({} retries)",
                    limits::DEFAULT_RETRY_LIMIT
                )),
            );
        }

        // Check execution errors first
        if let Some(ref e) = job_result.error {
            return ("failed".to_string(), "error".to_string(), Some(e.clone()));
        }

        if let Some(secs) = job_result.timeout_secs {
            return (
                "failed".to_string(),
                "timeout".to_string(),
                Some(format!("Phase timed out after {} seconds", secs)),
            );
        }

        // Then check phase action
        match &job_result.phase_result {
            PhaseResult::Continue => ("completed".to_string(), "pass".to_string(), None),
            PhaseResult::Retry => ("completed".to_string(), "retry".to_string(), None),
            PhaseResult::Skip => ("completed".to_string(), "skip".to_string(), None),
            PhaseResult::Stop => ("completed".to_string(), "stop".to_string(), None),
            PhaseResult::Fail => ("completed".to_string(), "fail".to_string(), None),
        }
    }
}

impl Orchestrator {
    #[must_use = "orchestrator must be initialized before use"]
    pub fn new(
        worker_count: usize,
        procedure_dir: std::path::PathBuf,
        execution_id: String,
        run_id: String,
        procedure_definition: ProcedureDefinition,
        debug: bool,
    ) -> Self {
        let mut workers = Vec::new();
        for i in 0..worker_count {
            workers.push(Worker::new_with_debug(i, procedure_dir.clone(), debug));
        }

        let (tx, rx) = mpsc::unbounded_channel();

        let num_workers = workers.len();

        Self {
            state: Arc::new(RwLock::new(OrchestratorState::new(num_workers))),
            workers: Arc::new(RwLock::new(workers)),
            resource_manager: Arc::new(RwLock::new(ResourceManager::new(procedure_dir.clone()))),
            report_managers: Arc::new(RwLock::new(HashMap::new())),
            job_semaphore: Arc::new(Semaphore::new(
                crate::execution::constants::limits::MAX_CONCURRENT_JOBS,
            )),
            procedure_dir,
            completion_tx: tx,
            completion_rx: Some(rx),
            run_id,
            execution_id,
            procedure_definition,
            procedure_plugs_created: Arc::new(RwLock::new(false)),
            slot_plugs_created: Arc::new(RwLock::new(HashSet::new())),
            app_handle: None,
            start_time: None,
            end_time: None,
            initial_unit_infos: HashMap::new(),
            debug,
        }
    }

    pub fn set_initial_unit_infos(
        &mut self,
        unit_infos: HashMap<String, crate::execution::types::UnitInfo>,
    ) {
        self.initial_unit_infos = unit_infos;
    }

    pub fn set_app_handle(&mut self, app_handle: tauri::AppHandle) {
        self.app_handle = Some(app_handle);
    }
}
