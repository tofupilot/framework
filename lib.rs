//! Test execution framework for hardware testing procedures.
//!
//! This library provides orchestration, execution, and validation for YAML-defined
//! test procedures with Python phases, instrument control via plugs, and
//! parallel execution across multiple test slots with resource-aware scheduling.
//!
//! # Core Components
//!
//! - [`execution`]: Test orchestrator, worker pool, and job scheduling
//! - [`plugs`]: Resource management and plug lifecycle (setup/teardown)
//! - [`schema`]: YAML procedure schema and types
//! - [`validation`]: Schema validation and diagnostic reporting
//! - [`python`]: Python environment resolution and execution
//! - [`measurements`]: Measurement evaluation and validation

pub mod cli;
pub mod schema;
pub mod loader;
pub mod utils;
pub mod execution;
pub mod measurements;
pub mod plugs;
pub mod system_monitor;
pub mod validation;
pub mod python;
pub mod procedure_context;

use execution::orchestrator::{ExecutionStrategy, ExecutionStats, Orchestrator};
use python::resolve_python_executable;
use std::path::{Path, PathBuf};

// Re-export cli_output for easier access
pub use execution::cli_output;

// Plug status for the plug page UI (separate from orchestrator)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlugStatus {
    pub name: String,
    pub connected: bool,
    pub id: String,
    pub channel_1_value: Option<String>,
}

// Type-safe plug status update events for orchestrator
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub enum PlugStatusValue {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "initializing")]
    Initializing,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "destructing")]
    Destructing,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "skipped")]
    Skipped,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub enum PlugScope {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "each")]
    Each,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub enum PlugStage {
    #[serde(rename = "setup")]
    Setup,
    #[serde(rename = "teardown")]
    Teardown,
    #[serde(rename = "manual")]
    Manual,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct PlugStatusUpdateEvent {
    pub plug_key: String,
    pub plug_name: String,
    pub scope: PlugScope,
    pub slot_id: Option<String>,
    pub stage: PlugStage,
    pub status: PlugStatusValue,
}

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::process::Command as StdCommand;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex as TokioMutex};


fn validate_procedure_file(procedure_file: &Path) -> Result<(), String> {
    if !procedure_file.exists() {
        return Err("Procedure file does not exist".to_string());
    }

    let extension = procedure_file.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if extension != "yaml" && extension != "yml" {
        return Err("File must be a YAML file".to_string());
    }

    Ok(())
}

// Pending unit input request with config for validation
pub struct PendingUnitInput {
    pub sender: tokio::sync::oneshot::Sender<UnitInfo>,
    pub unit_config: Option<schema::UnitConfig>,
}

// Global state for deep link registration
pub struct DeepLinkState {
    pub is_registered: Arc<TokioMutex<bool>>,
}

impl Default for DeepLinkState {
    fn default() -> Self {
        Self {
            is_registered: Arc::new(TokioMutex::new(false)),
        }
    }
}

// Global state for orchestrator instances
pub struct OrchestratorState {
    pub orchestrators: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Orchestrator>>>>>,
    pub state_refs: Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<execution::state::OrchestratorState>>>>>,
    pub worker_refs: Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<Vec<execution::worker::Worker>>>>>>,
    pub resource_manager_refs: Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<plugs::manager::ResourceManager>>>>>,
    pub pending_unit_inputs: Arc<TokioMutex<HashMap<String, PendingUnitInput>>>,
}

impl Default for OrchestratorState {
    fn default() -> Self {
        Self {
            orchestrators: Arc::new(TokioMutex::new(HashMap::new())),
            state_refs: Arc::new(TokioMutex::new(HashMap::new())),
            worker_refs: Arc::new(TokioMutex::new(HashMap::new())),
            resource_manager_refs: Arc::new(TokioMutex::new(HashMap::new())),
            pending_unit_inputs: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }
}

// Global state for UI response channels
pub struct UIResponseState {
    pub senders:
        Arc<TokioMutex<HashMap<usize, oneshot::Sender<HashMap<String, serde_json::Value>>>>>,
    pub completed_requests: Arc<TokioMutex<HashSet<String>>>, // Track completed UI requests
}

impl Default for UIResponseState {
    fn default() -> Self {
        Self {
            senders: Arc::new(TokioMutex::new(HashMap::new())),
            completed_requests: Arc::new(TokioMutex::new(HashSet::new())),
        }
    }
}

// Global state for standalone plug service management (for manual debug plugs)
pub struct StandalonePlugServiceState {
    pub resource_managers: Arc<TokioMutex<HashMap<String, Arc<plugs::manager::ResourceManager>>>>,
}

impl Default for StandalonePlugServiceState {
    fn default() -> Self {
        Self {
            resource_managers: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PythonInstallation {
    pub name: String,
    pub path: String,
    pub version: String,
    pub executable: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestPhase {
    pub name: String,
    pub line_number: u32,
    pub is_measurement: bool,
    pub enabled: bool,
    pub code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenHTFTest {
    pub file_path: String,
    pub file_name: String,
    pub phases: Vec<TestPhase>,
    pub full_code: String,
    pub last_modified: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct UnitInfo {
    pub serial_number: Option<String>,
    pub part_number: Option<String>,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    pub sub_units: Option<Vec<String>>,
    pub status: String,
}

/// Validate a single unit field against its configuration
fn validate_unit_field(
    field_name: &str,
    value: &Option<String>,
    config: &schema::UnitFieldConfig,
) -> Result<(), String> {
    // Serial number and part number are always required
    let is_required = field_name == "serial_number" || field_name == "part_number";

    if is_required {
        let val = value
            .as_ref()
            .ok_or_else(|| format!("{} is required", field_name))?;

        if val.trim().is_empty() {
            return Err(format!("{} cannot be empty", field_name));
        }
    }

    if let Some(val) = value {
        let trimmed = val.trim();

        // Ensure at least 1 character after trim if value is provided
        if trimmed.is_empty() {
            return Err(format!("{} cannot be empty or contain only whitespace", field_name));
        }

        // Check min_length on trimmed value
        if let Some(min) = config.min_length {
            if trimmed.len() < min {
                return Err(format!(
                    "{} must be at least {} characters (got {})",
                    field_name,
                    min,
                    trimmed.len()
                ));
            }
        }

        // Check max_length on trimmed value
        if let Some(max) = config.max_length {
            if trimmed.len() > max {
                return Err(format!(
                    "{} must be at most {} characters (got {})",
                    field_name,
                    max,
                    trimmed.len()
                ));
            }
        }

        // Check pattern on trimmed value
        if let Some(pattern) = &config.pattern {
            let regex = regex::Regex::new(pattern).map_err(|e| {
                format!("Invalid validation pattern for {}: {}", field_name, e)
            })?;

            if !regex.is_match(trimmed) {
                return Err(format!(
                    "{} does not match required format: {}",
                    field_name, pattern
                ));
            }
        }
    }

    Ok(())
}

/// Validate all unit fields against configuration
fn validate_unit_info(
    unit_info: &UnitInfo,
    unit_config: &Option<schema::UnitConfig>,
) -> Result<(), String> {
    let config = match unit_config {
        Some(c) => c,
        None => return Ok(()), // No config = no validation
    };

    // Validate built-in fields
    if let Some(sn_config) = &config.serial_number {
        validate_unit_field("Serial Number", &unit_info.serial_number, sn_config)?;
    }

    if let Some(pn_config) = &config.part_number {
        validate_unit_field("Part Number", &unit_info.part_number, pn_config)?;
    }

    if let Some(rev_config) = &config.revision_number {
        validate_unit_field("Revision Number", &unit_info.revision_number, rev_config)?;
    }

    if let Some(batch_config) = &config.batch_number {
        validate_unit_field("Batch Number", &unit_info.batch_number, batch_config)?;
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RunResult {
    Passed(HashMap<String, f64>),
    Failed(String),
    Error(String),
}

// CLI functionality with new orchestrator
pub fn run_cli_mode(procedure_dir: PathBuf, procedure_file: Option<String>) {
    // Initialize CLI output system
    execution::cli_output::init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to create async runtime: {}", e);
            std::process::exit(1);
        }
    };
    runtime.block_on(async {
        // Load procedure definition
        let procedure_def = if let Some(ref file) = procedure_file {
            let full_path = procedure_dir.join(file);
            match loader::load_procedure_definition(&full_path) {
                Ok(def) => def,
                Err(e) => {
                    execution::cli_output::print_section(execution::cli_output::Section::Error, format!(" Failed to load procedure: {}", e));
                    return;
                }
            }
        } else {
            execution::cli_output::print_section(execution::cli_output::Section::Error, " No procedure file specified");
            return;
        };

        // Get worker count from YAML or use CPU count as default
        let effective_worker_count = procedure_def.worker_count.unwrap_or_else(num_cpus::get);

        let mut orchestrator =
            Orchestrator::new(effective_worker_count, procedure_dir.clone());

        // Initialize workers
        if let Err(e) = orchestrator.initialize().await {
            execution::cli_output::print_section(execution::cli_output::Section::Error, format!(" Failed to initialize workers: {}", e));
            return;
        }

        // Get slots from YAML configuration
        let slots: Vec<String> = if procedure_def.slots.is_empty() {
            // If no slots defined in YAML, use single default slot
            vec!["SLOT_1".to_string()]
        } else {
            procedure_def.slots.iter().map(|s| s.key.clone()).collect()
        };

        // Validate slot names - ensure none are empty (reserved for shared phases)
        for slot in &slots {
            if slot.trim().is_empty() {
                execution::cli_output::print_section(execution::cli_output::Section::Error, " Error: Slot names cannot be empty (reserved for shared phases)");
                return;
            }
        }

        // Initialize report manager with UUID
        let execution_uuid = uuid::Uuid::new_v4(); // Single execution ID for all slots
        if let Some(file) = procedure_file {
            let full_path = procedure_dir.join(file);
            if let Err(e) = orchestrator
                .initialize_report_managers(
                    &full_path,
                    &execution_uuid.to_string(),
                    &procedure_def,
                    &slots,
                )
                .await
            {
                execution::cli_output::verbose(format!("WARNING: Failed to initialize report manager: {}", e));
                // Continue execution even if report manager fails
            }
        }

        // Show configuration
        execution::cli_output::print_section(execution::cli_output::Section::Config, format!(
            "Workers: {} | Slots: {}",
            effective_worker_count,
            slots.len()
        ));

        // Submit procedure with execution model (CLI mode has no initial unit input)
        let empty_unit_info = UnitInfo {
            serial_number: None,
            part_number: None,
            revision_number: None,
            batch_number: None,
            sub_units: None,
            status: "active".to_string(),
        };

        if let Err(e) = orchestrator
            .submit_procedure(&procedure_def, slots, procedure_def.strategy, empty_unit_info)
            .await
        {
            execution::cli_output::print_section(execution::cli_output::Section::Error, format!(" Failed to submit procedure: {}", e));
            return;
        }

        // Execute all jobs
        let exit_code = match orchestrator.execute_all(None).await {
            Ok(stats) => {
                println!();

                let passed = stats.completed_jobs - stats.failed_jobs;
                let status = if stats.failed_jobs == 0 { "PASSED" } else { "FAILED" };

                execution::cli_output::print_section(
                    execution::cli_output::Section::Summary,
                    format!("Result: {} ({} passed, {} failed)", status, passed, stats.failed_jobs)
                );

                if stats.failed_jobs == 0 { 0 } else { 1 }
            }
            Err(e) => {
                execution::cli_output::print_section(execution::cli_output::Section::Error, format!("Execution failed: {}", e));
                1
            }
        };

        // Shutdown workers
        let _ = orchestrator.shutdown(None).await;

        std::process::exit(exit_code);
    });
}

#[tauri::command]
async fn get_system_info() -> Result<String, String> {
    let cpu_count = num_cpus::get();

    let info = serde_json::json!({
        "cpu_cores": cpu_count,
        "default_workers": cpu_count,
        "recommendations": {
            "cpu_heavy": cpu_count,
            "io_heavy": cpu_count * 2,
            "memory_limited": (cpu_count / 2).max(1),
            "balanced": cpu_count
        },
        "guidelines": {
            "cpu_heavy": format!("Use {} workers for CPU-intensive tests", cpu_count),
            "io_heavy": format!("Try {} workers for I/O-heavy tests", cpu_count * 2),
            "memory_limited": format!("Use {} workers if memory is limited", (cpu_count / 2).max(1)),
            "general": format!("Default {} workers works for most cases", cpu_count)
        }
    });

    Ok(info.to_string())
}

#[tauri::command]
async fn is_deep_link_registered(state: tauri::State<'_, DeepLinkState>) -> Result<bool, String> {
    let registered = state.is_registered.lock().await;
    Ok(*registered)
}

#[tauri::command]
async fn get_procedure_context(procedure_file: String) -> Result<procedure_context::ProcedureContext, String> {
    procedure_context::ProcedureContext::from_procedure_file(procedure_file)
}

#[tauri::command]
async fn get_procedure_metadata(procedure_file: String) -> Result<serde_json::Value, String> {
    let procedure_file_buf = PathBuf::from(&procedure_file);
    validate_procedure_file(&procedure_file_buf)?;

    let procedure_def = loader::load_procedure_definition(&procedure_file_buf)
        .map_err(|e| format!("Failed to load procedure: {}", e))?;

    // Collect all phases in execution order
    let mut all_phases = Vec::new();

    // Helper function to serialize a phase with all control flow fields
    let serialize_phase = |phase: &schema::procedure::PhaseDefinition, stage_scope_str: &str, shared: bool| {
        let mut phase_obj = serde_json::json!({
            "name": phase.name,
            "stage_scope": stage_scope_str,
            "shared": shared,
            "enabled": phase.enabled
        });

        if let Some(ref timeout) = phase.timeout {
            phase_obj["timeout"] = serde_json::Value::String(
                utils::duration::format_duration(*timeout)
            );
        }

        if !phase.depends_on.is_empty() {
            phase_obj["wait_for"] = serde_json::to_value(&phase.depends_on).unwrap_or_default();
        }

        if let Some(ref retry) = phase.retry {
            let mut retry_obj = serde_json::json!({
                "limit": retry.limit
            });
            if let Some(delay) = retry.delay {
                retry_obj["delay"] = serde_json::Value::String(
                    utils::duration::format_duration(delay)
                );
            }
            phase_obj["retry"] = retry_obj;
        }

        if let Some(ref then) = phase.then {
            phase_obj["then"] = serde_json::to_value(then).unwrap_or_default();
        }

        if let Some(ref result) = phase.result {
            phase_obj["result"] = serde_json::json!(result);
        }

        phase_obj
    };

    // Add all phases using standardized iterator
    for (stage_scope, phase) in procedure_def.iter_phases_with_stage() {
        use schema::procedure::StageScope;
        let (group, is_shared) = match stage_scope {
            StageScope::SetupAll => ("setup_all", true),
            StageScope::SetupEach => ("setup_each", false),
            StageScope::Main => ("main", false),
            StageScope::TeardownEach => ("teardown_each", false),
            StageScope::TeardownAll => ("teardown_all", true),
        };
        all_phases.push(serialize_phase(phase, group, is_shared));
    }

    // Return metadata including phase list and plugs
    let metadata = serde_json::json!({
        "strategy": procedure_def.strategy,
        "worker_count": procedure_def.worker_count,
        "phases": all_phases,
        "plugs": procedure_def.plugs
    });

    Ok(metadata)
}

#[tauri::command]
async fn execute_parallel_runs(
    app_handle: AppHandle,
    procedure_file: String,
    procedure_dir: String,
    slots: Vec<String>,
    worker_count_override: Option<usize>,
    strategy: Option<String>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<String, String> {
    let procedure_file_buf = PathBuf::from(&procedure_file);
    validate_procedure_file(&procedure_file_buf)?;

    let procedure_dir = PathBuf::from(&procedure_dir);
    let procedure_def = loader::load_procedure_definition(&procedure_file_buf)
        .map_err(|e| format!("Failed to load procedure: {}", e))?;

    let yaml_content = std::fs::read_to_string(&procedure_file_buf)
        .map_err(|e| format!("Failed to read YAML file: {}", e))?;

    // Validate procedure before execution
    let validation_result = validation::validate_procedure_with_yaml(&app_handle, &procedure_def, &yaml_content, &procedure_dir).await;
    if !validation_result.is_valid {
        let error_messages: Vec<String> = validation_result
            .diagnostics
            .iter()
            .filter(|d| matches!(d.severity, validation::DiagnosticSeverity::Error))
            .map(|d| format!("• {}", d.message))
            .collect();
        return Err(format!(
            "Procedure validation failed:\n{}",
            error_messages.join("\n")
        ));
    }

    // Validate slot names
    let slot_diagnostics = validation::validate_slot_names(&slots);
    let slot_errors: Vec<_> = slot_diagnostics.iter()
        .filter(|d| matches!(d.severity, validation::DiagnosticSeverity::Error))
        .collect();
    if !slot_errors.is_empty() {
        let error_messages: Vec<String> = slot_errors
            .iter()
            .map(|e| format!("• {}", e.message))
            .collect();
        return Err(format!(
            "Slot validation failed:\n{}",
            error_messages.join("\n")
        ));
    }

    // Initialize report manager with UUID
    let execution_uuid = uuid::Uuid::new_v4(); // Single execution ID for all slots
    let execution_id_str = execution_uuid.to_string();

    // Determine worker count: UI override > YAML > CPU count (default)
    let worker_count = worker_count_override
        .or(procedure_def.worker_count)
        .unwrap_or_else(num_cpus::get);

    log::info!("Initializing {} workers for execution", worker_count);

    let mut orchestrator = Orchestrator::new(worker_count, procedure_dir.clone());
    orchestrator.set_app_handle(app_handle.clone());
    orchestrator
        .initialize()
        .await
        .map_err(|e| {
            log::error!("Worker initialization failed: {}", e);
            format!("Failed to initialize workers: {}", e)
        })?;

    // Emit unit input request and wait for response
    let (unit_input_tx, unit_input_rx) = tokio::sync::oneshot::channel();
    {
        let mut pending_unit_inputs = orchestrator_state.pending_unit_inputs.lock().await;
        pending_unit_inputs.insert(
            execution_id_str.clone(),
            PendingUnitInput {
                sender: unit_input_tx,
                unit_config: procedure_def.unit.clone(),
            },
        );
    }

    let _ = app_handle.emit(
        "unit-input-request",
        serde_json::json!({
            "execution_id": &execution_id_str,
            "unit_config": &procedure_def.unit
        }),
    );

    // Wait for unit input with timeout (5 minutes)
    let unit_info = tokio::time::timeout(std::time::Duration::from_secs(300), unit_input_rx)
        .await
        .map_err(|_| "Unit input timeout: no serial number received within 5 minutes".to_string())?
        .map_err(|_| "Unit input channel closed before receiving serial number".to_string())?;

    execution::cli_output::verbose(format!("Received unit serial number: {}", unit_info.serial_number.as_deref().unwrap_or("none")));

    // Store unit info in orchestrator so initialize_report_managers can use it
    orchestrator.set_initial_unit_info(unit_info.clone());

    // Initialize report managers AFTER we have unit info
    if let Err(e) = orchestrator
        .initialize_report_managers(
            &procedure_file_buf,
            &execution_uuid.to_string(),
            &procedure_def,
            &slots,
        )
        .await
    {
        execution::cli_output::verbose(format!("WARNING: Failed to initialize report manager: {}", e));
        // Continue execution even if report manager fails
    }

    // Determine execution strategy: UI override > YAML > default (phase_first)
    let exec_strategy = if let Some(ui_strategy) = strategy.as_deref() {
        match ui_strategy {
            "slot_first" => ExecutionStrategy::SlotFirst,
            "phase_first" => ExecutionStrategy::PhaseFirst,
            other => {
                execution::cli_output::print_section(execution::cli_output::Section::Error, format!(
                    " Invalid UI execution strategy: {}. Using default (phase_first)",
                    other
                ));
                ExecutionStrategy::PhaseFirst
            }
        }
    } else {
        procedure_def.strategy
    };

    // Submit procedure with determined execution strategy and initial unit info
    orchestrator
        .submit_procedure(&procedure_def, slots, exec_strategy, unit_info)
        .await
        .map_err(|e| format!("Failed to submit procedure: {}", e))?;

    // Get initial stats after submission
    let initial_stats = orchestrator.get_stats().await;

    // Store orchestrator and its Arc references for status monitoring and force kill
    let state_arc = orchestrator.state.clone();
    let workers_arc = orchestrator.workers.clone();
    let resource_manager_arc = orchestrator.resource_manager.clone();
    let orchestrator_arc = Arc::new(TokioMutex::new(orchestrator));
    {
        let mut orchestrators = orchestrator_state.orchestrators.lock().await;
        orchestrators.insert(execution_id_str.clone(), orchestrator_arc.clone());
    }
    {
        let mut state_refs = orchestrator_state.state_refs.lock().await;
        state_refs.insert(execution_id_str.clone(), state_arc);
    }
    {
        let mut worker_refs = orchestrator_state.worker_refs.lock().await;
        worker_refs.insert(execution_id_str.clone(), workers_arc);
    }
    {
        let mut resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;
        resource_manager_refs.insert(execution_id_str.clone(), resource_manager_arc);
    }

    // Emit initial stats immediately
    let _ = app_handle.emit(
        "execution-progress",
        serde_json::json!({
            "execution_id": &execution_id_str,
            "stats": initial_stats
        }),
    );

    // Start execution in background and emit progress updates
    let app_handle_clone = app_handle.clone();
    let orchestrator_clone = orchestrator_arc.clone();
    let key_clone = execution_id_str.clone();
    let orchestrators_map_clone = orchestrator_state.orchestrators.clone();
    let state_refs_map_clone = orchestrator_state.state_refs.clone();
    let worker_refs_map_clone = orchestrator_state.worker_refs.clone();
    let resource_manager_refs_map_clone = orchestrator_state.resource_manager_refs.clone();

    tokio::spawn(async move {
        // Start the actual execution with app handle for UI support
        let app_handle_for_execution = Some(app_handle_clone.clone());
        let orchestrator_for_exec = orchestrator_clone.clone();

        let execution_handle = {
            // Spawn the actual execution in a separate task
            tokio::spawn(async move {
                let mut orchestrator = orchestrator_for_exec.lock().await;
                orchestrator.execute_all(app_handle_for_execution).await
            })
        };

        // Monitor progress while execution is running
        loop {
            let stats = {
                let orchestrator = orchestrator_clone.lock().await;
                orchestrator.get_stats().await
            };

            // Emit progress update
            let _ = app_handle_clone.emit(
                "execution-progress",
                serde_json::json!({
                    "execution_id": key_clone,
                    "stats": stats
                }),
            );

            // Check if execution task is complete
            if execution_handle.is_finished() {
                execution::cli_output::debug(format!("Execution task finished for '{}'", key_clone));
                // Shutdown workers BEFORE emitting cleanup-complete so frontend listeners receive the events
                {
                    execution::cli_output::debug(format!("Calling orchestrator.shutdown() for '{}'", key_clone));
                    let mut orchestrator = orchestrator_clone.lock().await;
                    let _ = orchestrator.shutdown(Some(&app_handle_clone)).await;
                    execution::cli_output::debug(format!("orchestrator.shutdown() completed for '{}'", key_clone));
                }

                // Get the execution result
                match execution_handle.await {
                    Ok(Ok(final_stats)) => {
                        // Get final results
                        let results = {
                            let orchestrator = orchestrator_clone.lock().await;
                            let state = orchestrator.state.read().await;
                            state.job_results.iter().map(|(id, result)| {
                                // Determine success and error based on execution context
                                let (success, error) = if let Some(ref e) = result.error {
                                    (false, Some(e.clone()))
                                } else if let Some(secs) = result.timeout_secs {
                                    (false, Some(format!("Timed out after {} seconds", secs)))
                                } else if result.is_failure() {
                                    (false, None)
                                } else {
                                    (true, None)
                                };

                                serde_json::json!({
                                    "job_id": id.to_string(),
                                    "success": success,
                                    "measurements": result.measurements,
                                    "logs": result.logs,
                                    "error": error,
                                    "duration_ms": (result.completed_at - result.started_at).num_milliseconds()
                                })
                            }).collect::<Vec<_>>()
                        };

                        let _ = app_handle_clone.emit(
                            "execution-complete",
                            serde_json::json!({
                                "execution_id": key_clone,
                                "stats": final_stats,
                                "results": results
                            }),
                        );

                        // Emit cleanup complete to signal frontend can reset
                        execution::cli_output::debug(format!("Emitting orchestrator-cleanup-complete for '{}'", key_clone));
                        let _ = app_handle_clone.emit(
                            "orchestrator-cleanup-complete",
                            serde_json::json!({
                                "execution_id": key_clone
                            }),
                        );
                    }
                    Ok(Err(e)) => {
                        // Emit execution-complete with error info
                        let _ = app_handle_clone.emit(
                            "execution-complete",
                            serde_json::json!({
                                "execution_id": key_clone,
                                "stats": {
                                    "completed_jobs": 0,
                                    "failed_jobs": 0,
                                    "skipped_jobs": 0,
                                    "workers_active": 0,
                                    "workers_total": 0
                                },
                                "results": [],
                                "error": e
                            }),
                        );

                        // Emit cleanup complete so frontend can reset
                        let _ = app_handle_clone.emit(
                            "orchestrator-cleanup-complete",
                            serde_json::json!({
                                "execution_id": key_clone
                            }),
                        );
                    }
                    Err(e) => {
                        // Emit execution-complete with panic info
                        let _ = app_handle_clone.emit(
                            "execution-complete",
                            serde_json::json!({
                                "execution_id": key_clone,
                                "stats": {
                                    "completed_jobs": 0,
                                    "failed_jobs": 0,
                                    "skipped_jobs": 0,
                                    "workers_active": 0,
                                    "workers_total": 0
                                },
                                "results": [],
                                "error": format!("Execution task panicked: {}", e)
                            }),
                        );

                        // Emit cleanup complete so frontend can reset
                        let _ = app_handle_clone.emit(
                            "orchestrator-cleanup-complete",
                            serde_json::json!({
                                "execution_id": key_clone
                            }),
                        );
                    }
                }

                // Clean up orchestrator and all refs from maps
                {
                    let mut orchestrators = orchestrators_map_clone.lock().await;
                    orchestrators.remove(&key_clone);
                }
                {
                    let mut state_refs = state_refs_map_clone.lock().await;
                    state_refs.remove(&key_clone);
                }
                {
                    let mut worker_refs = worker_refs_map_clone.lock().await;
                    worker_refs.remove(&key_clone);
                }
                {
                    let mut resource_manager_refs = resource_manager_refs_map_clone.lock().await;
                    resource_manager_refs.remove(&key_clone);
                }
                execution::cli_output::debug(format!("Orchestrator '{}' teardown complete", key_clone));

                let _ = app_handle_clone.emit(
                    "orchestrator-teardown-complete",
                    serde_json::json!({
                        "execution_id": key_clone
                    }),
                );

                break;
            }

            // Small delay before next progress check
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });

    Ok(execution_id_str)
}

#[tauri::command]
async fn get_execution_stats(
    execution_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<ExecutionStats, String> {
    let orchestrators = orchestrator_state.orchestrators.lock().await;

    if let Some(orchestrator_arc) = orchestrators.get(&execution_id) {
        let orchestrator = orchestrator_arc.lock().await;
        Ok(orchestrator.get_stats().await)
    } else {
        Err("Orchestrator not found".to_string())
    }
}

#[tauri::command]
async fn stop_execution(
    execution_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<(), String> {
    execution::cli_output::error(format!("Requesting stop for orchestrator '{}'", execution_id));

    // Cancel pending unit input if any
    {
        let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.remove(&execution_id);
    }

    // Get the state Arc directly - this doesn't require locking the orchestrator
    let state_arc = {
        let state_refs = orchestrator_state.state_refs.lock().await;
        state_refs.get(&execution_id).cloned()
    };

    let Some(state_arc) = state_arc else {
        return Ok(());
    };

    // Set shutdown_requested flag immediately - this works even while execute_all is running!
    {
        let mut state = state_arc.write().await;
        if state.shutdown_requested {
            return Ok(());
        }
        state.shutdown_requested = true;
    }

    Ok(())
}

#[tauri::command]
async fn submit_unit_input(
    execution_id: String,
    unit_data: HashMap<String, String>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<(), String> {
    execution::cli_output::verbose(format!(
        "Submitting unit info for execution '{}': {:?}",
        execution_id, unit_data
    ));

    let pending_input = {
        let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.remove(&execution_id)
    };

    if let Some(pending_input) = pending_input {
        let serial_number = unit_data.get("serial_number").cloned();
        let part_number = unit_data.get("part_number").cloned();
        let revision_number = unit_data.get("revision_number").cloned();
        let batch_number = unit_data.get("batch_number").cloned();

        let unit_info = UnitInfo {
            serial_number,
            part_number,
            revision_number,
            batch_number,
            sub_units: None,
            status: "active".to_string(),
        };

        // Validate unit info against config
        validate_unit_info(&unit_info, &pending_input.unit_config)?;

        pending_input
            .sender
            .send(unit_info)
            .map_err(|_| "Failed to send unit input: execution may have been cancelled".to_string())?;
        Ok(())
    } else {
        Err(format!(
            "No pending unit input request for execution '{}': may have already been submitted or timed out",
            execution_id
        ))
    }
}

#[tauri::command]
async fn kill_execution(
    execution_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // Cancel pending unit input if any
    {
        let mut pending = orchestrator_state.pending_unit_inputs.lock().await;
        pending.remove(&execution_id);
    }

    let (state_arc, workers_arc, resource_manager_arc) = {
        let state_refs = orchestrator_state.state_refs.lock().await;
        let worker_refs = orchestrator_state.worker_refs.lock().await;
        let resource_manager_refs = orchestrator_state.resource_manager_refs.lock().await;

        let state = state_refs.get(&execution_id).cloned();
        let workers = worker_refs.get(&execution_id).cloned();
        let resource_manager = resource_manager_refs.get(&execution_id).cloned();

        (state, workers, resource_manager)
    };

    let Some(state_arc) = state_arc else {
        execution::cli_output::debug("Killing process aborted : Orchestrator state not found");
        return Ok(());
    };

    let Some(workers_arc) = workers_arc else {
        execution::cli_output::debug("Killing process aborted : Workers not found");
        return Ok(());
    };

    let Some(resource_manager_arc) = resource_manager_arc else {
        execution::cli_output::debug("Killing process aborted : ResourceManager not found");
        return Ok(());
    };

    {
        let state = state_arc.read().await;
        if state.force_kill_requested {
            execution::cli_output::debug("Force kill already requested");
            return Ok(());
        }
    }

    execution::orchestrator::Orchestrator::force_kill_immediate(
        state_arc,
        workers_arc,
        resource_manager_arc,
        Some(execution_id),
        Some(app_handle),
    ).await?;

    execution::cli_output::debug("Force kill completed");

    Ok(())
}

#[tauri::command]
async fn stop_slot(
    execution_id: String,
    slot_id: String,
    orchestrator_state: State<'_, OrchestratorState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let orchestrators = orchestrator_state.orchestrators.lock().await;

    if let Some(orchestrator_arc) = orchestrators.get(&execution_id) {
        let mut orchestrator = orchestrator_arc.lock().await;
        orchestrator
            .stop_slot(&slot_id, Some(&app_handle))
            .await
            .map_err(|e| format!("Failed to stop slot {}: {}", slot_id, e))
    } else {
        Err("Orchestrator not found".to_string())
    }
}

use std::sync::OnceLock;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

static WATCHED_YAML_FILES: OnceLock<Mutex<HashMap<String, AbortHandle>>> = OnceLock::new();

#[tauri::command]
async fn load_procedure_config(procedure_file: String) -> Result<String, String> {
    use std::fs;

    let path = Path::new(&procedure_file);
    validate_procedure_file(path)?;

    let yaml_content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read YAML file: {}", e))?;

    let procedure_yaml: schema::procedure::ProcedureYaml = serde_yaml::from_str(&yaml_content)
        .map_err(|e| format!("Failed to parse YAML: {}", e))?;

    serde_json::to_string(&procedure_yaml)
        .map_err(|e| format!("Failed to serialize to JSON: {}", e))
}

#[tauri::command]
async fn get_test_runs(procedure_dir: String) -> Result<String, String> {
    use std::fs;
    use std::path::Path;
    use crate::execution::runs::{Run, RunSummary};

    let reports_dir = Path::new(&procedure_dir).join("reports");

    if !reports_dir.exists() {
        return Ok(r#"{"runs":[]}"#.to_string());
    }

    let mut summaries: Vec<RunSummary> = Vec::new();

    let entries = fs::read_dir(&reports_dir)
        .map_err(|e| format!("Failed to read reports directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let run_file = path.join("report.json");
        if !run_file.exists() {
            continue;
        }

        let content = match fs::read_to_string(&run_file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[get_test_runs] Failed to read {:?}: {}", run_file, e);
                continue;
            }
        };

        let report: Run = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[get_test_runs] Failed to parse {:?}: {}", run_file, e);
                continue;
            }
        };

        let directory = path.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        summaries.push(RunSummary {
            run_id: report.run_id,
            execution_id: report.execution_id,
            timestamp: report.timestamp,
            directory,
            outcome: serde_json::to_value(&report.outcome)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "error".to_string()),
            duration_ms: report.duration_ms,
            total_phases: report.phases.len(),
            failed_phases: report.stats.phases_failed,
            unit: report.unit,
            has_attachments: report.phases.iter().any(|p| !p.attachments.is_empty()),
            dashboard: report.dashboard,
        });
    }

    summaries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    eprintln!("[get_test_runs] Returning {} runs from {}", summaries.len(), procedure_dir);

    let result = serde_json::json!({ "runs": summaries });
    serde_json::to_string(&result).map_err(|e| format!("Failed to serialize: {}", e))
}

#[tauri::command]
async fn get_test_run(procedure_dir: String, run_dir: String) -> Result<String, String> {
    use std::fs;
    use std::path::Path;

    let run_path = Path::new(&procedure_dir)
        .join("reports")
        .join(&run_dir)
        .join("report.json");

    fs::read_to_string(run_path).map_err(|e| format!("Failed to read report: {}", e))
}

#[tauri::command]
async fn mark_run_uploaded(procedure_dir: String, run_id: String, dashboard_run_id: String) -> Result<(), String> {
    use std::fs;
    use std::path::Path;
    use crate::execution::runs::{Run, DashboardInfo};

    let reports_dir = Path::new(&procedure_dir).join("reports");

    let entries = fs::read_dir(&reports_dir)
        .map_err(|e| format!("Failed to read reports directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let run_file = path.join("report.json");
        if !run_file.exists() {
            continue;
        }

        let content = fs::read_to_string(&run_file)
            .map_err(|e| format!("Failed to read report: {}", e))?;

        let mut report: Run = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse report: {}", e))?;

        if report.run_id == run_id {
            report.dashboard = Some(DashboardInfo {
                run_id: Some(dashboard_run_id),
                upload_error: None,
            });

            let updated_json = serde_json::to_string(&report)
                .map_err(|e| format!("Failed to serialize report: {}", e))?;

            fs::write(&run_file, updated_json)
                .map_err(|e| format!("Failed to write report: {}", e))?;

            return Ok(());
        }
    }

    Err(format!("Report not found: {}", run_id))
}

#[tauri::command]
async fn mark_run_upload_failed(procedure_dir: String, run_id: String, error: String) -> Result<(), String> {
    use std::fs;
    use std::path::Path;
    use crate::execution::runs::{Run, DashboardInfo};

    eprintln!("[mark_run_upload_failed] Looking for run_id: {} in {}", run_id, procedure_dir);

    let reports_dir = Path::new(&procedure_dir).join("reports");

    let entries = fs::read_dir(&reports_dir)
        .map_err(|e| format!("Failed to read reports directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let run_file = path.join("report.json");
        if !run_file.exists() {
            continue;
        }

        let content = fs::read_to_string(&run_file)
            .map_err(|e| format!("Failed to read report: {}", e))?;

        let mut report: Run = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse report: {}", e))?;

        eprintln!("[mark_run_upload_failed] Checking report with run_id: {}", report.run_id);

        if report.run_id == run_id {
            eprintln!("[mark_run_upload_failed] FOUND! Setting error and writing to {:?}", run_file);
            report.dashboard = Some(DashboardInfo {
                run_id: report.dashboard.as_ref().and_then(|d| d.run_id.clone()),
                upload_error: Some(error),
            });

            let updated_json = serde_json::to_string(&report)
                .map_err(|e| format!("Failed to serialize report: {}", e))?;

            fs::write(&run_file, updated_json)
                .map_err(|e| format!("Failed to write report: {}", e))?;

            eprintln!("[mark_run_upload_failed] Successfully wrote updated report");
            return Ok(());
        }
    }

    eprintln!("[mark_run_upload_failed] ERROR: Report not found for run_id: {}", run_id);
    Err(format!("Report not found: {}", run_id))
}

#[tauri::command]
async fn get_run_attachments(procedure_dir: String, run_dir: String) -> Result<String, String> {
    use std::fs;
    use std::path::Path;

    let run_path = Path::new(&procedure_dir)
        .join("reports")
        .join(&run_dir);

    if !run_path.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    let entries = fs::read_dir(&run_path)
        .map_err(|e| format!("Failed to read report directory: {}", e))?;

    let mut attachments = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();

        if path.is_file() {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            if file_name != "report.json" {
                let metadata = fs::metadata(&path)
                    .map_err(|e| format!("Failed to get file metadata: {}", e))?;

                let mime_type = match path.extension().and_then(|ext| ext.to_str()) {
                    Some("png") => "image/png",
                    Some("jpg") | Some("jpeg") => "image/jpeg",
                    Some("gif") => "image/gif",
                    Some("svg") => "image/svg+xml",
                    Some("pdf") => "application/pdf",
                    Some("txt") | Some("log") => "text/plain",
                    Some("csv") => "text/csv",
                    Some("json") => "application/json",
                    Some("xml") => "application/xml",
                    Some("html") => "text/html",
                    Some("zip") => "application/zip",
                    Some("bin") => "application/octet-stream",
                    Some("yaml") | Some("yml") => "text/yaml",
                    _ => "application/octet-stream",
                };

                attachments.push(serde_json::json!({
                    "name": file_name,
                    "size": metadata.len(),
                    "mime_type": mime_type,
                    "path": path.to_str(),
                }));
            }
        }
    }

    let result = serde_json::to_string(&attachments)
        .map_err(|e| format!("Failed to serialize attachments: {}", e))?;

    Ok(result)
}

#[tauri::command]
async fn get_attachment_info(
    procedure_dir: String,
    run_dir: String,
    attachment_name: String,
) -> Result<serde_json::Value, String> {
    use std::fs;
    use std::path::Path;

    let attachment_path = Path::new(&procedure_dir)
        .join("reports")
        .join(&run_dir)
        .join(&attachment_name);

    if !attachment_path.exists() {
        return Err(format!("Attachment not found: {}", attachment_name));
    }

    // Get file metadata
    let metadata = fs::metadata(&attachment_path)
        .map_err(|e| format!("Failed to get file metadata: {}", e))?;

    // Determine MIME type based on file extension
    let mime_type = match attachment_path.extension().and_then(|ext| ext.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") | Some("log") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("html") => "text/html",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    };

    Ok(serde_json::json!({
        "name": attachment_name,
        "size": metadata.len(),
        "mime_type": mime_type,
        "path": attachment_path.to_str(),
    }))
}

#[tauri::command]
async fn open_attachment(
    procedure_dir: String,
    run_dir: String,
    attachment_name: String,
) -> Result<(), String> {
    use std::path::Path;

    let attachment_path = Path::new(&procedure_dir)
        .join("reports")
        .join(&run_dir)
        .join(&attachment_name);

    if !attachment_path.exists() {
        return Err(format!("Attachment not found: {}", attachment_name));
    }

    // Open with default system application
    #[cfg(target_os = "windows")]
    {
        StdCommand::new("explorer")
            .arg(&attachment_path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        StdCommand::new("open")
            .arg(&attachment_path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        StdCommand::new("xdg-open")
            .arg(&attachment_path)
            .spawn()
            .map_err(|e| format!("Failed to open file: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
async fn open_reports_folder(procedure_dir: String) -> Result<(), String> {
    use std::path::Path;

    let reports_dir = Path::new(&procedure_dir).join("reports");

    // Create directory if it doesn't exist
    if !reports_dir.exists() {
        std::fs::create_dir_all(&reports_dir)
            .map_err(|e| format!("Failed to create reports directory: {}", e))?;
    }

    // Open in system file explorer
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
async fn open_run_directory(procedure_dir: String, run_dir: String) -> Result<(), String> {
    use std::path::Path;

    let reports_dir = Path::new(&procedure_dir).join("reports").join(&run_dir);

    if !reports_dir.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    // Open in system file explorer
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(reports_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
async fn delete_test_run(procedure_dir: String, run_dir: String) -> Result<(), String> {
    use std::fs;
    use std::path::Path;

    let run_path = Path::new(&procedure_dir).join("reports").join(&run_dir);

    if !run_path.exists() {
        return Err(format!("Report directory not found: {}", run_dir));
    }

    fs::remove_dir_all(&run_path)
        .map_err(|e| format!("Failed to delete report directory: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn watch_yaml_file(path: String, app_handle: AppHandle) -> Result<(), String> {
    use std::fs;
    use tokio::time::{interval, Duration};

    // Get or initialize the watched files map
    let watched = WATCHED_YAML_FILES.get_or_init(|| Mutex::new(HashMap::new()));

    let mut files = watched.lock().await;
    if files.contains_key(&path) {
        // Already watching this file, skip duplicate setup
        return Ok(());
    }

    execution::cli_output::debug(format!("Setting up YAML file watcher for: {}", path));

    let path_clone = path.clone();

    // Get initial modification time
    let initial_modified = fs::metadata(&path)
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .modified()
        .map_err(|e| format!("Failed to get modification time: {}", e))?;

    // Spawn a background task to check for changes
    let handle = tokio::spawn(async move {
        let mut last_modified = initial_modified;
        let mut ticker = interval(Duration::from_secs(1)); // Check every second

        loop {
            ticker.tick().await;

            // Check if file still exists and get modification time
            match fs::metadata(&path_clone) {
                Ok(metadata) => {
                    if let Ok(modified) = metadata.modified() {
                        if modified != last_modified {
                            execution::cli_output::verbose(format!("YAML file changed: {}", path_clone));
                            last_modified = modified;

                            // Emit event to frontend
                            let _ = app_handle.emit("yaml-file-changed", &path_clone);
                        }
                    }
                }
                Err(e) => {
                    // File doesn't exist anymore (deleted, or directory renamed/moved)
                    execution::cli_output::info(format!("YAML file no longer exists: {} (error: {})", path_clone, e));

                    // Emit event to frontend that file was deleted/moved
                    let _ = app_handle.emit("yaml-file-deleted", &path_clone);

                    // Remove from watched files
                    if let Some(watched) = WATCHED_YAML_FILES.get() {
                        let mut files = watched.lock().await;
                        files.remove(&path_clone);
                    }

                    break;
                }
            }
        }
    });

    // Store the abort handle so we can stop watching later
    files.insert(path, handle.abort_handle());

    Ok(())
}

#[tauri::command]
async fn unwatch_yaml_file(path: String) -> Result<(), String> {
    execution::cli_output::debug(format!("Removing YAML file watcher for: {}", path));

    if let Some(watched) = WATCHED_YAML_FILES.get() {
        let mut files = watched.lock().await;
        if let Some(abort_handle) = files.remove(&path) {
            abort_handle.abort();
            execution::cli_output::debug(format!("Aborted file watcher for: {}", path));
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditorSettings {
    pub sequential_dependencies: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            sequential_dependencies: true,
        }
    }
}

#[tauri::command]
async fn load_editor_settings(app: AppHandle) -> Result<EditorSettings, String> {
    let config_dir = app.path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config directory: {}", e))?;
    let settings_path = config_dir.join("editor_settings.json");

    if !settings_path.exists() {
        return Ok(EditorSettings::default());
    }

    let contents = tokio::fs::read_to_string(&settings_path)
        .await
        .map_err(|e| format!("Failed to read editor settings: {}", e))?;

    serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse editor settings: {}", e))
}

#[tauri::command]
async fn save_editor_settings(
    app: AppHandle,
    settings: EditorSettings,
) -> Result<(), String> {
    let config_dir = app.path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config directory: {}", e))?;

    tokio::fs::create_dir_all(&config_dir)
        .await
        .map_err(|e| format!("Failed to create config directory: {}", e))?;

    let settings_path = config_dir.join("editor_settings.json");
    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Failed to serialize editor settings: {}", e))?;

    tokio::fs::write(&settings_path, json)
        .await
        .map_err(|e| format!("Failed to write editor settings: {}", e))
}

#[tauri::command]
async fn save_yaml_config(procedure_file: String, config_json: String) -> Result<String, String> {
    use std::fs;
    use validator::Validate;

    // Parse JSON from frontend
    let config: serde_json::Value = serde_json::from_str(&config_json)
        .map_err(|e| format!("Invalid JSON format: {}", e))?;

    execution::cli_output::debug(format!("Saving YAML config: {}", serde_json::to_string_pretty(&config).unwrap_or_default()));

    // Extract setup phases
    let setup_phases: Vec<schema::procedure::PhaseDefinition> = if let Some(setup_array) = config.get("setup").and_then(|v| v.as_array()) {
        setup_array.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    } else {
        Vec::new()
    };

    // Extract main phases
    let main_phases: Vec<schema::procedure::PhaseDefinition> = if let Some(main_array) = config.get("main").and_then(|v| v.as_array()) {
        main_array.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    } else {
        Vec::new()
    };

    // Extract teardown phases
    let teardown_phases: Vec<schema::procedure::PhaseDefinition> = if let Some(teardown_array) = config.get("teardown").and_then(|v| v.as_array()) {
        teardown_array.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    } else {
        Vec::new()
    };

    execution::cli_output::debug(format!("Loaded phases - setup: {}, main: {}, teardown: {}",
        setup_phases.len(), main_phases.len(), teardown_phases.len()));

    // Extract unit config
    let unit_value = config.get("unit");
    let unit: Option<schema::procedure::UnitConfig> = unit_value
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Extract plugs
    let plugs: Vec<schema::procedure::PlugDefinition> = if let Some(plugs_array) = config.get("plugs").and_then(|v| v.as_array()) {
        plugs_array.iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    } else {
        Vec::new()
    };

    // Extract execution config
    let execution_value = config.get("execution");
    let execution: Option<schema::procedure::ExecutionConfig> = execution_value
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Extract id
    let id = config.get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let procedure_yaml = schema::procedure::ProcedureYaml {
        id,
        name: config["name"].as_str().unwrap_or("Untitled").to_string(),
        version: config["version"].as_str().unwrap_or("1.0.0").to_string(),
        description: config.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        execution,
        unit,
        plugs,
        setup: setup_phases,
        main: main_phases,
        teardown: teardown_phases,
    };

    // Convert to ProcedureDefinition for validation
    let procedure_def: schema::procedure::ProcedureDefinition = procedure_yaml.clone().into();
    procedure_def.validate()
        .map_err(|e| format!("Validation failed: {}", e))?;

    // Serialize to YAML
    let mut yaml_content = serde_yaml::to_string(&procedure_yaml)
        .map_err(|e| format!("Failed to serialize YAML: {}", e))?;

    // Add blank lines between main sections for readability
    yaml_content = yaml_content
        .replace("\nexecution:", "\n\nexecution:")
        .replace("\nunit:", "\n\nunit:")
        .replace("\nplugs:", "\n\nplugs:")
        .replace("\nsetup:", "\n\nsetup:")
        .replace("\nmain:", "\n\nmain:")
        .replace("\nphases:", "\n\nphases:")
        .replace("\nteardown:", "\n\nteardown:");

    // Write to file
    fs::write(&procedure_file, yaml_content)
        .map_err(|e| format!("Failed to write file: {}", e))?;

    Ok("Configuration saved successfully".to_string())
}

#[tauri::command]
async fn validate_procedure_config(app_handle: AppHandle, procedure_file: String, procedure_dir: String) -> Result<String, String> {
    use std::fs;

    let procedure_file_buf = PathBuf::from(&procedure_file);
    validate_procedure_file(&procedure_file_buf)?;

    let procedure_dir = PathBuf::from(&procedure_dir);

    let yaml_content = fs::read_to_string(&procedure_file_buf)
        .map_err(|e| format!("Failed to read YAML file: {}", e))?;

    let syntax_validation = validation::validate_yaml_syntax(&yaml_content);
    if !syntax_validation.is_valid {
        return serde_json::to_string(&syntax_validation)
            .map_err(|e| format!("Failed to serialize validation result: {}", e));
    }

    let procedure_def = match loader::load_procedure_definition(&procedure_file_buf) {
        Ok(def) => def,
        Err(e) => {
            let error_result = validation::ValidationResult {
                is_valid: false,
                diagnostics: vec![validation::ValidationDiagnostic::error(
                    "procedure-load-error",
                    format!("Failed to load procedure: {}", e),
                    1,
                    1,
                    10,
                )],
                phase_plugs: std::collections::HashMap::new(),
            };
            return serde_json::to_string(&error_result)
                .map_err(|e2| format!("Failed to serialize validation result: {}", e2));
        }
    };

    let validation_result = validation::validate_procedure_with_yaml(&app_handle, &procedure_def, &yaml_content, &procedure_dir).await;

    serde_json::to_string(&validation_result)
        .map_err(|e| format!("Failed to serialize validation result: {}", e))
}

#[tauri::command]
async fn resize_window_half_screen(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let monitor = window
            .current_monitor()
            .map_err(|e| format!("Failed to get monitor: {}", e))?
            .ok_or("No monitor found")?;
        let size = monitor.size();
        let _ = window.set_size(tauri::LogicalSize::new(size.width / 2, size.height));
        let _ = window.set_position(tauri::LogicalPosition::new(0, 0));
    }
    Ok(())
}

#[tauri::command]
async fn maximize_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.maximize();
    }
    Ok(())
}


#[tauri::command]
async fn submit_native_ui_response(
    request_id: String,
    values: HashMap<String, serde_json::Value>,
    bound_measurements: Option<HashMap<String, serde_json::Value>>,
) -> Result<(), String> {
    use crate::execution::worker::UI_RESPONSE_CHANNELS;

    execution::cli_output::debug(format!(
        "🎯 submit_native_ui_response: request_id={}, bound_measurements={:?}",
        request_id, bound_measurements
    ));

    let mut channels = UI_RESPONSE_CHANNELS.lock().await;

    if let Some(tx) = channels.remove(&request_id) {
        // Convert serde_json::Value to String for channel
        let response: HashMap<String, String> = values
            .into_iter()
            .map(|(k, v)| {
                let string_value = match v {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                        serde_json::to_string(&v).unwrap_or_default()
                    }
                };
                (k, string_value)
            })
            .collect();

        // Add special key for bound measurements
        let mut final_response = response;
        if let Some(measurements) = bound_measurements {
            final_response.insert(
                "__bound_measurements__".to_string(),
                serde_json::to_string(&measurements).unwrap_or_default()
            );
        }

        tx.send(final_response).map_err(|_| "Failed to send response".to_string())?;
        Ok(())
    } else {
        // Python phase - no channel exists
        execution::cli_output::debug(format!(
            "📝 UI submission for Python phase (no channel): {}",
            request_id
        ));
        Ok(())
    }
}





#[tauri::command]
async fn scan_available_plugs(procedure_file: String, procedure_dir: String) -> Result<Vec<serde_json::Value>, String> {
    let procedure_file_path = PathBuf::from(&procedure_file);
    let procedure_dir = PathBuf::from(&procedure_dir);
    let plugs_dir = procedure_dir.join("plugs/python");
    let mut available_plugs = Vec::new();
    
    // Read Python files in the plugs directory
    if plugs_dir.exists() {
        let entries = std::fs::read_dir(&plugs_dir)
            .map_err(|e| format!("Failed to read plugs directory: {}", e))?;
        
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
            let path = entry.path();
            
            if path.extension().and_then(|s| s.to_str()) == Some("py") && 
               path.file_stem().and_then(|s| s.to_str()) != Some("__init__") {
                
                let file_name = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                
                // Read the Python file to extract class definitions
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
                
                // Simple parsing to find class definitions
                for line in content.lines() {
                    if line.starts_with("class ") && line.contains(":") {
                        let class_name = line[6..]
                            .split('(')
                            .next()
                            .unwrap_or("")
                            .split(':')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        
                        if !class_name.is_empty() {
                            // Try to extract docstring and methods
                            let mut description = String::new();
                            let mut methods = Vec::new();
                            let mut in_class = false;
                            let mut found_docstring = false;
                            let mut class_indent = 0;
                            
                            for line in content.lines() {
                                if line.starts_with(&format!("class {}", class_name)) {
                                    in_class = true;
                                    class_indent = line.len() - line.trim_start().len();
                                } else if in_class && !found_docstring {
                                    if line.trim().starts_with("\"\"\"") {
                                        description = line.trim()
                                            .trim_start_matches("\"\"\"")
                                            .trim_end_matches("\"\"\"")
                                            .to_string();
                                        found_docstring = true;
                                    }
                                } else if in_class {
                                    // Check for method definitions
                                    let line_indent = line.len() - line.trim_start().len();
                                    if line_indent > class_indent && line.trim().starts_with("def ") {
                                        let method_def = line.trim().trim_start_matches("def ");
                                        if let Some(paren_pos) = method_def.find('(') {
                                            let method_name = method_def[..paren_pos].trim();
                                            
                                            // Skip private methods and constructor
                                            if !method_name.starts_with('_') {
                                                // Extract parameters
                                                if let Some(close_paren) = method_def.find(')') {
                                                    let params_str = &method_def[paren_pos + 1..close_paren];
                                                    let params: Vec<String> = params_str
                                                        .split(',')
                                                        .filter_map(|p| {
                                                            let p = p.trim();
                                                            if !p.is_empty() && p != "self" {
                                                                // Remove type hints and default values for simplicity
                                                                Some(p.split(':').next()
                                                                    .unwrap_or(p)
                                                                    .split('=').next()
                                                                    .unwrap_or(p)
                                                                    .trim()
                                                                    .to_string())
                                                            } else {
                                                                None
                                                            }
                                                        })
                                                        .collect();
                                                    
                                                    methods.push(serde_json::json!({
                                                        "name": method_name,
                                                        "params": params
                                                    }));
                                                }
                                            }
                                        }
                                    } else if line_indent <= class_indent && line.trim().starts_with("class ") {
                                        // Found next class, stop processing
                                        break;
                                    }
                                }
                            }
                            
                            available_plugs.push(serde_json::json!({
                                "module": format!("plugs.python.{}", file_name),
                                "class": class_name,
                                "description": description,
                                "methods": methods,
                                "language": "python"
                            }));
                        }
                    }
                }
            }
        }
    }

    if procedure_file_path.exists() {
        let yaml_content = std::fs::read_to_string(&procedure_file_path)
            .map_err(|e| format!("Failed to read procedure file: {}", e))?;
        
        if let Ok(procedure_raw) = serde_yaml::from_str::<schema::ProcedureYaml>(&yaml_content) {
            let procedure_def = schema::ProcedureDefinition::from(procedure_raw);
            for plug_config in &procedure_def.plugs {
                // Check if this plug is already in our list
                let module = plug_config.python.get_module();
                let class = plug_config.python.get_callable_name();

                let already_exists = available_plugs.iter().any(|p| {
                    p.get("module").and_then(|m| m.as_str()) == Some(module.as_str()) &&
                    p.get("class").and_then(|c| c.as_str()) == Some(class.as_str())
                });

                // Only include Python plugs for now (check if module starts with "plugs/python" or "plugs.python")
                let is_python = module.starts_with("plugs/python") || module.starts_with("plugs.python");

                if !already_exists && is_python {
                    available_plugs.push(serde_json::json!({
                        "name": plug_config.name,
                        "module": module,
                        "class": class,
                        "configured": true,
                        "language": "python"
                    }));
                }
            }
        }
    }
    
    Ok(available_plugs)
}

#[tauri::command]
async fn list_plug_services(
    standalone_state: State<'_, StandalonePlugServiceState>,
    orchestrator_state: State<'_, OrchestratorState>,
) -> Result<Vec<String>, String> {
    // First, try to get services from any running orchestrator
    let orchestrators = orchestrator_state.orchestrators.lock().await;
    execution::cli_output::debug(format!("🔍 list_plug_services: Found {} orchestrator(s)", orchestrators.len()));
    
    // Collect all services from all orchestrators
    let mut all_services = Vec::new();
    
    for (id, orchestrator_arc) in orchestrators.iter() {
        execution::cli_output::debug(format!("🔍 Checking orchestrator: {}", id));
        let orchestrator = orchestrator_arc.lock().await;
        let resource_manager = orchestrator.resource_manager.read().await;
        let service_manager = resource_manager.get_plug_service_manager();
        let services = service_manager.list_services().await;
        execution::cli_output::debug(format!("🔍 Orchestrator {} has {} service(s): {:?}", id, services.len(), services));
        for service in services {
            if !all_services.contains(&service) {
                all_services.push(service);
            }
        }
    }
    
    // If we found services in orchestrators, return them
    if !all_services.is_empty() {
        execution::cli_output::debug(format!("🔍 Returning {} orchestrator service(s)", all_services.len()));
        return Ok(all_services);
    }
    
    // Fallback to standalone managers
    execution::cli_output::debug("🔍 Checking standalone managers...");
    let managers = standalone_state.resource_managers.lock().await;
    for (_, resource_manager) in managers.iter() {
        let service_manager = resource_manager.get_plug_service_manager();
        let services = service_manager.list_services().await;
        execution::cli_output::debug(format!("🔍 Standalone manager has {} service(s): {:?}", services.len(), services));
        for service in services {
            if !all_services.contains(&service) {
                all_services.push(service);
            }
        }
    }

    Ok(all_services)
}

#[tauri::command]
async fn teardown_stuck_plugs() -> Result<Vec<u16>, String> {
    execution::cli_output::verbose("🧹 Manual teardown of stuck plug processes requested");

    // Kill all plug processes
    plugs::plug_service::kill_all_plug_processes()?;

    // Wait for processes to die
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    execution::cli_output::success("Teardown successful - all plug processes terminated");

    Ok(vec![])
}

#[tauri::command]
async fn read_procedure_file(procedure_file: String) -> Result<String, String> {
    use std::fs;

    let path = Path::new(&procedure_file);
    validate_procedure_file(path)?;

    fs::read_to_string(path)
        .map_err(|e| format!("Failed to read YAML file: {}", e))
}

#[tauri::command]
async fn write_procedure_file(procedure_file: String, content: String) -> Result<(), String> {
    use std::fs;

    let path = Path::new(&procedure_file);
    validate_procedure_file(path)?;

    fs::write(path, content)
        .map_err(|e| format!("Failed to write YAML file: {}", e))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PythonEditorContext {
    Phase { key: String },
    Plug { key: String },
    Function { module_path: String, function_name: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct PythonFileResult {
    content: String,
    file_path: String,
}

#[tauri::command]
async fn read_python_file(procedure_file: String, procedure_dir: String, context: PythonEditorContext) -> Result<PythonFileResult, String> {
    use std::fs;

    let procedure_file_buf = PathBuf::from(&procedure_file);
    validate_procedure_file(&procedure_file_buf)?;

    let project_path = PathBuf::from(&procedure_dir);

    let procedure_def = loader::load_procedure_definition(&procedure_file_buf)
        .map_err(|e| format!("Failed to load procedure: {}", e))?;

    let file_path = match context {
        PythonEditorContext::Phase { key } => {
            // Find phase by key
            let phase = procedure_def.get_flat_phases()
                .into_iter()
                .find(|p| p.key == key)
                .ok_or_else(|| format!("Phase with key '{}' not found", key))?;

            let python_spec = phase.python
                .ok_or_else(|| format!("Phase '{}' does not have Python configuration", key))?;

            let (file_path, _callable) = python_spec.parse(&project_path)?;
            file_path
        }
        PythonEditorContext::Plug { key } => {
            // Find plug by key
            let plug = procedure_def.plugs
                .iter()
                .find(|p| &p.key == &key)
                .ok_or_else(|| format!("Plug with key '{}' not found", key))?;

            let (file_path, _callable) = plug.python.parse(&project_path)?;
            file_path
        }
        PythonEditorContext::Function { module_path, .. } => {
            // Direct module path
            project_path.join(&module_path.replace('.', "/")).with_extension("py")
        }
    };

    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read Python file: {}", e))?;

    Ok(PythonFileResult {
        content,
        file_path: file_path.to_string_lossy().to_string(),
    })
}

#[tauri::command]
async fn write_python_file(file_path: String, content: String) -> Result<(), String> {
    use std::fs;

    fs::write(&file_path, content)
        .map_err(|e| format!("Failed to write Python file: {}", e))
}

#[tauri::command]
async fn read_run_report(procedure_dir: String, run_dir: String) -> Result<String, String> {
    use std::fs;
    use std::path::Path;

    let file_path = Path::new(&procedure_dir).join("reports").join(&run_dir).join("report.json");

    if !file_path.exists() {
        return Err(format!("Report file not found: {}", file_path.display()));
    }

    fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read report file: {}", e))
}

#[derive(Debug, Serialize, Deserialize)]
struct PythonCallable {
    name: String,
    r#type: String,
}

#[tauri::command]
async fn analyze_python_file(
    app_handle: AppHandle,
    procedure_dir: String,
    file_path: String
) -> Result<Vec<PythonCallable>, String> {
    use std::process::Command as StdCommand;

    let file_path_obj = std::path::Path::new(&file_path);
    let procedure_dir_obj = std::path::Path::new(&procedure_dir);

    if !file_path_obj.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    let canonical_file = file_path_obj.canonicalize()
        .map_err(|e| format!("Failed to resolve file path: {}", e))?;
    let canonical_procedure = procedure_dir_obj.canonicalize()
        .map_err(|e| format!("Failed to resolve project directory: {}", e))?;

    if !canonical_file.starts_with(&canonical_procedure) {
        return Err(format!("File must be inside the project directory.\n\nFile: {}\nProject: {}",
            canonical_file.display(),
            canonical_procedure.display()));
    }

    // Resolve Python executable
    let python_path = resolve_python_executable(
        Some(&app_handle),
        std::path::Path::new(&procedure_dir)
    ).await.map_err(|e| {
        format!("Failed to resolve Python for file analysis:\n\n{}", e)
    })?;

    let python_script = r#"
import ast
import sys
import json

def analyze_python_file(file_path):
    with open(file_path, 'r') as f:
        content = f.read()

    try:
        tree = ast.parse(content)
    except SyntaxError as e:
        return []

    callables = []

    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef):
            if not node.name.startswith('_'):
                callables.append({
                    'name': node.name,
                    'type': 'function'
                })
        elif isinstance(node, ast.ClassDef):
            if not node.name.startswith('_'):
                callables.append({
                    'name': node.name,
                    'type': 'class'
                })

    return callables

if __name__ == '__main__':
    file_path = sys.argv[1]
    result = analyze_python_file(file_path)
    print(json.dumps(result))
"#;

    let mut cmd = StdCommand::new(&python_path);
    cmd.arg("-c")
        .arg(python_script)
        .arg(&file_path);

    utils::configure_no_window(&mut cmd);

    let output = cmd.output()
        .map_err(|e| format!("Failed to execute Python: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Python analysis failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let callables: Vec<PythonCallable> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse Python analysis result: {}", e))?;

    Ok(callables)
}

#[tauri::command]
fn to_python_identifier_text(text: String) -> String {
    utils::to_python_identifier(&text)
}

#[tauri::command]
fn read_pyproject(procedure_dir: String) -> Result<python::manifest::PythonManifest, String> {
    python::manifest::read_pyproject(std::path::Path::new(&procedure_dir))
}

#[tauri::command]
fn update_python_version(procedure_dir: String, version: String) -> Result<(), String> {
    python::manifest::update_python_version(std::path::Path::new(&procedure_dir), &version)
}

#[tauri::command]
fn update_dependencies(procedure_dir: String, dependencies: Vec<String>) -> Result<(), String> {
    python::manifest::update_dependencies(std::path::Path::new(&procedure_dir), dependencies)
}

#[tauri::command]
fn update_pyproject_metadata(
    procedure_dir: String,
    name: Option<String>,
    version: Option<String>,
) -> Result<(), String> {
    python::manifest::update_project_metadata(
        std::path::Path::new(&procedure_dir),
        name,
        version,
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
            log::info!("Single instance: argv={:?}, deep link event was already triggered", argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            use tauri::Manager;


            // Maximize the main window on startup
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
            }

            // Setup deep link handler for auth
            use tauri_plugin_deep_link::DeepLinkExt;
            let app_handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    log::info!("Deep link received: {}", url);

                    if url.as_str().starts_with("tofupilot://auth") {
                        let _ = app_handle.emit("deep-link:auth", url.as_str());
                    }
                }
            });

            // Register deep link and track status
            let deep_link_state = app.state::<DeepLinkState>();

            #[cfg(target_os = "macos")]
            {
                // macOS uses static Info.plist, schemes registered via config
                tauri::async_runtime::block_on(async {
                    let mut registered = deep_link_state.is_registered.lock().await;
                    *registered = true;
                });
            }

            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                // Windows/Linux: register at runtime for dev/testing
                let registration_result = app.deep_link().register_all();
                let is_registered = registration_result.is_ok();

                if let Err(e) = registration_result {
                    log::warn!("Failed to register deep link: {}", e);
                }

                tauri::async_runtime::block_on(async {
                    let mut registered = deep_link_state.is_registered.lock().await;
                    *registered = is_registered;
                });
            }

            Ok(())
        })
        .manage(DeepLinkState::default())
        .manage(OrchestratorState::default())
        .manage(UIResponseState::default())
        .manage(StandalonePlugServiceState::default())
        .manage(Arc::new(system_monitor::SystemMonitor::new()))
        .invoke_handler(tauri::generate_handler![
            get_system_info,
            is_deep_link_registered,
            python::environment::get_venv_info,
            python::environment::check_venv_packages,
            python::resolution::resolve_python_for_project,
            get_procedure_context,
            get_procedure_metadata,
            execute_parallel_runs,
            get_execution_stats,
            stop_execution,
            submit_unit_input,
            kill_execution,
            stop_slot,
            load_procedure_config,
            python::environment::create_virtual_environment,
            python::environment::sync_pyproject_dependencies,
            python::environment::manual_sync_pyproject_dependencies,
            get_test_runs,
            get_test_run,
            mark_run_uploaded,
            mark_run_upload_failed,
            get_run_attachments,
            get_attachment_info,
            open_attachment,
            open_reports_folder,
            open_run_directory,
            delete_test_run,
            watch_yaml_file,
            unwatch_yaml_file,
            save_yaml_config,
            validate_procedure_config,
            resize_window_half_screen,
            maximize_window,
            submit_native_ui_response,
            scan_available_plugs,
            list_plug_services,
            teardown_stuck_plugs,
            read_procedure_file,
            write_procedure_file,
            read_python_file,
            write_python_file,
            read_run_report,
            analyze_python_file,
            to_python_identifier_text,
            read_pyproject,
            update_python_version,
            update_dependencies,
            update_pyproject_metadata,
            load_editor_settings,
            save_editor_settings,
            system_monitor::start_monitoring,
            system_monitor::stop_monitoring,
            system_monitor::get_metrics_history
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
