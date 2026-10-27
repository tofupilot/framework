use crate::constants::limits;
use crate::log::LogEntry;
use crate::procedure::schema::{MeasurementSpec, StageScope};
use crate::ui::{PythonPhaseResult, UiConfig};
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

// Completion outcomes (no lifecycle states)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    Error,
    Timeout,
    Stop,
    Skip,
    Retry,
}

// Lifecycle states (no completion outcomes)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Stopping,
    Completed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeType {
    Native, // Rust built-in phases
    #[default]
    Python, // External Python runtime (default for compatibility)
    Shell,  // Shell commands
}

/// PhaseResult represents what the phase function returns (user's decision)
/// This is the return value that controls execution flow
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PhaseResult {
    #[default]
    Continue, // Proceed to next phase (default)
    Retry, // Retry this phase (with limit)
    Skip,  // Skip to next phase (log but ignore measurements)
    Stop,  // Stop test execution (intentional failure)
    Fail,  // Mark as failed (when.fail determines next action)
}

impl PhaseResult {
    pub fn from_python_result(value: &PythonPhaseResult) -> Result<Self, String> {
        match value {
            PythonPhaseResult::Bool(b) => {
                if *b {
                    Ok(PhaseResult::Continue) // True = pass and continue
                } else {
                    Ok(PhaseResult::Fail) // False = fail
                }
            }
            PythonPhaseResult::String(s) => match s.to_uppercase().as_str() {
                "CONTINUE" => Ok(PhaseResult::Continue),
                "RETRY" => Ok(PhaseResult::Retry),
                "SKIP" => Ok(PhaseResult::Skip),
                "STOP" => Ok(PhaseResult::Stop),
                "FAIL" => Ok(PhaseResult::Fail),
                _ => Err(format!("Unknown phase result: {}", s)),
            },
            PythonPhaseResult::Null => Ok(PhaseResult::Continue), // None in Python = Continue
        }
    }
}

impl Outcome {
    /// Temporary placeholder used by workers before completion handler computes real outcome
    pub const PENDING_COMPLETION: Outcome = Outcome::Pass;

    /// Compute outcome from result and execution context
    pub fn from_execution(
        result: &PhaseResult,
        timeout_secs: Option<u64>,
        error: Option<&String>,
        measurements_pass: bool,
        shutdown_requested: bool,
        can_retry: bool,
    ) -> Self {
        // Terminal errors → ERROR
        if error.is_some() {
            return Outcome::Error;
        }

        // Timeout → TIMEOUT
        if timeout_secs.is_some() {
            return Outcome::Timeout;
        }

        // Shutdown → STOP (takes precedence over retry)
        if shutdown_requested {
            return Outcome::Stop;
        }

        // Retry (only if authorized) → RETRY or FAIL
        if matches!(result, PhaseResult::Retry) {
            return if can_retry {
                Outcome::Retry
            } else {
                Outcome::Fail
            };
        }

        // Measurement failures → FAIL. Outranks the phase's own
        // `phase.stop()`: a unit that measured out of limits is a failure
        // whatever the phase decided to do next, and must count as one.
        // The stop itself is still honoured by `determine_next_action`.
        if !measurements_pass {
            return Outcome::Fail;
        }

        // Explicit stop with passing measurements → STOP
        if matches!(result, PhaseResult::Stop) {
            return Outcome::Stop;
        }

        // Skip → SKIP
        if matches!(result, PhaseResult::Skip) {
            return Outcome::Skip;
        }

        // Explicit failures → FAIL
        if matches!(result, PhaseResult::Fail) {
            return Outcome::Fail;
        }

        // Default → PASS
        Outcome::Pass
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub slot_id: Option<String>, // None = shared across all slots, Some(id) = specific slot
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: StageScope,
    pub module: String,   // Python module path (e.g., "phases.test")
    pub function: String, // Python function name
    pub depends_on: HashSet<Uuid>,
    pub required_plugs: Vec<String>,
    pub ui_config: UiConfig,
    pub status: JobStatus,
    pub retry_count: usize,
    pub retry_limit: usize,
    pub retry_delay_ms: Option<u64>,
    pub timeout_ms: Option<u64>, // Phase timeout in milliseconds (None = no timeout)

    // New fields for multi-runtime support
    #[serde(default)]
    pub runtime_type: RuntimeType,
    pub command: Option<String>, // For shell phases: command to execute
    pub shell_type: Option<String>, // For shell phases: which shell (bash, powershell, etc.)
    pub working_directory: Option<String>, // For shell phases: execution directory
    pub procedure_dir: Option<String>, // Procedure directory for resolving relative paths and default working dir
    pub phase_measurements: Vec<MeasurementSpec>, // YAML measurement definitions
    /// Initial unit info (serial, part number, sub-units, etc.) for Python access
    pub initial_unit_info: Option<crate::unit::UnitInfo>,
    /// Completed phase measurements keyed by phase key, values are JSON strings
    #[serde(default)]
    pub phase_results: HashMap<String, String>,
    /// The job UUID that dependent phases are waiting on.
    /// For original jobs: same as `id`. For retry jobs: the original job's `dependency_id`.
    /// When this job completes, `dependency_id` is inserted into `completed_jobs` to unblock dependents.
    #[serde(default = "Uuid::new_v4")]
    pub dependency_id: Uuid,
    /// Instance keys of the sequence calls the phase came through, root
    /// first; empty for a phase of the procedure itself. The scope a
    /// sequence cancellation and settlement work on (prefix match).
    #[serde(default)]
    pub sequence_path: Vec<String>,
    /// Parallel to `sequence_path`: the stage of the next level (or the
    /// phase itself at the end) within each enclosing sequence.
    #[serde(default)]
    pub sequence_stages: Vec<crate::procedure::schema::PhaseStage>,
    /// The innermost sequence's resolved inputs, handed to Python as the
    /// `inputs` injectable.
    #[serde(default)]
    pub inputs: std::collections::BTreeMap<String, serde_json::Value>,
    /// Plug parameter name in the phase to the procedure plug key bound to
    /// it, for phases of a sequence whose requirement was bound under
    /// another name.
    #[serde(default)]
    pub plug_aliases: std::collections::BTreeMap<String, String>,
    /// The phase's `with:`, `env:`, config, secrets and run context,
    /// resolved for this job's slot when the job is created. Never
    /// serialized: it may hold secrets.
    #[serde(skip)]
    pub run: JobRunValues,
    /// Report the frame's variables if the phase raises. Off
    /// by default: `repr()` on a driver object can talk to the instrument.
    #[serde(default)]
    pub capture_locals: bool,
    /// Plugs of the run that did not open, with why: a parameter naming
    /// one makes the phase SKIP (`Not run: plug … did not open`) instead
    /// of a binding TypeError. Set per dispatch, never serialized.
    #[serde(skip)]
    pub unavailable_plugs: std::collections::BTreeMap<String, String>,
}

/// What a job receives from the run's values (see `Job::run`).
#[derive(Clone, Default, PartialEq)]
pub struct JobRunValues {
    /// The phase's `with:`, markers expanded: keyword arguments.
    pub with: std::collections::BTreeMap<String, serde_json::Value>,
    /// The phase's own `env:`, expanded.
    pub env: Vec<(String, String)>,
    /// The `run` object of the phase: station, procedure, deployment and
    /// this job's slot (see `RunContext::to_json`).
    pub context: serde_json::Value,
    /// The config the phase reads, by key.
    pub config: std::collections::BTreeMap<String, serde_json::Value>,
    /// The secrets the phase reads, by key.
    pub secrets: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Keys only: the values may be secrets, and jobs are logged.
impl std::fmt::Debug for JobRunValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRunValues")
            .field("with", &self.with.keys().collect::<Vec<_>>())
            .field("env", &self.env.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .field("config", &self.config.keys().collect::<Vec<_>>())
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Job {
    /// The phase sits under the sequence call at `path` (or is one of its
    /// phases when `path` is its own).
    pub fn in_sequence_scope(&self, path: &[String]) -> bool {
        path_has_prefix(&self.sequence_path, path)
    }

    /// Teardown work at any level: a phase of the procedure's teardown
    /// stages, or a teardown phase of some enclosing sequence (which sits
    /// in its caller's stage, so only `sequence_stages` says so). The work
    /// that runs BECAUSE of a stop, and so keeps its own outcome and its
    /// retry budget under one. `JobInfo::is_teardown` is the same rule.
    pub fn is_teardown_work(&self) -> bool {
        is_teardown_work(self.stage_scope, &self.sequence_stages)
    }
}

/// The one rule behind `Job::is_teardown_work` and `JobInfo::is_teardown`:
/// the procedure's own teardown stages, or a teardown stage of any
/// enclosing sequence.
pub fn is_teardown_work(
    stage_scope: StageScope,
    sequence_stages: &[crate::procedure::schema::PhaseStage],
) -> bool {
    matches!(
        stage_scope,
        StageScope::TeardownEach | StageScope::TeardownAll
    ) || sequence_stages.contains(&crate::procedure::schema::PhaseStage::Teardown)
}

/// `path` is `prefix` or sits under it: sequence instance keys, outermost
/// first. The one prefix test behind every "is this inside that call".
pub fn path_has_prefix(path: &[String], prefix: &[String]) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == prefix[..]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceMetrics {
    pub cpu_usage_percent: f32,
    pub cpu_time_seconds: f64,
    pub memory_peak_mb: f64,
    pub memory_avg_mb: f64,
    pub process_count: usize, // Main + children
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub phase_result: PhaseResult,
    pub phase_outcome: Outcome, // Always computed - REQUIRED
    pub next_action: Option<crate::procedure::schema::PhaseNextAction>, // Computed in completion handler after applying then config
    pub timeout_secs: Option<u64>,                                      // Set if phase timed out
    /// The timeout that cut it, exactly (`timeout_secs` rounds it): its
    /// message and its duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub error: Option<String>,  // Set if execution error occurred
    pub exit_code: Option<i32>, // Exit code from sys.exit() if called
    pub measurements: Vec<crate::measurements::Measurement>,
    pub logs: Vec<LogEntry>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    /// Always `None` since the per-phase sampler was removed (its numbers
    /// were wrong); kept so the serialized result keeps its shape.
    pub resource_metrics: Option<ResourceMetrics>,
    pub unit: Option<crate::unit::UnitInfo>,
    /// The unit info that was passed to this job before execution.
    /// Used to distinguish intentional changes from inherited values during merge.
    pub input_unit_info: Option<crate::unit::UnitInfo>,
    pub retry_count: usize,
    /// Run-level metadata set by the phase via `run.metadata[...]`.
    #[serde(default)]
    pub run_metadata: std::collections::HashMap<String, serde_json::Value>,
    /// Unit-level metadata set by the phase via `unit.metadata[...]`.
    #[serde(default)]
    pub unit_metadata: std::collections::HashMap<String, serde_json::Value>,
    /// Variables of the phase's own frame where it raised, `repr`s
    /// truncated and secret-looking names masked. For the people and
    /// agents reading the failure locally: never part of the uploaded run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_locals: Vec<FrameLocal>,
    /// A stop closed the phase's operator prompt or `ui.wait()` under it,
    /// unanswered: its error then is the cut's, not its own (completion
    /// handler), and its STOP is no stop request. A prompt nobody could
    /// answer (no surface, stdin closed, `--ui-timeout`) is not one.
    #[serde(default)]
    pub prompt_closed: bool,
}

/// One variable of a failing phase's frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLocal {
    pub name: String,
    pub value: String,
}

/// How the log line of a forced phase begins (`Forced to FAIL: not run`):
/// what tells a reader of the run's events that the phase never ran.
pub const FORCED_LOG_PREFIX: &str = "Forced to";

impl JobResult {
    /// Whether this phase ASKED to stop: an explicit `phase.stop()`, or
    /// `then: {…: stop}` on a phase that passed or skipped. A phase whose
    /// own outcome is Stop got it from the raised flag (it was in flight,
    /// or never started), and so does one whose prompt a stop closed
    /// (`prompt_closed`); `next_action` is Stop for those too, so it is
    /// not a request. A `phase.stop()` after a failed measurement is a
    /// FAIL phase: the unit is defective, the stop only ended the run
    /// early, so it counts as the failure it is.
    pub fn requested_stop(&self) -> bool {
        if self.prompt_closed {
            return false;
        }
        (matches!(self.phase_result, PhaseResult::Stop)
            && !matches!(self.phase_outcome, Outcome::Fail))
            || (self.next_action == Some(crate::procedure::schema::PhaseNextAction::Stop)
                && matches!(self.phase_outcome, Outcome::Pass | Outcome::Skip))
    }

    /// The result of a forced phase: no measurement, no
    /// error, and a log line saying why, so the run reads as what it is.
    pub fn forced(forced: crate::orchestrator::ForcedResult) -> Self {
        let now = chrono::Utc::now();
        let (phase_result, phase_outcome) = match forced {
            crate::orchestrator::ForcedResult::Pass => (PhaseResult::Continue, Outcome::Pass),
            crate::orchestrator::ForcedResult::Fail => (PhaseResult::Fail, Outcome::Fail),
        };
        Self {
            phase_result,
            phase_outcome,
            logs: vec![LogEntry {
                timestamp: now.to_rfc3339(),
                level: "WARNING".to_string(),
                // Not the "Not run:" of a skipped phase: this one is
                // recorded with a result.
                message: format!("{FORCED_LOG_PREFIX} {}: not run", forced.outcome()),
                file: None,
                line: None,
            }],
            error: None,
            ..Self::new_error(String::new())
        }
    }

    pub fn new_error(error_msg: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Continue,
            phase_outcome: Outcome::Error,
            next_action: None, // Will be computed in completion handler
            timeout_secs: None,
            timeout_ms: None,
            error: Some(error_msg),
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            input_unit_info: None,
            retry_count: 0,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        }
    }

    /// A phase the operator stopped while it was running. Not an error:
    /// nothing was observed on the DUT, so `error` stays empty and the
    /// outcome is `Stop`, matching the `JobProgress` the kill path emits
    /// for the same phase. Recording it as `new_error` once made every
    /// force kill with a phase in flight aggregate to ERROR (TP-957). The
    /// reason is kept as a log line on the result. Today this result never
    /// reaches the upload (the CLI drops the `JobProgress{Completed}` the
    /// shutdown path emits), so the line is for whoever reads it next.
    pub fn new_interrupted(reason: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Continue,
            phase_outcome: Outcome::Stop,
            next_action: None,
            timeout_secs: None,
            timeout_ms: None,
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![LogEntry {
                timestamp: now.to_rfc3339(),
                level: "WARNING".to_string(),
                message: reason,
                file: None,
                line: None,
            }],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            input_unit_info: None,
            retry_count: 0,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        }
    }

    pub fn new_timeout(secs: u64) -> Self {
        Self::new_timeout_ms(secs.saturating_mul(1000))
    }

    /// A phase cut by its `timeout` of `ms`: it ran the whole budget, so
    /// `started_at` is back-dated by exactly that.
    pub fn new_timeout_ms(ms: u64) -> Self {
        let now = chrono::Utc::now();
        let started = now - chrono::Duration::milliseconds(ms as i64);
        Self {
            phase_result: PhaseResult::Continue,
            phase_outcome: Outcome::Timeout,
            next_action: None, // Will be computed in completion handler
            timeout_secs: Some(((ms as f64) / 1000.0).round().max(1.0) as u64),
            timeout_ms: Some(ms),
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: started,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            input_unit_info: None,
            retry_count: 0,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        }
    }

    pub fn new_skip() -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Skip,
            phase_outcome: Outcome::Skip,
            next_action: None, // Will be computed in completion handler
            timeout_secs: None,
            timeout_ms: None,
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            input_unit_info: None,
            retry_count: 0,
            run_metadata: Default::default(),
            unit_metadata: Default::default(),
            failure_locals: Vec::new(),
            prompt_closed: false,
        }
    }

    /// Check if this result represents a failure (for retry logic)
    /// The phase failed: raised, timed out, called `phase.fail()`, or a
    /// measurement missed its limits. An explicit `phase.stop()` with
    /// measurements in limits is a stop, not a failure.
    pub fn is_failure(&self) -> bool {
        self.error.is_some()
            || self.timeout_secs.is_some()
            || matches!(self.phase_result, PhaseResult::Fail)
            || matches!(self.phase_outcome, Outcome::Fail)
    }

    /// Check if this result should stop test execution
    /// This checks the final next_action after applying then overrides, not the raw phase_result
    pub fn should_stop_test(&self) -> bool {
        // If next_action is computed, use it (respects then overrides)
        if let Some(next_action) = &self.next_action {
            matches!(next_action, crate::procedure::schema::PhaseNextAction::Stop)
        } else {
            // Fallback: if next_action not yet computed, check raw conditions
            // (this handles cases where we check before completion handler runs)
            self.error.is_some()
                || self.timeout_secs.is_some()
                || matches!(self.phase_result, PhaseResult::Stop)
        }
    }
}

/// What every job is made of, whatever runs it (Python, a shell
/// command, the engine itself for a prompt-only phase).
pub struct JobSpec {
    pub slot_id: Option<String>,
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: StageScope,
    /// Phase keys, resolved to job ids through the caller's job map.
    pub depends_on: Vec<String>,
    pub required_plugs: Vec<String>,
    pub ui_config: UiConfig,
    pub timeout_ms: Option<u64>,
    pub retry_limit: Option<usize>,
    pub retry_delay_ms: Option<u64>,
    pub phase_measurements: Vec<MeasurementSpec>,
}

impl JobSpec {
    /// A spec with nothing but its key and stage: no dependency, plug,
    /// prompt, timeout, retry or measurement.
    pub fn bare(
        slot_id: Option<String>,
        phase_key: &str,
        phase_name: &str,
        stage_scope: StageScope,
    ) -> Self {
        Self {
            slot_id,
            phase_key: phase_key.to_string(),
            phase_name: phase_name.to_string(),
            stage_scope,
            depends_on: Vec::new(),
            required_plugs: Vec::new(),
            ui_config: UiConfig::default(),
            timeout_ms: None,
            retry_limit: None,
            retry_delay_ms: None,
            phase_measurements: Vec::new(),
        }
    }
}

/// What a shell phase runs.
pub struct ShellSpec {
    pub command: String,
    pub shell_type: Option<String>,
    pub working_directory: Option<String>,
    pub procedure_dir: Option<String>,
}

impl Job {
    /// A Python phase's job: `function` of `module`.
    pub fn new(
        spec: JobSpec,
        module: String,
        function: String,
        job_map: &HashMap<String, Uuid>,
    ) -> Self {
        let JobSpec {
            slot_id,
            phase_key,
            phase_name,
            stage_scope,
            depends_on,
            required_plugs,
            ui_config,
            timeout_ms,
            retry_limit,
            retry_delay_ms,
            phase_measurements,
        } = spec;
        let id = Uuid::new_v4();

        // Convert phase dependency IDs to job UUIDs. Skipped phases
        // (`should_skip()` true at submit time) and typos in
        // `depends_on:` are silently absent from `job_map`. Log a
        // warning so a phase doesn't suddenly run earlier than the
        // author expects with no signal — the cycle check in
        // `procedure/loader.rs` only sees declared phases, not
        // missing references.
        let dependencies: HashSet<Uuid> = depends_on
            .iter()
            .filter_map(|dep| {
                let found = resolve_dependency(job_map, slot_id.as_deref(), dep);
                match found {
                    Some(id) => Some(id),
                    None => {
                        // Not created yet (a phase further down the file)
                        // or skipped: the orchestrator resolves again once
                        // every job exists and warns about what is left.
                        log::debug!(
                            "Phase '{}' (slot {:?}) depends_on '{}': no job yet, resolved after creation.",
                            phase_name, slot_id, dep
                        );
                        None
                    }
                }
            })
            .collect();

        Self {
            id,
            slot_id,
            phase_key,
            phase_name,
            stage_scope,
            module,
            function,
            depends_on: dependencies,
            required_plugs,
            ui_config,
            status: JobStatus::Pending,
            retry_count: 0,
            retry_limit: retry_limit.unwrap_or(limits::DEFAULT_RETRY_LIMIT),
            retry_delay_ms,
            timeout_ms,
            runtime_type: RuntimeType::Python, // Default for backward compatibility
            command: None,
            shell_type: None,
            working_directory: None,
            procedure_dir: None,
            phase_measurements,
            initial_unit_info: None, // Set by orchestrator before execution
            phase_results: HashMap::new(), // Populated by orchestrator before execution
            dependency_id: id,       // Original jobs: dependents wait on this UUID
            sequence_path: Vec::new(),
            sequence_stages: Vec::new(),
            inputs: Default::default(),
            plug_aliases: Default::default(),
            run: Default::default(),
            capture_locals: false,
            unavailable_plugs: Default::default(),
        }
    }

    /// A prompt-only (or empty) phase's job: the engine runs it itself.
    pub fn new_native(spec: JobSpec, job_map: &HashMap<String, Uuid>) -> Self {
        let mut job = Self::new(spec, String::new(), String::new(), job_map);
        job.runtime_type = RuntimeType::Native;
        job
    }

    /// A shell phase's job. It records no measurement.
    pub fn new_shell(mut spec: JobSpec, shell: ShellSpec, job_map: &HashMap<String, Uuid>) -> Self {
        spec.phase_measurements = Vec::new();
        let mut job = Self::new(spec, String::new(), String::new(), job_map);
        job.runtime_type = RuntimeType::Shell;
        job.command = Some(shell.command);
        job.shell_type = shell.shell_type;
        job.working_directory = shell.working_directory;
        job.procedure_dir = shell.procedure_dir;
        job
    }

    /// Check if this is a shared procedure job
    pub fn is_shared(&self) -> bool {
        self.slot_id.is_none()
    }

    pub fn is_teardown(&self) -> bool {
        matches!(
            self.stage_scope,
            StageScope::TeardownEach | StageScope::TeardownAll
        )
    }

    pub fn dependencies_satisfied(&self, completed_jobs: &HashSet<Uuid>) -> bool {
        self.depends_on.is_subset(completed_jobs)
    }

    pub fn can_retry(&self) -> bool {
        self.retry_count < self.retry_limit
    }

    pub fn create_retry_job(&self) -> Self {
        let mut retry_job = self.clone();
        retry_job.id = Uuid::new_v4(); // New ID for retry
        retry_job.retry_count += 1;
        retry_job.status = JobStatus::Pending;
        // dependency_id is preserved via clone — retry resolves the same dependency as the original
        retry_job
    }
}

/// The job a dependency named `dep` points to: a slot job's own slot's
/// job, or the one shared job when `dep` is a shared phase.
pub(crate) fn resolve_dependency(
    job_map: &HashMap<String, Uuid>,
    slot_id: Option<&str>,
    dep: &str,
) -> Option<Uuid> {
    match slot_id {
        Some(sid) => job_map
            .get(&format!("{sid}:{dep}"))
            .or_else(|| job_map.get(&format!("SHARED:{dep}"))),
        None => job_map.get(&format!("SHARED:{dep}")),
    }
    .copied()
}

#[cfg(test)]
mod dependency_lookup_tests {
    use super::*;

    fn job(slot: Option<&str>, depends_on: &[&str], job_map: &HashMap<String, Uuid>) -> Job {
        let mut spec = JobSpec::bare(
            slot.map(str::to_string),
            "use_bench",
            "Use bench",
            StageScope::Main,
        );
        spec.depends_on = depends_on.iter().map(|s| s.to_string()).collect();
        Job::new(spec, "phases.use_bench".into(), "use_bench".into(), job_map)
    }

    /// A slot phase naming a shared phase (a `scope: execution` setup)
    /// binds to the one shared job instead of dropping the dependency.
    #[test]
    fn slot_job_resolves_a_dependency_on_a_shared_phase() {
        let shared = Uuid::new_v4();
        let map = HashMap::from([("SHARED:calibrate".to_string(), shared)]);
        let j = job(Some("A"), &["calibrate"], &map);
        assert!(j.depends_on.contains(&shared));
    }

    /// The slot's own job wins when both exist.
    #[test]
    fn slot_job_prefers_its_own_slot_job() {
        let own = Uuid::new_v4();
        let shared = Uuid::new_v4();
        let map = HashMap::from([
            ("A:calibrate".to_string(), own),
            ("SHARED:calibrate".to_string(), shared),
        ]);
        let j = job(Some("A"), &["calibrate"], &map);
        assert!(j.depends_on.contains(&own) && !j.depends_on.contains(&shared));
    }
}
