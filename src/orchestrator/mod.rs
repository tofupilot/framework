mod completion;
mod events;
mod execution;
mod initialization;
mod jobs;
mod plugs;
mod scheduling;
mod shutdown;
mod stats;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock, Semaphore};
use uuid::Uuid;

use crate::job::{JobResult, JobStatus, Outcome};
use crate::plugs::manager::ResourceManager;
use crate::procedure::schema::ProcedureDefinition;
use crate::state::OrchestratorState;
use crate::worker::Worker;

// Re-export ExecutionStrategy from schema instead of duplicating
pub use crate::procedure::schema::ExecutionStrategy;
// The partial-run dependency closure of one target: the CLI judges each
// `--phase` key with it, and `submit_procedure` filters the union.
pub use initialization::partial_main_phase_set;
pub use shutdown::{ShutdownMode, TEARDOWN_TIMEOUT};

#[derive(Debug)]
pub struct JobCompletionEvent {
    pub job_id: Uuid,
    pub result: Result<JobResult, String>,
    pub original_job: crate::job::Job,
    pub worker_id: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionStats {
    pub total_jobs: usize,
    pub completed_jobs: usize,
    pub failed_jobs: usize,
    pub running_jobs: usize,
    pub queued_jobs: usize,
    pub workers_busy: usize,
    pub workers_total: usize,
    pub run_outcome: Option<Outcome>,
    pub run_dir: Option<String>,
    pub run_id: Option<String>,
    pub slot_outcomes: HashMap<String, Outcome>,
    pub slot_run_ids: HashMap<String, String>,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobProgress {
    pub job_id: String,
    pub slot_id: Option<String>,
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: crate::procedure::schema::StageScope,
    pub status: JobStatus,
    pub worker_id: Option<usize>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timeout_ms: Option<u64>,
    pub outcome: Option<Outcome>,
    pub retry_count: usize,
    pub error: Option<String>,
}

// PlannedPhase and PlannedPlug are defined in crate::event_sink and re-exported from crate root
pub use crate::{PlannedPhase, PlannedPlug};

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionPlan {
    pub phases: Vec<PlannedPhase>,
    pub plugs_all: Vec<PlannedPlug>,
    pub plugs_each: Vec<PlannedPlug>,
    pub slots: Vec<String>,
    pub total_expected_jobs: u32,
    pub sequences: Vec<crate::event_sink::PlannedSequence>,
}

pub struct Orchestrator {
    pub state: Arc<RwLock<OrchestratorState>>,
    pub workers: Arc<RwLock<Vec<Worker>>>,
    pub resource_manager: Arc<RwLock<ResourceManager>>,
    pub(super) job_semaphore: Arc<Semaphore>,
    pub(super) procedure_dir: std::path::PathBuf,
    /// Directory the worker writes attachment bytes into (one file per
    /// `attach.file`/`attach.data`). The CLI supplies a per-run scratch
    /// dir it owns and deletes after upload; `None` leaves `attach.file`
    /// pointing at the source path and drops `attach.data` (no on-disk
    /// destination). Replaces the removed Studio-style ReportManager.
    pub(super) attachment_dir: Option<std::path::PathBuf>,
    /// Pre-resolved Python interpreter for workers + plug services. When
    /// set, downstream consumers skip the engine's `resolve_python`
    /// walk-up. CLI runs always set this; legacy callers leave it `None`.
    pub(super) python_path: Option<std::path::PathBuf>,
    pub(super) completion_tx: mpsc::Sender<JobCompletionEvent>,
    pub(super) completion_rx: Option<mpsc::Receiver<JobCompletionEvent>>,
    /// Cap on the shutdown teardown (`shutdown::TEARDOWN_TIMEOUT` unless
    /// a test shortens it).
    pub(super) teardown_timeout: std::time::Duration,
    pub(super) run_id: String,
    pub execution_id: String,
    /// The environment every worker and plug process this run spawns
    /// receives (respawns and teardown workers included): the run's
    /// credentials (`credentials`), then the procedure's `env:` expanded
    /// with the run's values.
    pub(super) procedure_env: crate::procedure_env::ProcedureEnv,
    /// The run's credentials as the host passed them (`with_procedure_env`).
    /// Empty when the CLI has no credentials.
    pub(super) credentials: crate::procedure_env::ProcedureEnv,
    /// The run's context, config and secrets (`with_run_values`). Resolved
    /// from the declarations' defaults at `initialize` when the host
    /// passed none.
    pub(super) run_values: Option<Arc<crate::procedure::RunValues>>,
    /// The secret values every event this run emits is masked with. Shared
    /// with the sink wrapper `set_event_sink` installs.
    pub(super) masks: Arc<std::sync::RwLock<Vec<String>>>,
    pub(super) procedure_definition: ProcedureDefinition,
    /// Execution-scope plug keys already created this run. `None` until
    /// the first job passes the creation gate — the scope-batch progress
    /// event pair must fire exactly once per run, even when the gate has
    /// nothing to create (submit_procedure reserves exactly one pair).
    pub(super) procedure_plugs_created: Arc<RwLock<Option<HashSet<String>>>>,
    /// Slot id → slot-scope plug keys already created for that slot.
    /// Entry presence marks the slot's batch event pair as fired (same
    /// accounting as above).
    pub(super) slot_plugs_created: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub(super) event_sink: Arc<dyn crate::EventSink>,
    pub(super) start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) initial_unit_infos: HashMap<String, crate::unit::UnitInfo>,
    /// Debug run: `Some(port)` → pool workers carry TP_DEBUG/TP_DEBUG_PORT
    /// and the Rust-side phase timeout wrapper is skipped so breakpoint
    /// pauses aren't killed.
    pub(super) debug_port: Option<u16>,
    /// Station-process-scoped owner of `scope: station` plugs. Set in
    /// station mode via `with_station_plug_host`; `None` (one-shot runs,
    /// Studio) degrades station plugs to execution scope.
    pub(super) station_plug_host: Option<Arc<crate::plugs::station_host::StationPlugHost>>,
    /// Whether this execution has already borrowed its station plugs
    /// from the host (mirrors `procedure_plugs_created`).
    pub(super) station_plugs_acquired: Arc<RwLock<bool>>,
    /// Forced phases: phases whose code does not
    /// run, their result set instead (TestStand's Force to Pass / Fail).
    /// Runtime only, never written to the procedure.
    pub(super) forced_phases: HashMap<String, ForcedResult>,
    /// Failure locals: a phase that raises reports its frame's
    /// variables. Off by default: `repr()` on a driver object can talk to
    /// the instrument.
    pub(super) capture_locals: bool,
}

/// The result a forced phase is given without running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedResult {
    Pass,
    Fail,
}

impl ForcedResult {
    /// The outcome it stands for, as the wire writes outcomes.
    pub fn outcome(self) -> &'static str {
        match self {
            ForcedResult::Pass => "PASS",
            ForcedResult::Fail => "FAIL",
        }
    }
}

impl Orchestrator {
    /// Shorten the shutdown teardown cap. Tests only; production keeps
    /// `shutdown::TEARDOWN_TIMEOUT`.
    pub fn set_teardown_timeout(&mut self, timeout: std::time::Duration) {
        self.teardown_timeout = timeout;
    }

    #[must_use = "orchestrator must be initialized before use"]
    pub fn new(
        worker_count: usize,
        procedure_dir: std::path::PathBuf,
        execution_id: String,
        run_id: String,
        procedure_definition: ProcedureDefinition,
    ) -> Self {
        Self::new_with_python(
            worker_count,
            procedure_dir,
            None,
            None,
            execution_id,
            run_id,
            procedure_definition,
            None,
        )
    }

    /// Construct an orchestrator with a pre-resolved Python interpreter.
    /// CLI runs use this so workers + plug services skip the engine's
    /// `resolve_python` walk-up. The path comes from the deterministic
    /// `<package_dir>/venv/python` resolver in CLI's `prepare_run`.
    #[must_use = "orchestrator must be initialized before use"]
    // One argument per thing a run is made of; every caller states each.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_python(
        worker_count: usize,
        procedure_dir: std::path::PathBuf,
        python_path: Option<std::path::PathBuf>,
        attachment_dir: Option<std::path::PathBuf>,
        execution_id: String,
        run_id: String,
        procedure_definition: ProcedureDefinition,
        debug_port: Option<u16>,
    ) -> Self {
        let mut workers = Vec::new();
        for i in 0..worker_count {
            workers.push(
                Worker::new_with_python(i, procedure_dir.clone(), python_path.clone())
                    .with_debug_port(debug_port),
            );
        }

        let num_workers = workers.len();

        // Bounded so a lagging consumer (the schedule loop holding
        // locks while recursing) doesn't pile up `JobCompletionEvent`s
        // — each carries the full `Job` plus result (logs,
        // measurements). Capacity covers a couple of fully-saturated
        // worker pools plus headroom; senders `.send().await` so
        // backpressure flows back into the worker task naturally.
        let capacity = num_workers.max(1) * 8 + 16;
        let (tx, rx) = mpsc::channel(capacity);

        Self {
            state: Arc::new(RwLock::new(OrchestratorState::new(num_workers))),
            workers: Arc::new(RwLock::new(workers)),
            resource_manager: Arc::new(RwLock::new(ResourceManager::new_with_python(
                procedure_dir.clone(),
                python_path.clone(),
            ))),
            job_semaphore: Arc::new(Semaphore::new(
                crate::constants::limits::MAX_CONCURRENT_JOBS,
            )),
            procedure_dir,
            attachment_dir,
            python_path,
            completion_tx: tx,
            completion_rx: Some(rx),
            teardown_timeout: shutdown::TEARDOWN_TIMEOUT,
            run_id,
            execution_id,
            procedure_definition,
            procedure_plugs_created: Arc::new(RwLock::new(None)),
            slot_plugs_created: Arc::new(RwLock::new(HashMap::new())),
            event_sink: Arc::new(crate::NullSink),
            start_time: None,
            end_time: None,
            initial_unit_infos: HashMap::new(),
            debug_port,
            station_plug_host: None,
            station_plugs_acquired: Arc::new(RwLock::new(false)),
            forced_phases: HashMap::new(),
            capture_locals: false,
            procedure_env: Vec::new(),
            credentials: Vec::new(),
            run_values: None,
            masks: Arc::default(),
        }
    }

    /// Attach the station-process-scoped plug host. Builder-style so the
    /// constructor signature (shared with Studio) stays stable.
    #[must_use]
    pub fn with_station_plug_host(
        mut self,
        host: Option<Arc<crate::plugs::station_host::StationPlugHost>>,
    ) -> Self {
        self.station_plug_host = host;
        self
    }

    /// The procedure's `dashboard:` credentials. Applied to the pool
    /// workers and the plug service manager now, and to every worker
    /// spawned later (replacement, teardown) through the stored copy.
    /// Builder-style, called before the orchestrator runs: the locks
    /// are uncontended at that point, so `try_*` cannot fail.
    #[must_use]
    pub fn with_procedure_env(mut self, env: crate::procedure_env::ProcedureEnv) -> Self {
        self.credentials = env;
        self.apply_process_env();
        self
    }

    /// The run's context and values, as `procedure::resolve_values`
    /// returned them: what `{run.*}`, `{config.X}` and `{secrets.X}` expand
    /// to, what phases read as `run.*`, `config` and `secrets`, and the
    /// secret values every event is masked with. Builder-style, before the
    /// orchestrator runs; without it `initialize` resolves the
    /// declarations' defaults and refuses a procedure with a required
    /// value.
    #[must_use]
    pub fn with_run_values(mut self, values: crate::procedure::RunValues) -> Self {
        self.set_run_values(values);
        self
    }

    pub(super) fn set_run_values(&mut self, values: crate::procedure::RunValues) {
        *self.masks.write().unwrap_or_else(|e| e.into_inner()) = values.masked_values();
        self.run_values = Some(Arc::new(values));
        self.apply_process_env();
    }

    /// The credentials, then the procedure's `env:` expanded with the run's
    /// values (no slot: these processes serve every slot), set on the pool
    /// workers and the plug service manager now and kept for every process
    /// spawned later. The locks are uncontended before the run, so `try_*`
    /// cannot fail.
    fn apply_process_env(&mut self) {
        let mut env = self.credentials.clone();
        if let Some(values) = &self.run_values {
            env.extend(values.expand_env(&self.procedure_definition.env, None));
        }
        if let Ok(mut workers) = self.workers.try_write() {
            for worker in workers.iter_mut() {
                worker.set_procedure_env(env.clone());
            }
        }
        if let Ok(rm) = self.resource_manager.try_read() {
            rm.set_procedure_env(env.clone());
        }
        self.procedure_env = env;
    }

    /// The run's values; the declarations' defaults until `initialize`
    /// resolved them or the host passed its own.
    pub fn run_values(&self) -> Option<&crate::procedure::RunValues> {
        self.run_values.as_deref()
    }

    pub fn set_initial_unit_infos(&mut self, unit_infos: HashMap<String, crate::unit::UnitInfo>) {
        self.initial_unit_infos = unit_infos;
    }

    /// Phases given a result without running, by key.
    pub fn set_forced_phases(&mut self, forced: HashMap<String, ForcedResult>) {
        self.forced_phases = forced;
    }

    /// Whether a phase that raises reports its frame's variables.
    pub fn set_capture_locals(&mut self, capture: bool) {
        self.capture_locals = capture;
    }

    /// Whether the job has attempts left that would change anything: a
    /// forced result is the same on every attempt.
    pub(super) fn may_retry(&self, job: &crate::job::Job) -> bool {
        job.can_retry() && self.forced_result(&job.phase_key).is_none()
    }

    /// The result this phase was forced to, when it was: the
    /// phase does not run, needs no plug and is never retried.
    pub(super) fn forced_result(&self, phase_key: &str) -> Option<ForcedResult> {
        self.forced_phases.get(phase_key).copied()
    }

    /// Every event of the run reaches `sink` with the run's secret values
    /// masked (`events::MaskingSink`).
    pub fn set_event_sink(&mut self, sink: Arc<dyn crate::EventSink>) {
        self.event_sink = Arc::new(crate::event_sink::MaskingSink::new(
            sink,
            self.masks.clone(),
        ));
    }
}
