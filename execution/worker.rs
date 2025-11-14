use base64::Engine;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::execution::job::{Job, JobResult, PhaseResult, RuntimeType};
use crate::execution::LogEntry;
use crate::execution::runs::RunManager;
use crate::execution::worker_ipc::IpcHandler;
use crate::execution::worker_process::ProcessManager;
pub use crate::execution::worker_types::*;
use crate::measurements::auto_evaluate_measurements;
use crate::schema::procedure::PhaseDefinition;

// Global registry for UI response channels
pub static UI_RESPONSE_CHANNELS: Lazy<
    Arc<Mutex<HashMap<String, oneshot::Sender<HashMap<String, String>>>>>,
> = Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

#[derive(Debug, Clone)]
pub struct Worker {
    pub id: usize,
    inner: Arc<RwLock<WorkerInner>>,
    procedure_dir: PathBuf,
    process_pid: Arc<Mutex<Option<u32>>>,
}

#[derive(Debug)]
struct WorkerInner {
    process_manager: ProcessManager,
    ipc_handler: IpcHandler,
}

impl Worker {
    pub fn new(id: usize, procedure_dir: PathBuf) -> Self {
        Self {
            id,
            inner: Arc::new(RwLock::new(WorkerInner {
                process_manager: ProcessManager::new(id),
                ipc_handler: IpcHandler::new(),
            })),
            procedure_dir,
            process_pid: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        let mut inner = self.inner.write().await;

        // Start the process
        inner
            .process_manager
            .start(&self.procedure_dir, app_handle)
            .await?;

        // Cache the PID for lock-free access during force kill
        if let Some(pid) = inner.process_manager.get_pid() {
            *self.process_pid.lock().await = Some(pid);
        }

        // Get stdin and stdout from process manager
        let stdin = inner.process_manager.take_stdin();
        let stdout = inner.process_manager.take_stdout();

        // Setup IPC with the process
        inner.ipc_handler.connect(stdin, stdout)?;

        // Wait for ready signal
        log::debug!("Worker {} waiting for Ready signal from Python", self.id);

        let response: WorkerResponse = inner.ipc_handler.receive_json().await.map_err(|e| {
            log::error!("Worker {} failed to receive Ready signal: {}", self.id, e);
            format!("Worker {} failed to receive Ready signal: {}", self.id, e)
        })?;

        match response {
            WorkerResponse::Ready => {
                log::debug!("Worker {} received Ready signal", self.id);
                Ok(())
            }
            WorkerResponse::Error { message } => {
                log::error!("Worker {} startup error: {}", self.id, message);
                Err(format!("Worker {} failed to start: {}", self.id, message))
            }
            _ => {
                log::error!("Worker {} received unexpected response: {:?}", self.id, response);
                Err(format!("Worker {} received unexpected response: {:?}", self.id, response))
            }
        }
    }

    pub async fn execute_job(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        app_handle: Option<AppHandle>,
        report_managers: Option<Arc<RwLock<HashMap<String, RunManager>>>>,
    ) -> Result<JobResult, String> {
        crate::execution::cli_output::debug(format!(
            "🎮 Worker {} executing {} phase: {}",
            self.id,
            match job.runtime_type {
                RuntimeType::Native => "native Rust",
                RuntimeType::Python => "Python",
                RuntimeType::Shell => "shell",
            },
            job.phase_name
        ));

        // Route to appropriate runtime based on type
        match job.runtime_type {
            RuntimeType::Native => self.execute_native_phase(job, app_handle).await,
            RuntimeType::Python => {
                self.execute_python_phase(job, plug_ports, app_handle, report_managers)
                    .await
            }
            RuntimeType::Shell => self.execute_shell_phase(job).await,
        }
    }

    async fn execute_python_phase(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        app_handle: Option<AppHandle>,
        report_managers: Option<Arc<RwLock<HashMap<String, RunManager>>>>,
    ) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();

        crate::execution::cli_output::debug(format!(
            "🔧 execute_python_phase for job {} phase '{}' with {} UI components",
            job.id,
            job.phase_name,
            job.ui_config.components.len()
        ));

        // Step 1: Display UI immediately if configured (before Python starts)
        let has_ui = !job.ui_config.components.is_empty();

        // Prepare UI response channel for manual input phases
        let (_, ui_response_rx) = if has_ui {
            let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());

            crate::execution::cli_output::debug(format!(
                "📺 Displaying UI immediately for job {} with {} components",
                job.id,
                job.ui_config.components.len()
            ));

            let requires_user_input = job.ui_config.requires_user_input();

            // For manual input phases, create the response channel immediately
            let ui_response_rx = if requires_user_input {
                let (tx, rx) = tokio::sync::oneshot::channel();
                {
                    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
                    channels.insert(request_id.clone(), tx);
                }
                crate::execution::cli_output::debug(format!(
                    "Created UI response channel for early submissions: {}",
                    request_id
                ));
                Some(rx)
            } else {
                None
            };

            if let Some(app) = app_handle.as_ref() {
                // GUI mode - emit UI request event (frontend expects this)

                let event_data = serde_json::json!({
                    "request_id": request_id,
                    "job_id": job.id.to_string(),
                    "slot_id": job.slot_id.as_deref().unwrap_or("<shared>"),
                    "worker_id": self.id,
                    "phase_key": job.phase_key.clone(),
                    "phase_name": job.phase_name.clone(),
                    "stage_scope": job.stage_scope.clone(),
                    "data": job.ui_config,  // Frontend expects "data" not "ui_config"
                    "timeout_ms": job.timeout_ms,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "requires_input": requires_user_input
                });

                let _ = app.emit("ui-request", &event_data);
            } else {
                // CLI mode - display UI in terminal
                crate::execution::cli_output::print_section(
                    crate::execution::cli_output::Section::Phase,
                    format!(
                        "UI: {}",
                        job.ui_config
                            .instructions
                            .as_deref()
                            .unwrap_or("No instructions")
                    ),
                );

                // Display components info
                for component in &job.ui_config.components {
                    let label = component.label.as_deref().unwrap_or(&component.key);
                    let value = component
                        .value
                        .as_ref()
                        .map(|v| match v {
                            crate::execution::ui_types::ComponentValue::Boolean(b) => b.to_string(),
                            crate::execution::ui_types::ComponentValue::Number(n) => n.to_string(),
                            crate::execution::ui_types::ComponentValue::String(s) => s.clone(),
                        })
                        .unwrap_or_else(|| "waiting...".to_string());
                    crate::execution::cli_output::print_section(
                        crate::execution::cli_output::Section::Phase,
                        format!("  • {} ({}): {}", label, component.key, value),
                    );
                }
            }

            (Some(request_id), ui_response_rx) // Return both request_id and receiver
        } else {
            (None, None) // No UI
        };

        // Step 2: Start Python process (in parallel with UI)
        {
            let mut inner = self.inner.write().await;

            // Start resource tracking for this job
            inner.process_manager.start_resource_tracking();

            // Send job command to Python
            crate::execution::cli_output::debug(format!(
                "DEBUG: Sending job to Python with plug_ports: {:?}",
                plug_ports
            ));

            let command = JobCommand {
                job_id: job.id.to_string(),
                slot_id: job
                    .slot_id
                    .clone()
                    .unwrap_or_else(|| "<shared>".to_string()),
                phase_name: job.phase_name.clone(),
                module: job.module.clone(),
                function: job.function.clone(),
                plugs: plug_ports
                    .into_iter()
                    .map(|(k, v)| (crate::utils::to_python_identifier(&k), format!("127.0.0.1:{}", v)))
                    .collect(),
                ui_config: job.ui_config.clone(),
                timeout_ms: job.timeout_ms,
                retry_count: job.retry_count,
                retry_limit: job.retry_limit,
            };

            inner.ipc_handler.send_json(&command).await?;
        }

        // Step 3: Process Python messages until completion

        loop {
            let response: WorkerResponse = {
                let mut inner = self.inner.write().await;
                match inner.ipc_handler.receive_json().await {
                    Ok(resp) => resp,
                    Err(e) => {
                        return Err(format!("Worker {} IPC error: {}", self.id, e));
                    }
                }
            };

            match response {
                WorkerResponse::JobComplete { result } => {
                    // Check if this job has UI - if so, we need to wait for user input
                    let has_ui = !job.ui_config.components.is_empty();
                    let requires_user_input = job.ui_config.requires_user_input();

                    // Use the receiver that was created when UI was displayed
                    // This handles early submissions that happened before Python finished
                    let mut bound_measurements_to_merge: Option<
                        HashMap<String, serde_json::Value>,
                    > = None;
                    let mut unit_info: Option<crate::UnitInfo> = None;

                    if has_ui && requires_user_input {
                        // Job has UI requiring manual input - wait for user response
                        crate::execution::cli_output::debug(format!(
                            "Job {} has UI requiring manual input - Python finished, waiting for user",
                            job.id
                        ));

                        // Python is done, now wait for UI response using the channel
                        // that was created when UI was first displayed
                        if let Some(rx) = ui_response_rx {
                            match rx.await {
                                Ok(ui_values) => {
                                    // Extract bound measurements for later merging
                                    if let Some(bound_json) =
                                        ui_values.get("__bound_measurements__")
                                    {
                                        if let Ok(mut bound) = serde_json::from_str::<
                                            HashMap<String, serde_json::Value>,
                                        >(
                                            bound_json
                                        ) {
                                            crate::execution::cli_output::debug(format!(
                                                "📊 Extracted {} bound items from UI submission",
                                                bound.len()
                                            ));

                                            // Extract __unit__ data if present
                                            if let Some(unit_value) = bound.remove("__unit__") {
                                                if let Ok(unit_data) = serde_json::from_value::<
                                                    serde_json::Map<String, serde_json::Value>,
                                                >(
                                                    unit_value
                                                ) {
                                                    let serial_number = unit_data
                                                        .get("serial_number")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from);
                                                    let batch_number = unit_data
                                                        .get("batch_number")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from);
                                                    let part_number = unit_data
                                                        .get("part_number")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from);
                                                    let revision_number = unit_data
                                                        .get("revision_number")
                                                        .and_then(|v| v.as_str())
                                                        .map(String::from);

                                                    let sub_units =
                                                        unit_data.get("sub_units").and_then(|v| {
                                                            if let Some(arr) = v.as_array() {
                                                                let parsed: Vec<String> = arr
                                                                    .iter()
                                                                    .filter_map(|item| {
                                                                        item.as_str()
                                                                            .map(String::from)
                                                                    })
                                                                    .collect();
                                                                if parsed.is_empty() {
                                                                    None
                                                                } else {
                                                                    Some(parsed)
                                                                }
                                                            } else {
                                                                None
                                                            }
                                                        });

                                                    unit_info = Some(crate::UnitInfo {
                                                        serial_number,
                                                        batch_number,
                                                        part_number,
                                                        revision_number,
                                                        sub_units,
                                                        status: "tested".to_string(),
                                                    });
                                                    crate::execution::cli_output::debug(format!(
                                                        "📦 Extracted unit info from UI: serial={:?}, batch={:?}, part={:?}, revision={:?}, sub_units={}",
                                                        unit_info.as_ref().and_then(|u| u.serial_number.as_ref()),
                                                        unit_info.as_ref().and_then(|u| u.batch_number.as_ref()),
                                                        unit_info.as_ref().and_then(|u| u.part_number.as_ref()),
                                                        unit_info.as_ref().and_then(|u| u.revision_number.as_ref()),
                                                        unit_info.as_ref().and_then(|u| u.sub_units.as_ref().map(|s| s.len())).unwrap_or(0)
                                                    ));
                                                }
                                            }

                                            bound_measurements_to_merge = Some(bound);
                                        }
                                    }
                                }
                                Err(_) => {
                                    // Channel closed (timeout or cancellation) - still complete with Python result
                                    crate::execution::cli_output::debug(format!(
                                    "Job {} UI response channel closed, completing with Python result",
                                    job.id
                                ));
                                }
                            }
                        } else {
                            // This shouldn't happen - we should have created a receiver
                            crate::execution::cli_output::error(
                                "Expected UI response receiver but found none",
                            );
                        }
                    }

                    // Extract and merge unit information from Python result
                    // Strategy: Start with Python values, then UI values override
                    if result.unit.is_some() {
                        if let Some(unit_value) = result.unit {
                            if let Ok(py_unit_data) = serde_json::from_value::<
                                serde_json::Map<String, serde_json::Value>,
                            >(unit_value)
                            {
                                let py_serial = py_unit_data
                                    .get("serial_number")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                let py_batch = py_unit_data
                                    .get("batch_number")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                let py_part = py_unit_data
                                    .get("part_number")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                let py_revision = py_unit_data
                                    .get("revision_number")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                let py_sub_units = py_unit_data.get("sub_units").and_then(|v| {
                                    if let Some(arr) = v.as_array() {
                                        let parsed: Vec<String> = arr
                                            .iter()
                                            .filter_map(|item| item.as_str().map(String::from))
                                            .collect();
                                        if parsed.is_empty() {
                                            None
                                        } else {
                                            Some(parsed)
                                        }
                                    } else {
                                        None
                                    }
                                });

                                // Merge: UI overrides Python (field by field)
                                if let Some(ref mut ui_unit) = unit_info {
                                    // UI exists, merge Python values for fields that UI didn't set
                                    if ui_unit.serial_number.is_none() {
                                        ui_unit.serial_number = py_serial.clone();
                                    }
                                    if ui_unit.batch_number.is_none() {
                                        ui_unit.batch_number = py_batch.clone();
                                    }
                                    if ui_unit.part_number.is_none() {
                                        ui_unit.part_number = py_part.clone();
                                    }
                                    if ui_unit.revision_number.is_none() {
                                        ui_unit.revision_number = py_revision.clone();
                                    }
                                    if ui_unit.sub_units.is_none() {
                                        ui_unit.sub_units = py_sub_units.clone();
                                    }
                                    crate::execution::cli_output::debug(
                                        "📦 Merged Python unit info with UI unit info",
                                    );
                                } else {
                                    // No UI unit, use Python values
                                    unit_info = Some(crate::UnitInfo {
                                        serial_number: py_serial,
                                        batch_number: py_batch,
                                        part_number: py_part,
                                        revision_number: py_revision,
                                        sub_units: py_sub_units,
                                        status: "tested".to_string(),
                                    });
                                    crate::execution::cli_output::debug(format!(
                                        "📦 Set unit info from Python: serial={:?}, part={:?}",
                                        unit_info.as_ref().and_then(|u| u.serial_number.as_ref()),
                                        unit_info.as_ref().and_then(|u| u.part_number.as_ref()),
                                    ));
                                }
                            }
                        }
                    }

                    // Either no UI, or auto-continue enabled - complete immediately
                    let end_time = chrono::Utc::now();

                    // Parse phase action from Python return value
                    let phase_result = if let Some(ref phase_value) = result.phase_result {
                        crate::execution::cli_output::debug(format!(
                            "Received phase_result from Python: {:?}",
                            phase_value
                        ));
                        let parsed =
                            PhaseResult::from_python_result(phase_value).unwrap_or_else(|e| {
                                crate::execution::cli_output::verbose(format!(
                                    "Warning: Failed to parse phase action: {}. Using Continue.",
                                    e
                                ));
                                PhaseResult::Continue
                            });
                        crate::execution::cli_output::debug(format!(
                            "Parsed as PhaseResult: {:?}",
                            parsed
                        ));
                        parsed
                    } else {
                        crate::execution::cli_output::debug(
                            "No phase_result from Python, using Continue",
                        );
                        PhaseResult::Continue // None = Continue in OpenHTF
                    };

                    // Extract error if execution failed
                    let execution_error = if !result.success && result.error.is_some() {
                        Some(result.error.unwrap())
                    } else {
                        None
                    };

                    // Collect resource metrics from the process
                    let resource_metrics = {
                        let mut inner = self.inner.write().await;
                        inner.process_manager.collect_resource_metrics()
                    };

                    // Create a temporary PhaseDefinition for auto-evaluation
                    let phase_config = PhaseDefinition {
                        measurements: job.phase_measurements.clone(),
                        // Other fields are not needed for evaluation
                        key: String::new(),
                        name: String::new(),
                        scope: None,
                        python: None,
                        executable: None,
                        description: String::new(),
                        depends_on: Vec::new(),
                        ui: Default::default(),
                        enabled: true,
                        result: None,
                        timeout: None,
                        retry: None,
                        then: None,
                    };

                    // Merge bound measurements from UI into result
                    let mut all_measurements = result.measurements;
                    if let Some(bound) = bound_measurements_to_merge {
                        use crate::measurements::{Measurement, MeasurementValue};

                        crate::execution::cli_output::debug(format!(
                            "🔗 Merging {} bound measurements into result",
                            bound.len()
                        ));

                        for (name, value) in bound {
                            // Convert serde_json::Value to MeasurementValue
                            let measurement_value = match value {
                                serde_json::Value::Null => MeasurementValue::Null,
                                serde_json::Value::Bool(b) => MeasurementValue::Boolean(b),
                                serde_json::Value::Number(n) => {
                                    MeasurementValue::Numeric(n.as_f64().unwrap_or(0.0))
                                }
                                serde_json::Value::String(s) => MeasurementValue::String(s),
                                serde_json::Value::Array(arr) => MeasurementValue::Array(arr),
                                serde_json::Value::Object(obj) => MeasurementValue::Object(obj),
                            };

                            all_measurements.push(Measurement {
                                name: name.clone(),
                                value: measurement_value,
                                unit: None,
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                validators: None,
                                aggregations: None,
                                description: None,
                            });

                            crate::execution::cli_output::debug(format!(
                                "  OK: Added bound measurement: {}",
                                name
                            ));
                        }
                    }

                    // Auto-evaluate measurements (merges YAML with Python and evaluates validators)
                    let evaluated_measurements =
                        auto_evaluate_measurements(all_measurements, &phase_config);

                    return Ok(JobResult {
                        phase_result,
                        phase_outcome: None, // Will be computed in completion handler
                        next_action: None,   // Will be computed in completion handler
                        timeout_secs: None,  // No timeout occurred
                        error: execution_error,
                        exit_code: result.exit_code,
                        measurements: evaluated_measurements,
                        logs: result.logs,
                        started_at: start_time,
                        completed_at: end_time,
                        resource_metrics,
                        unit: unit_info,
                        retry_count: job.retry_count,
                    });
                }
                WorkerResponse::UIUpdate {
                    job_id: _,
                    action,
                    data,
                } => {
                    // Python is updating a UI component in real-time
                    if let Some(app) = app_handle.as_ref() {
                        // Forward the update to frontend in expected format
                        let update_event = serde_json::json!({
                            "job_id": job.id.to_string(),
                            "slot_id": job.slot_id.as_deref().unwrap_or("<shared>"),
                            "phase_key": job.phase_key,
                            "worker_id": self.id,
                            "action": action,  // e.g., "set_value", "add_log"
                            "data": data,      // Contains component_id and value, or log data
                        });

                        let _ = app.emit("ui-update", &update_event);
                    }
                    // Continue processing
                }
                WorkerResponse::AttachFile {
                    job_id: _python_job_id,
                    source_path,
                    attachment_name,
                } => {
                    // Handle file attachment - use the actual job.id from the Rust side
                    if let Some(ref managers_arc) = report_managers {
                        let mut managers_lock = managers_arc.write().await;
                        if let Some(slot_id) = &job.slot_id {
                            // Slot-specific phase: attach to this slot's manager only
                            if let Some(ref mut manager) = managers_lock.get_mut(slot_id) {
                                if let Err(e) = manager.attach_file(
                                    &job.id,
                                    Path::new(&source_path),
                                    &attachment_name,
                                ) {
                                    crate::execution::cli_output::print_section(
                                        crate::execution::cli_output::Section::Error,
                                        format!(
                                            "WARNING: Failed to attach file {}: {}",
                                            attachment_name, e
                                        ),
                                    );
                                } else {
                                    crate::execution::cli_output::debug(format!(
                                        "📎 Attached file {} to job {}",
                                        attachment_name, job.id
                                    ));
                                }
                            }
                        } else {
                            // Shared phase: attach to ALL slot managers
                            for (slot_id, manager) in managers_lock.iter_mut() {
                                if let Err(e) = manager.attach_file(
                                    &job.id,
                                    Path::new(&source_path),
                                    &attachment_name,
                                ) {
                                    crate::execution::cli_output::print_section(
                                        crate::execution::cli_output::Section::Error,
                                        format!(
                                            "WARNING: Failed to attach file {} to slot {}: {}",
                                            attachment_name, slot_id, e
                                        ),
                                    );
                                } else {
                                    crate::execution::cli_output::debug(format!(
                                        "📎 Attached file {} to job {} (slot {})",
                                        attachment_name, job.id, slot_id
                                    ));
                                }
                            }
                        }
                    }
                    // Continue loop to wait for job completion
                }
                WorkerResponse::AttachData {
                    job_id: _python_job_id,
                    data,
                    attachment_name,
                } => {
                    // Handle data attachment (base64 decoded) - use the actual job.id from the Rust side
                    if let Some(ref managers_arc) = report_managers {
                        let mut managers_lock = managers_arc.write().await;
                        if let Some(slot_id) = &job.slot_id {
                            // Slot-specific phase: attach to this slot's manager only
                            if let Some(ref mut manager) = managers_lock.get_mut(slot_id) {
                                match base64::engine::general_purpose::STANDARD.decode(&data) {
                                    Ok(bytes) => {
                                        if let Err(e) =
                                            manager.attach_data(&job.id, &bytes, &attachment_name)
                                        {
                                            crate::execution::cli_output::print_section(
                                                crate::execution::cli_output::Section::Error,
                                                format!(
                                                    "WARNING: Failed to attach data {}: {}",
                                                    attachment_name, e
                                                ),
                                            );
                                        } else {
                                            crate::execution::cli_output::debug(format!(
                                                "📎 Attached data {} to job {}",
                                                attachment_name, job.id
                                            ));
                                        }
                                    }
                                    Err(e) => {
                                        crate::execution::cli_output::print_section(
                                            crate::execution::cli_output::Section::Error,
                                            format!(
                                                "WARNING: Failed to decode base64 for {}: {}",
                                                attachment_name, e
                                            ),
                                        );
                                    }
                                }
                            }
                        } else {
                            // Shared phase: attach to ALL slot managers
                            match base64::engine::general_purpose::STANDARD.decode(&data) {
                                Ok(bytes) => {
                                    for (slot_id, manager) in managers_lock.iter_mut() {
                                        if let Err(e) =
                                            manager.attach_data(&job.id, &bytes, &attachment_name)
                                        {
                                            crate::execution::cli_output::print_section(
                                                crate::execution::cli_output::Section::Error,
                                                format!(
                                                    "WARNING: Failed to attach data {} to slot {}: {}",
                                                    attachment_name, slot_id, e
                                                ),
                                            );
                                        } else {
                                            crate::execution::cli_output::debug(format!(
                                                "📎 Attached data {} to job {} (slot {})",
                                                attachment_name, job.id, slot_id
                                            ));
                                        }
                                    }
                                }
                                Err(e) => {
                                    crate::execution::cli_output::print_section(
                                        crate::execution::cli_output::Section::Error,
                                        format!(
                                            "WARNING: Failed to decode base64 for {}: {}",
                                            attachment_name, e
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // Continue loop to wait for job completion
                }
                // Removed WaitingForUI and duplicate UIUpdate - handled above
                WorkerResponse::Error { message } => {
                    return Err(message);
                }
                _ => {
                    return Err("Unexpected response from worker".to_string());
                }
            }
        }
    }

    pub async fn interrupt_current_job(&mut self) -> Result<(), String> {
        let mut inner = self.inner.write().await;

        inner.process_manager.send_sigterm()?;

        let command = serde_json::json!({"type": "interrupt"});
        let _ = inner.ipc_handler.send_json(&command).await;

        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        let mut inner = self.inner.write().await;

        // Send shutdown command
        let command = serde_json::json!({"type": "shutdown"});
        inner.ipc_handler.send_json(&command).await?;

        // Shutdown the process
        inner.process_manager.shutdown().await?;

        Ok(())
    }

    pub async fn shutdown_with_timeout(&mut self, timeout_ms: u64) -> Result<(), String> {
        // Try graceful shutdown first
        let shutdown_result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            self.shutdown(),
        )
        .await;

        match shutdown_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout - force kill
                let mut inner = self.inner.write().await;
                inner.process_manager.force_kill().await
            }
        }
    }

    pub async fn force_shutdown(&mut self) -> Result<(), String> {
        let pid = *self.process_pid.lock().await;

        if let Some(_pid) = pid {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid as NixPid;

                let pgid = NixPid::from_raw(-(_pid as i32));
                let _ = kill(pgid, Signal::SIGKILL);
            }

            #[cfg(not(unix))]
            {
                let mut inner = self.inner.write().await;
                let _ = inner.process_manager.force_kill().await;
            }
        }
        Ok(())
    }

    // Native Rust phase executor
    async fn execute_native_phase(
        &self,
        job: Job,
        app_handle: Option<AppHandle>,
    ) -> Result<JobResult, String> {
        use crate::execution::resource_tracker::ResourceTracker;

        let start_time = chrono::Utc::now();
        let mut logs = Vec::new();

        // Start resource tracking (no PID for native phases, will track time only)
        let mut resource_tracker = ResourceTracker::new();
        resource_tracker.start_tracking(None);

        // Native phases are UI-only phases with no Python execution
        logs.push(LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!("🦀 Native UI-only phase: {}", job.phase_name),
            file: None,
            line: None,
        });

        // Check if this phase has UI that requires user input
        let has_ui = !job.ui_config.components.is_empty();
        let requires_user_input = job.ui_config.requires_user_input();

        // Prepare UI response channel and emit UI request for all phases with UI
        let ui_response_rx = if has_ui {
            let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());

            // Create response channel for UI (only needed for manual input phases)
            let ui_response_rx = if requires_user_input {
                let (tx, rx) = tokio::sync::oneshot::channel();
                {
                    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
                    channels.insert(request_id.clone(), tx);
                }

                crate::execution::cli_output::debug(format!(
                    "Created UI response channel for native phase: {}",
                    request_id
                ));
                Some(rx)
            } else {
                None
            };

            // Display UI request for both manual and auto-continue phases
            if let Some(app) = &app_handle {
                // GUI mode - emit UI request event
                let event_data = serde_json::json!({
                    "request_id": request_id,
                    "job_id": job.id.to_string(),
                    "slot_id": job.slot_id.as_deref().unwrap_or("<shared>"),
                    "worker_id": self.id,
                    "phase_key": job.phase_key.clone(),
                    "phase_name": job.phase_name.clone(),
                    "stage_scope": job.stage_scope.clone(),
                    "data": job.ui_config,
                    "timeout_ms": job.timeout_ms,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "requires_input": requires_user_input
                });

                if let Err(e) = app.emit("ui-request", &event_data) {
                    crate::execution::cli_output::debug(format!(
                        "Failed to emit UI request: {}",
                        e
                    ));
                }

                crate::execution::cli_output::debug(format!(
                    "📺 Sent UI request {} for native phase {}",
                    request_id, job.phase_name
                ));
            } else {
                // CLI mode - display UI in terminal
                crate::execution::cli_output::print_section(
                    crate::execution::cli_output::Section::Phase,
                    format!(
                        "UI: {}",
                        job.ui_config
                            .instructions
                            .as_deref()
                            .unwrap_or("No instructions")
                    ),
                );
            }

            ui_response_rx
        } else {
            None
        };

        let mut bound_measurements_to_merge: Option<HashMap<String, serde_json::Value>> = None;
        let mut unit_info: Option<crate::UnitInfo> = None;

        let ui_result = if has_ui && requires_user_input {
            // Phase has UI requiring manual input - wait for user response
            crate::execution::cli_output::print_section(
                crate::execution::cli_output::Section::Phase,
                format!(
                    "Waiting for operator input: {}",
                    job.ui_config
                        .instructions
                        .as_deref()
                        .unwrap_or("Please complete the form")
                ),
            );

            logs.push(LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: "⏳ Waiting for operator response...".to_string(),
                file: None,
                line: None,
            });

            // Wait for UI response using the receiver passed to this function
            if let Some(rx) = ui_response_rx {
                match rx.await {
                    Ok(ui_values) => {
                        logs.push(LogEntry {
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            level: "INFO".to_string(),
                            message: format!(
                                "✅ Received operator response with {} values",
                                ui_values.len()
                            ),
                            file: None,
                            line: None,
                        });

                        // Log the collected values
                        for (key, value) in &ui_values {
                            logs.push(LogEntry {
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                level: "INFO".to_string(),
                                message: format!("  {} = {}", key, value),
                                file: None,
                                line: None,
                            });
                        }

                        // Extract bound measurements for later merging
                        if let Some(bound_json) = ui_values.get("__bound_measurements__") {
                            if let Ok(mut bound) = serde_json::from_str::<
                                HashMap<String, serde_json::Value>,
                            >(bound_json)
                            {
                                crate::execution::cli_output::debug(format!(
                                    "📊 Extracted {} bound items from native UI submission",
                                    bound.len()
                                ));

                                // Extract __unit__ data if present
                                if let Some(unit_value) = bound.remove("__unit__") {
                                    if let Ok(unit_data) =
                                        serde_json::from_value::<
                                            serde_json::Map<String, serde_json::Value>,
                                        >(unit_value)
                                    {
                                        let serial_number = unit_data
                                            .get("serial_number")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);
                                        let batch_number = unit_data
                                            .get("batch_number")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);
                                        let part_number = unit_data
                                            .get("part_number")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);
                                        let revision_number = unit_data
                                            .get("revision_number")
                                            .and_then(|v| v.as_str())
                                            .map(String::from);

                                        let sub_units = unit_data.get("sub_units").and_then(|v| {
                                            if let Some(arr) = v.as_array() {
                                                let parsed: Vec<String> = arr
                                                    .iter()
                                                    .filter_map(|item| {
                                                        item.as_str().map(String::from)
                                                    })
                                                    .collect();
                                                if parsed.is_empty() {
                                                    None
                                                } else {
                                                    Some(parsed)
                                                }
                                            } else {
                                                None
                                            }
                                        });

                                        unit_info = Some(crate::UnitInfo {
                                            serial_number,
                                            batch_number,
                                            part_number,
                                            revision_number,
                                            sub_units,
                                            status: "tested".to_string(),
                                        });
                                        crate::execution::cli_output::debug(format!(
                                            "📦 Extracted unit info from native UI: serial={:?}, batch={:?}, part={:?}, revision={:?}, sub_units={}",
                                            unit_info.as_ref().and_then(|u| u.serial_number.as_ref()),
                                            unit_info.as_ref().and_then(|u| u.batch_number.as_ref()),
                                            unit_info.as_ref().and_then(|u| u.part_number.as_ref()),
                                            unit_info.as_ref().and_then(|u| u.revision_number.as_ref()),
                                            unit_info.as_ref().and_then(|u| u.sub_units.as_ref().map(|s| s.len())).unwrap_or(0)
                                        ));
                                    }
                                }

                                bound_measurements_to_merge = Some(bound);
                            }
                        }

                        (PhaseResult::Continue, None)
                    }
                    Err(_) => {
                        logs.push(LogEntry {
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            level: "ERROR".to_string(),
                            message: "❌ Failed to receive UI response".to_string(),
                            file: None,
                            line: None,
                        });
                        (
                            PhaseResult::Continue,
                            Some("Failed to receive UI response".to_string()),
                        )
                    }
                }
            } else {
                logs.push(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "ERROR".to_string(),
                    message: "❌ No UI response channel available".to_string(),
                    file: None,
                    line: None,
                });
                (
                    PhaseResult::Continue,
                    Some("No UI response channel available".to_string()),
                )
            }
        } else if has_ui && !requires_user_input {
            // Display-only UI - just log and continue
            logs.push(LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: "✅ Display-only UI, auto-continuing phase".to_string(),
                file: None,
                line: None,
            });
            (PhaseResult::Continue, None)
        } else {
            // No UI - just continue
            (PhaseResult::Continue, None)
        };

        let (phase_result, execution_error) = ui_result;

        let end_time = chrono::Utc::now();

        // Create a temporary PhaseDefinition for auto-evaluation
        let phase_config = PhaseDefinition {
            measurements: job.phase_measurements.clone(),
            key: String::new(),
            name: String::new(),
            scope: None,
            python: None,
            executable: None,
            description: String::new(),
            depends_on: Vec::new(),
            ui: Default::default(),
            enabled: true,
            result: None,
            timeout: None,
            retry: None,
            then: None,
        };

        // Merge bound measurements from UI into result
        let mut all_measurements = Vec::new();
        if let Some(bound) = bound_measurements_to_merge {
            use crate::measurements::{Measurement, MeasurementValue};

            crate::execution::cli_output::debug(format!(
                "🔗 Merging {} bound measurements into native phase result",
                bound.len()
            ));

            for (name, value) in bound {
                let measurement_value = match value {
                    serde_json::Value::Null => MeasurementValue::Null,
                    serde_json::Value::Bool(b) => MeasurementValue::Boolean(b),
                    serde_json::Value::Number(n) => {
                        MeasurementValue::Numeric(n.as_f64().unwrap_or(0.0))
                    }
                    serde_json::Value::String(s) => MeasurementValue::String(s),
                    serde_json::Value::Array(arr) => MeasurementValue::Array(arr),
                    serde_json::Value::Object(obj) => MeasurementValue::Object(obj),
                };

                all_measurements.push(Measurement {
                    name: name.clone(),
                    value: measurement_value,
                    unit: None,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    validators: None,
                    aggregations: None,
                    description: None,
                });

                crate::execution::cli_output::debug(format!(
                    "  OK: Added bound measurement: {}",
                    name
                ));
            }
        }

        // Auto-evaluate measurements (merges YAML config and evaluates validators)
        let evaluated_measurements = auto_evaluate_measurements(all_measurements, &phase_config);

        // Collect resource metrics (time only, no process tracking)
        let resource_metrics = resource_tracker.collect_metrics();

        Ok(JobResult {
            phase_result,
            phase_outcome: None, // Will be computed in completion handler
            next_action: None,   // Will be computed in completion handler
            timeout_secs: None,  // No timeout for native UI phases
            error: execution_error,
            exit_code: None,
            measurements: evaluated_measurements,
            logs,
            started_at: start_time,
            completed_at: end_time,
            resource_metrics,
            unit: unit_info,
            retry_count: job.retry_count,
        })
    }

    // Shell command executor
    async fn execute_shell_phase(&self, job: Job) -> Result<JobResult, String> {
        use crate::execution::resource_tracker::ResourceTracker;

        let start_time = chrono::Utc::now();
        let mut logs = Vec::new();

        let command = job
            .command
            .as_ref()
            .ok_or_else(|| "No command specified for shell phase".to_string())?;

        // Determine which shell to use
        let default_shell = if cfg!(target_os = "windows") {
            "powershell"
        } else {
            "sh"
        };

        let shell_name = job.shell_type.as_deref().unwrap_or(default_shell);

        let (shell_exe, shell_flag) = match shell_name {
            "bash" => ("bash", "-c"),
            "sh" => ("sh", "-c"),
            "zsh" => ("zsh", "-c"),
            "powershell" => {
                if cfg!(target_os = "windows") {
                    ("powershell", "-Command")
                } else {
                    ("pwsh", "-Command")
                }
            }
            "pwsh" => ("pwsh", "-Command"),
            "cmd" => ("cmd", "/C"),
            _ => {
                return Err(format!("Unsupported shell type: {}", shell_name));
            }
        };

        logs.push(LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!("🐚 Executing command with {}: {}", shell_exe, command),
            file: None,
            line: None,
        });

        // Build command
        let mut cmd = tokio::process::Command::new(shell_exe);
        cmd.args([shell_flag, command])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Determine working directory
        let working_dir = if let Some(ref dir) = job.working_directory {
            // User specified a working directory
            let path = std::path::Path::new(dir);
            if path.is_absolute() {
                // Absolute path - use as is
                path.to_path_buf()
            } else {
                // Relative path - resolve from procedure directory
                if let Some(ref proc_dir) = job.procedure_dir {
                    std::path::Path::new(proc_dir).join(path)
                } else {
                    // Fallback to current dir if no procedure_dir
                    path.to_path_buf()
                }
            }
        } else {
            // No working_directory specified - use procedure directory as default
            if let Some(ref proc_dir) = job.procedure_dir {
                std::path::PathBuf::from(proc_dir)
            } else {
                // Fallback to current dir (shouldn't happen in practice)
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            }
        };

        // Validate working directory exists
        if !working_dir.exists() {
            return Err(format!(
                "Working directory does not exist: {}",
                working_dir.display()
            ));
        }

        logs.push(LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!("📁 Working directory: {}", working_dir.display()),
            file: None,
            line: None,
        });

        cmd.current_dir(&working_dir);

        crate::utils::configure_no_window_tokio(&mut cmd);

        // Start resource tracking
        let mut resource_tracker = ResourceTracker::new();

        // Spawn command to track PID
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Shell '{}' not found. Make sure it's installed and in PATH.",
                    shell_exe
                )
            } else {
                format!("Failed to execute command with {}: {}", shell_exe, e)
            }
        })?;

        // Start tracking resources with the process PID
        let pid = child.id();
        resource_tracker.start_tracking(pid);

        // Wait for process to complete and collect output
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("Failed to wait for command: {}", e))?;

        // Log stdout if present
        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                logs.push(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "INFO".to_string(),
                    message: format!("📤 {}", line),
                    file: None,
                    line: None,
                });
            }
        }

        // Log stderr if present
        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                logs.push(LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "WARN".to_string(),
                    message: format!("WARNING: {}", line),
                    file: None,
                    line: None,
                });
            }
        }

        // Determine phase action and error based on exit code
        let shell_exit_code = output.status.code();
        let (phase_result, error) = if output.status.success() {
            logs.push(LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: "✅ Command succeeded with exit code 0".to_string(),
                file: None,
                line: None,
            });
            (PhaseResult::Continue, None)
        } else {
            let exit_code = shell_exit_code.unwrap_or(-1);
            logs.push(LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "ERROR".to_string(),
                message: format!("❌ Command failed with exit code {}", exit_code),
                file: None,
                line: None,
            });
            (PhaseResult::Fail, None)
        };

        let end_time = chrono::Utc::now();

        // Collect resource metrics
        let resource_metrics = resource_tracker.collect_metrics();

        Ok(JobResult {
            phase_result,
            phase_outcome: None, // Will be computed in completion handler
            next_action: None,   // Will be computed in completion handler
            timeout_secs: None,  // No timeout for shell phases
            error,
            exit_code: shell_exit_code,
            measurements: Vec::new(),
            logs,
            started_at: start_time,
            completed_at: end_time,
            resource_metrics,
            unit: None,
            retry_count: job.retry_count,
        })
    }
}
