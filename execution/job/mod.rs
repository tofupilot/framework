use crate::execution::constants::limits;
use crate::features::operator_ui::{PythonPhaseResult, UiConfig};
use crate::execution::log::LogEntry;
use crate::procedure::schema::{MeasurementSpec, StageScope};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

// Completion outcomes (no lifecycle states)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    Error,
    Timeout,
    Aborted,
    Skip,
}

// Lifecycle states (no completion outcomes)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Idle,
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
    ) -> Self {
        // Terminal errors → ERROR
        if error.is_some() {
            return Outcome::Error;
        }

        // Manual abort or shutdown → ABORTED
        if shutdown_requested || matches!(result, PhaseResult::Stop) {
            return Outcome::Aborted;
        }

        // Timeout → TIMEOUT
        if timeout_secs.is_some() {
            return Outcome::Timeout;
        }

        // Retry/Skip → SKIP
        if matches!(result, PhaseResult::Retry | PhaseResult::Skip) {
            return Outcome::Skip;
        }

        // Explicit failures → FAIL
        if matches!(result, PhaseResult::Fail | PhaseResult::Stop) {
            return Outcome::Fail;
        }

        // Measurement failures → FAIL
        if !measurements_pass {
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
    pub result: Option<JobResult>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ResourceMetrics {
    pub cpu_usage_percent: f32,
    pub cpu_time_seconds: f64,
    pub memory_peak_mb: f64,
    pub memory_avg_mb: f64,
    #[specta(type = u32)]
    pub process_count: usize, // Main + children
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub phase_result: PhaseResult,
    pub phase_outcome: Outcome, // Always computed - REQUIRED
    pub next_action: Option<crate::procedure::schema::PhaseNextAction>, // Computed in completion handler after applying then config
    pub timeout_secs: Option<u64>,                                      // Set if phase timed out
    pub error: Option<String>,  // Set if execution error occurred
    pub exit_code: Option<i32>, // Exit code from sys.exit() if called
    pub measurements: Vec<crate::measurements::Measurement>,
    pub logs: Vec<LogEntry>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: chrono::DateTime<chrono::Utc>,
    pub resource_metrics: Option<ResourceMetrics>,
    pub unit: Option<crate::execution::types::UnitInfo>,
    pub retry_count: usize,
}

impl JobResult {
    pub fn new_error(error_msg: String) -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Continue,
            phase_outcome: Outcome::Error,
            next_action: None,   // Will be computed in completion handler
            timeout_secs: None,
            error: Some(error_msg),
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    pub fn new_timeout(secs: u64) -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Continue,
            phase_outcome: Outcome::Timeout,
            next_action: None,   // Will be computed in completion handler
            timeout_secs: Some(secs),
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    pub fn new_skip() -> Self {
        let now = chrono::Utc::now();
        Self {
            phase_result: PhaseResult::Skip,
            phase_outcome: Outcome::Skip,
            next_action: None,   // Will be computed in completion handler
            timeout_secs: None,
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    /// Check if this result represents a failure (for retry logic)
    pub fn is_failure(&self) -> bool {
        self.error.is_some()
            || self.timeout_secs.is_some()
            || matches!(self.phase_result, PhaseResult::Stop | PhaseResult::Fail)
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

impl Job {
    pub fn new(
        slot_id: Option<String>,
        phase_key: String,
        phase_name: String,
        stage_scope: StageScope,
        module: String,
        function: String,
        depends_on: Vec<String>,
        required_plugs: Vec<String>,
        ui_config: UiConfig,
        timeout_ms: Option<u64>,
        retry_limit: Option<usize>,
        retry_delay_ms: Option<u64>,
        job_map: &HashMap<String, Uuid>,
        phase_measurements: Vec<MeasurementSpec>,
    ) -> Self {
        let id = Uuid::new_v4();

        // Convert phase dependency IDs to job UUIDs
        let dependencies: HashSet<Uuid> = depends_on
            .iter()
            .filter_map(|dep| {
                // Look for dependency in same context
                let key = match &slot_id {
                    Some(sid) => format!("{}:{}", sid, dep),
                    None => format!("SHARED:{}", dep),
                };
                job_map.get(&key).copied()
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
            result: None,
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
        }
    }

    // Helper constructor for native phases
    pub fn new_native(
        slot_id: Option<String>,
        phase_key: String,
        phase_name: String,
        stage_scope: StageScope,
        depends_on: Vec<String>,
        required_plugs: Vec<String>,
        ui_config: UiConfig,
        timeout_ms: Option<u64>,
        retry_limit: Option<usize>,
        retry_delay_ms: Option<u64>,
        job_map: &HashMap<String, Uuid>,
        phase_measurements: Vec<MeasurementSpec>,
    ) -> Self {
        let mut job = Self::new(
            slot_id,
            phase_key,
            phase_name,
            stage_scope,
            String::new(),
            String::new(), // Empty module/function for native
            depends_on,
            required_plugs,
            ui_config,
            timeout_ms,
            retry_limit,
            retry_delay_ms,
            job_map,
            phase_measurements,
        );
        job.runtime_type = RuntimeType::Native;
        job
    }

    // Helper constructor for shell phases
    pub fn new_shell(
        slot_id: Option<String>,
        phase_key: String,
        phase_name: String,
        stage_scope: StageScope,
        command: String,
        depends_on: Vec<String>,
        required_plugs: Vec<String>,
        ui_config: UiConfig,
        timeout_ms: Option<u64>,
        retry_limit: Option<usize>,
        retry_delay_ms: Option<u64>,
        job_map: &HashMap<String, Uuid>,
        shell_type: Option<String>,
        working_directory: Option<String>,
        procedure_dir: Option<String>,
    ) -> Self {
        let mut job = Self::new(
            slot_id,
            phase_key,
            phase_name,
            stage_scope,
            String::new(),
            String::new(), // Empty module/function for shell
            depends_on,
            required_plugs,
            ui_config,
            timeout_ms,
            retry_limit,
            retry_delay_ms,
            job_map,
            Vec::new(), // Empty measurements for shell
        );
        job.runtime_type = RuntimeType::Shell;
        job.command = Some(command);
        job.shell_type = shell_type;
        job.working_directory = working_directory;
        job.procedure_dir = procedure_dir;
        job
    }

    /// Check if this is a shared procedure job
    pub fn is_shared(&self) -> bool {
        self.slot_id.is_none()
    }

    /// Get slot ID or a display string for shared jobs
    pub fn get_slot_display(&self) -> String {
        self.slot_id
            .clone()
            .unwrap_or_else(|| "<shared>".to_string())
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
        retry_job.result = None;
        retry_job // timeout_ms and retry_delay_ms are preserved via clone
    }

    pub fn is_failure(&self) -> bool {
        self.result
            .as_ref()
            .map(|r| r.is_failure())
            .unwrap_or(false)
    }

    pub fn should_stop_test(&self) -> bool {
        self.result
            .as_ref()
            .map(|r| r.should_stop_test())
            .unwrap_or(false)
    }

    pub fn should_skip_measurements(&self) -> bool {
        matches!(
            self.result.as_ref().map(|r| &r.phase_result),
            Some(PhaseResult::Skip)
        )
    }
}
