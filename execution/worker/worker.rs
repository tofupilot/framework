use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::RwLock;
use tonic::transport::Channel;

use super::grpc::{worker_client::WorkerClient, *};
use crate::execution::job::{Job, JobResult};
use crate::execution::reports::ReportManager;
use crate::grpc_process::GrpcProcess;

// gRPC type conversions
impl From<&crate::features::operator_ui::UiConfig> for UiConfig {
    fn from(ui_config: &crate::features::operator_ui::UiConfig) -> Self {
        UiConfig {
            components: ui_config.components.iter().map(Into::into).collect(),
        }
    }
}

impl From<&crate::features::operator_ui::UiComponent> for UiComponent {
    fn from(c: &crate::features::operator_ui::UiComponent) -> Self {
        UiComponent {
            key: c.key.clone(),
            component_type: serde_json::to_string(&c.component_type)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            label: c.label.clone(),
            value_json: c
                .value
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default()),
        }
    }
}

/// Convert gRPC Measurement to internal Measurement
/// Returns None if value_json cannot be parsed
fn try_measurement_from_grpc(m: Measurement) -> Option<crate::measurements::Measurement> {
    let value = serde_json::from_str(&m.value_json).ok()?;
    let aggregations = m
        .aggregations_json
        .as_ref()
        .and_then(|json| serde_json::from_str(json).ok());
    Some(crate::measurements::Measurement {
        name: m.name,
        value,
        unit: m.unit,
        timestamp: m.timestamp,
        validators: None,
        aggregations,
        description: None,
    })
}

impl From<LogEntry> for crate::execution::log::LogEntry {
    fn from(l: LogEntry) -> Self {
        crate::execution::log::LogEntry {
            timestamp: l.timestamp,
            level: l.level,
            message: l.message,
            file: l.file,
            line: l.line,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Worker {
    pub id: usize,
    inner: Arc<RwLock<Option<GrpcProcess<WorkerClient<Channel>>>>>,
    procedure_dir: PathBuf,
}

impl Worker {
    pub fn new(id: usize, procedure_dir: PathBuf) -> Self {
        Self {
            id,
            inner: Arc::new(RwLock::new(None)),
            procedure_dir,
        }
    }

    fn find_worker_script_cli() -> Result<PathBuf, String> {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;
        let exe_dir = exe_path
            .parent()
            .ok_or("Failed to get executable directory")?;

        let script_path = exe_dir.join("python").join("tp_worker.py");
        if script_path.exists() {
            return Ok(script_path);
        }

        Err(format!(
            "tp_worker.py not found at {}. Ensure the Python resources are built.",
            script_path.display()
        ))
    }

    pub async fn start(&mut self, app_handle: Option<&AppHandle>) -> Result<(), String> {
        let abs_procedure_dir = self
            .procedure_dir
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize procedure dir: {}", e))?;

        let python_cmd =
            crate::python::resolve_python_internal(app_handle, &abs_procedure_dir).await?;

        self.start_with_python(app_handle, &python_cmd).await
    }

    pub async fn start_with_python(
        &mut self,
        app_handle: Option<&AppHandle>,
        python_cmd: &str,
    ) -> Result<(), String> {
        let abs_procedure_dir = self
            .procedure_dir
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize procedure dir: {}", e))?;

        let worker_script = if let Some(handle) = app_handle {
            handle
                .path()
                .resolve("python/tp_worker.py", tauri::path::BaseDirectory::Resource)
                .map_err(|e| format!("Failed to resolve tp_worker.py: {}", e))?
        } else {
            Self::find_worker_script_cli()?
        };

        log::debug!(
            "Worker {} using gRPC script: {}",
            self.id,
            worker_script.display()
        );

        let worker_id = self.id;

        let process = GrpcProcess::spawn(
            python_cmd,
            worker_script,
            vec![abs_procedure_dir.to_string_lossy().to_string()],
            Some(&abs_procedure_dir),
            vec![("WORKER_ID".to_string(), worker_id.to_string())],
            Some(Box::new(move |stderr| {
                Self::spawn_stderr_reader_static(worker_id, stderr);
            })),
            |port| async move {
                let channel = crate::grpc_process::connect_grpc_channel(port).await?;
                Ok(WorkerClient::new(channel))
            },
        )
        .await?;

        log::debug!("Worker {} gRPC port: {}", self.id, process.port);

        let mut inner = self.inner.write().await;
        *inner = Some(process);

        Ok(())
    }

    /// Helper to execute operation on report manager(s) based on job slot
    /// If job has slot_id, operates on single manager. Otherwise operates on all.
    async fn with_report_managers<F>(
        managers_arc: &Arc<RwLock<HashMap<String, ReportManager>>>,
        job_slot_id: Option<&String>,
        job_id: &str,
        mut operation: F,
    ) where
        F: FnMut(&str, &mut ReportManager) -> Result<(), String>,
    {
        let mut managers = managers_arc.write().await;

        if let Some(slot_id) = job_slot_id {
            if let Some(manager) = managers.get_mut(slot_id) {
                if let Err(e) = operation(slot_id, manager) {
                    log::warn!(
                        "Operation failed for job {} slot {}: {}",
                        job_id,
                        slot_id,
                        e
                    );
                }
            }
        } else {
            for (slot_id, manager) in managers.iter_mut() {
                if let Err(e) = operation(slot_id, manager) {
                    log::warn!(
                        "Operation failed for job {} slot {}: {}",
                        job_id,
                        slot_id,
                        e
                    );
                }
            }
        }
    }

    pub async fn execute_python_phase(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        app_handle: Option<AppHandle>,
        report_managers: Option<Arc<RwLock<HashMap<String, ReportManager>>>>,
    ) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();

        // Emit UI request if phase has components
        let has_ui = !job.ui_config.components.is_empty();
        let requires_user_input = job.ui_config.requires_user_input();

        let ui_response_rx = if has_ui && requires_user_input {
            let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());

            let (tx, rx) = tokio::sync::oneshot::channel();
            {
                let mut channels = crate::features::operator_ui::UI_RESPONSE_CHANNELS.lock().await;
                channels.insert(request_id.clone(), tx);
            }

            if let Some(app) = &app_handle {
                let event_data = crate::features::operator_ui::UiRequestData {
                    request_id: request_id.clone(),
                    job_id: job.id.to_string(),
                    pipe_path: String::new(),
                    config: job.ui_config.clone(),
                    phase_key: job.phase_key.clone(),
                    slot_id: job.slot_id.clone(),
                };

                if let Err(e) = crate::features::operator_ui::UiRequestEvent(event_data).emit(app) {
                    log::warn!("Failed to emit UI request for Python phase: {}", e);
                } else {
                    log::debug!("📺 Sent UI request {} for Python phase {}", request_id, job.phase_name);
                }
            }

            Some((request_id, rx))
        } else if has_ui && !requires_user_input {
            // Display-only UI, emit but don't wait
            if let Some(app) = &app_handle {
                let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());
                let event_data = crate::features::operator_ui::UiRequestData {
                    request_id: request_id.clone(),
                    job_id: job.id.to_string(),
                    pipe_path: String::new(),
                    config: job.ui_config.clone(),
                    phase_key: job.phase_key.clone(),
                    slot_id: job.slot_id.clone(),
                };

                let _ = crate::features::operator_ui::UiRequestEvent(event_data).emit(app);
            }
            None
        } else {
            None
        };

        // Build unit_info for gRPC if available
        let grpc_unit_info = job.initial_unit_info.as_ref().map(|ui| {
            super::grpc::UnitInfo {
                serial_number: ui.serial_number.clone(),
                part_number: ui.part_number.clone(),
                revision_number: ui.revision_number.clone(),
                batch_number: ui.batch_number.clone(),
                sub_units: ui.sub_units.clone().unwrap_or_default(),
            }
        });

        // Build gRPC command
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
                .map(|(k, v)| {
                    (
                        crate::python::identifier::to_python_identifier(&k),
                        format!("127.0.0.1:{}", v),
                    )
                })
                .collect(),
            ui_config: Some(self.convert_ui_config(&job.ui_config)),
            timeout_ms: job.timeout_ms,
            retry_count: job.retry_count as u32,
            retry_limit: job.retry_limit as u32,
            unit_info: grpc_unit_info,
            phase_results: job.phase_results.clone(),
        };

        let mut client = {
            let inner = self.inner.read().await;
            inner
                .as_ref()
                .ok_or("gRPC worker not started")?
                .client
                .clone()
        };

        // Call ExecutePhase RPC
        let mut stream = client
            .execute_phase(command)
            .await
            .map_err(|e| format!("gRPC ExecutePhase failed: {}", e))?
            .into_inner();

        // Process streaming responses
        while let Some(event) = stream
            .message()
            .await
            .map_err(|e| format!("gRPC stream error: {}", e))?
        {
            use worker_event::Event;

            match event.event {
                Some(Event::JobComplete(grpc_result)) => {
                    // Check phase result to determine if we should wait for UI
                    let phase_result = grpc_result
                        .phase_result_json
                        .as_ref()
                        .and_then(|json| serde_json::from_str(json).ok())
                        .and_then(|pr: crate::features::operator_ui::PythonPhaseResult| {
                            crate::execution::job::PhaseResult::from_python_result(&pr).ok()
                        })
                        .unwrap_or(crate::execution::job::PhaseResult::Continue);

                    let is_terminal = matches!(
                        phase_result,
                        crate::execution::job::PhaseResult::Skip
                            | crate::execution::job::PhaseResult::Stop
                            | crate::execution::job::PhaseResult::Fail
                    ) || grpc_result.error.is_some();

                    let mut ui_unit_info: Option<crate::execution::types::UnitInfo> = None;
                    let mut ui_bound_measurements: Option<HashMap<String, serde_json::Value>> = None;
                    if let Some((request_id, mut rx)) = ui_response_rx {
                        // Check if UI was already submitted before Python finished
                        match rx.try_recv() {
                            Ok(ui_values) => {
                                // UI was submitted before Python completed — use the data
                                log::debug!("UI already submitted for phase {}", job.phase_name);
                                if let Some((unit_info, bound)) = extract_bound_measurements(&ui_values) {
                                    ui_unit_info = unit_info;
                                    ui_bound_measurements = Some(bound);
                                }
                            }
                            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                if is_terminal {
                                    // Terminal result and UI not yet submitted — dismiss UI
                                    log::debug!(
                                        "Python phase {} returned terminal result {:?}, dismissing UI",
                                        job.phase_name, phase_result
                                    );
                                    drop(rx);
                                    let mut channels = crate::features::operator_ui::UI_RESPONSE_CHANNELS.lock().await;
                                    channels.remove(&request_id);
                                } else {
                                    // Continue result — wait for operator to submit
                                    log::debug!("Python phase {} finished, waiting for UI submission", job.phase_name);
                                    match rx.await {
                                        Ok(ui_values) => {
                                            log::debug!("Received UI submission for phase {}", job.phase_name);
                                            if let Some((unit_info, bound)) = extract_bound_measurements(&ui_values) {
                                                ui_unit_info = unit_info;
                                                ui_bound_measurements = Some(bound);
                                            }
                                        }
                                        Err(_) => {
                                            log::warn!("UI response channel closed for phase {}", job.phase_name);
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                                log::warn!("UI response channel closed for phase {}", job.phase_name);
                            }
                        }
                    }

                    let end_time = chrono::Utc::now();

                    // Save phase measurements before job is consumed by convert_job_result
                    let phase_measurements = job.phase_measurements.clone();

                    // Convert gRPC result to JobResult
                    let mut job_result = self.convert_job_result(grpc_result, start_time, end_time, job)?;

                    // Merge UI bound measurements: UI fills in what Python didn't set,
                    // Python wins on conflicts (same measurement name)
                    if let Some(bound) = ui_bound_measurements {
                        let existing_names: std::collections::HashSet<String> = job_result
                            .measurements
                            .iter()
                            .map(|m| m.name.clone())
                            .collect();
                        let bound_measurements = convert_bound_to_measurements(bound);

                        // Evaluate bound measurements with YAML definitions (validators, units, etc.)
                        let phase_config = crate::procedure::schema::PhaseDefinition {
                            measurements: phase_measurements,
                            key: String::new(),
                            name: String::new(),
                            scope: None,
                            python: None,
                            executable: None,
                            description: None,
                            depends_on: Vec::new(),
                            ui: None,
                            enabled: true,
                            result: None,
                            timeout: None,
                            retry: None,
                            then: None,
                        };
                        let evaluated_bound = crate::measurements::auto_evaluate_measurements(bound_measurements, &phase_config);

                        for m in evaluated_bound {
                            if !existing_names.contains(&m.name) {
                                job_result.measurements.push(m);
                            }
                        }
                    }

                    // Merge UI unit info if present
                    if let Some(ui_unit) = ui_unit_info {
                        job_result.unit = Some(merge_unit_info(job_result.unit, ui_unit));
                    }

                    return Ok(job_result);
                }
                Some(Event::Error(err)) => {
                    return Err(err.message);
                }
                Some(Event::AttachFile(attach_event)) => {
                    if let Some(ref managers_arc) = report_managers {
                        let source_path = attach_event.source_path.clone();
                        let attachment_name = attach_event.attachment_name.clone();
                        let job_id = job.id.to_string();

                        Self::with_report_managers(
                            managers_arc,
                            job.slot_id.as_ref(),
                            &job_id,
                            |_slot_id, manager| {
                                manager
                                    .attach_file(&job.id, Path::new(&source_path), &attachment_name)
                                    .map_err(|e| {
                                        format!("Failed to attach file {}: {}", attachment_name, e)
                                    })
                            },
                        )
                        .await;
                    }
                }
                Some(Event::AttachData(attach_event)) => {
                    if let Some(ref managers_arc) = report_managers {
                        // Decode base64 data once before passing to managers
                        match base64::engine::general_purpose::STANDARD.decode(&attach_event.data) {
                            Ok(bytes) => {
                                let attachment_name = attach_event.attachment_name.clone();
                                let job_id = job.id.to_string();

                                Self::with_report_managers(
                                    managers_arc,
                                    job.slot_id.as_ref(),
                                    &job_id,
                                    |_slot_id, manager| {
                                        manager
                                            .attach_data(&job.id, &bytes, &attachment_name)
                                            .map_err(|e| {
                                                format!(
                                                    "Failed to attach data {}: {}",
                                                    attachment_name, e
                                                )
                                            })
                                    },
                                )
                                .await;
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to decode base64 for {}: {}",
                                    attach_event.attachment_name,
                                    e
                                );
                            }
                        }
                    }
                }
                Some(Event::UiUpdate(ui_event)) => {
                    if let Some(app) = app_handle.as_ref() {
                        let update_event = crate::execution::events::UiUpdateEvent {
                            job_id: job.id.to_string(),
                            slot_id: job.slot_id.as_deref().unwrap_or("<shared>").to_string(),
                            phase_key: job.phase_key.clone(),
                            worker_id: self.id,
                            action: ui_event.action.clone(),
                            data: serde_json::from_str(&ui_event.data_json).unwrap_or_default(),
                        };

                        let _ = update_event.emit(app);
                    }
                }
                None => {
                    return Err("Empty event received from worker".to_string());
                }
            }
        }

        Err("Worker stream ended without job completion".to_string())
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        let mut inner = self.inner.write().await;

        if let Some(ref mut process) = *inner {
            let mut client = process.client.clone();
            let result = process
                .graceful_shutdown(
                    || async move {
                        let _ = client.shutdown(Empty {}).await;
                        Ok(())
                    },
                    5,
                )
                .await;

            // Take the process out after shutdown completes (it's dead now)
            // This prevents double-killing and marks it as cleaned up
            // If this future was cancelled before reaching here, kill_on_drop handles cleanup
            inner.take();

            result
        } else {
            Ok(())
        }
    }

    pub async fn force_shutdown(&mut self) -> Result<(), String> {
        let mut inner = self.inner.write().await;

        if let Some(ref mut process) = *inner {
            let result = process.force_kill().await;

            // Take the process out after kill (it's dead now)
            inner.take();

            result
        } else {
            Ok(())
        }
    }

    /// Convert internal UiConfig to gRPC UiConfig using From trait
    fn convert_ui_config(&self, ui_config: &crate::features::operator_ui::UiConfig) -> UiConfig {
        ui_config.into()
    }

    /// Convert gRPC JobResult to internal JobResult
    /// Handles phase result parsing, measurement/log conversion, and unit info
    fn convert_job_result(
        &self,
        result: super::grpc::JobResult,
        start_time: chrono::DateTime<chrono::Utc>,
        end_time: chrono::DateTime<chrono::Utc>,
        job: Job,
    ) -> Result<crate::execution::job::JobResult, String> {
        use crate::execution::job::PhaseResult;

        // Parse phase result from JSON
        let phase_result = result
            .phase_result_json
            .as_ref()
            .and_then(|json| serde_json::from_str(json).ok())
            .and_then(|pr| PhaseResult::from_python_result(&pr).ok())
            .unwrap_or(PhaseResult::Continue);

        // Convert measurements, filtering invalid entries
        let measurements: Vec<crate::measurements::Measurement> = result
            .measurements
            .into_iter()
            .filter_map(try_measurement_from_grpc)
            .collect();

        // Build phase config for validator evaluation
        let phase_config = crate::procedure::schema::PhaseDefinition {
            measurements: job.phase_measurements.clone(),
            key: String::new(),
            name: String::new(),
            scope: None,
            python: None,
            executable: None,
            description: None,
            depends_on: Vec::new(),
            ui: None,
            enabled: true,
            result: None,
            timeout: None,
            retry: None,
            then: None,
        };

        // Evaluate measurements and merge YAML validators
        let evaluated_measurements = crate::measurements::auto_evaluate_measurements(measurements, &phase_config);

        // Convert logs using From trait
        let logs = result.logs.into_iter().map(Into::into).collect();

        // Parse unit info from JSON if present
        let unit = result.unit_json.and_then(|json| {
            match serde_json::from_str(&json) {
                Ok(u) => Some(u),
                Err(e) => {
                    log::warn!("Failed to parse unit_json: {} (json: {})", e, json);
                    None
                }
            }
        });

        Ok(crate::execution::job::JobResult {
            phase_result,
            phase_outcome: crate::execution::job::Outcome::PENDING_COMPLETION,
            next_action: None,
            timeout_secs: None,
            error: result.error,
            exit_code: result.exit_code,
            measurements: evaluated_measurements,
            logs,
            started_at: start_time,
            completed_at: end_time,
            resource_metrics: Default::default(),
            unit,
            input_unit_info: job.initial_unit_info.clone(),
            retry_count: job.retry_count,
        })
    }

    pub async fn interrupt_current_job(&mut self) -> Result<(), String> {
        self.force_shutdown().await
    }

    pub async fn shutdown_with_timeout(&mut self, timeout_ms: u64) -> Result<(), String> {
        let shutdown_result = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            self.shutdown(),
        )
        .await;

        match shutdown_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => self.force_shutdown().await,
        }
    }

    fn spawn_stderr_reader_static(worker_id: usize, stderr: ChildStderr) {
        tokio::spawn(async move {
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while stderr_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    log::warn!("Worker {} Python stderr: {}", worker_id, trimmed);
                }
                line.clear();
            }
        });
    }

    pub async fn execute_job(
        &self,
        job: Job,
        plug_ports: HashMap<String, u16>,
        app_handle: Option<AppHandle>,
        report_managers: Option<Arc<RwLock<HashMap<String, ReportManager>>>>,
    ) -> Result<JobResult, String> {
        log::debug!(
            "🎮 Worker {} executing {} phase: {}",
            self.id,
            match job.runtime_type {
                crate::execution::job::RuntimeType::Native => "native Rust",
                crate::execution::job::RuntimeType::Python => "Python",
                crate::execution::job::RuntimeType::Shell => "shell",
            },
            job.phase_name
        );

        match job.runtime_type {
            crate::execution::job::RuntimeType::Native => self.execute_native_phase(job, app_handle).await,
            crate::execution::job::RuntimeType::Python => {
                self.execute_python_phase(job, plug_ports, app_handle, report_managers)
                    .await
            }
            crate::execution::job::RuntimeType::Shell => self.execute_shell_phase(job).await,
        }
    }

    pub async fn execute_shell_phase(&self, job: Job) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();
        let mut logs = Vec::new();

        let command = job
            .command
            .as_ref()
            .ok_or_else(|| "No command specified for shell phase".to_string())?;

        let working_dir = crate::execution::runtime::shell::resolve_working_directory(
            job.working_directory.as_deref(),
            job.procedure_dir.as_deref(),
        );

        if !working_dir.exists() {
            return Err(format!(
                "Working directory does not exist: {}",
                working_dir.display()
            ));
        }

        let shell_builder = crate::execution::runtime::shell::ShellCommandBuilder::new(job.shell_type.as_deref())?;
        let shell_type = shell_builder.shell_type().to_string();

        logs.push(crate::execution::log::LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!("🐚 Executing command with {}: {}", shell_type, command),
            file: None,
            line: None,
        });

        logs.push(crate::execution::log::LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: "INFO".to_string(),
            message: format!("📁 Working directory: {}", working_dir.display()),
            file: None,
            line: None,
        });

        let mut resource_tracker = crate::execution::monitoring::ResourceTracker::new();

        let child = shell_builder
            .command(command)
            .working_dir(&working_dir)
            .with_stdio(
                std::process::Stdio::piped(),
                std::process::Stdio::piped(),
                std::process::Stdio::piped(),
            )
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!(
                        "Shell '{}' not found. Make sure it's installed and in PATH.",
                        shell_type
                    )
                } else {
                    format!("Failed to execute command with {}: {}", shell_type, e)
                }
            })?;

        let pid = child.id();
        resource_tracker.start_tracking(pid);

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("Failed to wait for command: {}", e))?;

        if !output.stdout.is_empty() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                logs.push(crate::execution::log::LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "INFO".to_string(),
                    message: line.to_string(),
                    file: None,
                    line: None,
                });
            }
        }

        if !output.stderr.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                logs.push(crate::execution::log::LogEntry {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    level: "ERROR".to_string(),
                    message: line.to_string(),
                    file: None,
                    line: None,
                });
            }
        }

        let shell_exit_code = output.status.code();
        let (phase_result, error) = if output.status.success() {
            logs.push(crate::execution::log::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "INFO".to_string(),
                message: "✅ Command succeeded with exit code 0".to_string(),
                file: None,
                line: None,
            });
            (crate::execution::job::PhaseResult::Continue, None)
        } else {
            let exit_code = shell_exit_code.unwrap_or(-1);
            logs.push(crate::execution::log::LogEntry {
                timestamp: chrono::Utc::now().to_rfc3339(),
                level: "ERROR".to_string(),
                message: format!("❌ Command failed with exit code {}", exit_code),
                file: None,
                line: None,
            });
            (crate::execution::job::PhaseResult::Fail, None)
        };

        let end_time = chrono::Utc::now();

        let resource_metrics = resource_tracker.collect_metrics();

        Ok(JobResult {
            phase_result,
            phase_outcome: crate::execution::job::Outcome::PENDING_COMPLETION,
            next_action: None,
            timeout_secs: None,
            error,
            exit_code: shell_exit_code,
            measurements: Vec::new(),
            logs,
            started_at: start_time,
            completed_at: end_time,
            resource_metrics,
            unit: None,
            input_unit_info: job.initial_unit_info.clone(),
            retry_count: job.retry_count,
        })
    }

    pub async fn execute_native_phase(
        &self,
        job: Job,
        app_handle: Option<AppHandle>,
    ) -> Result<JobResult, String> {
        let start_time = chrono::Utc::now();

        let mut resource_tracker = crate::execution::monitoring::ResourceTracker::new();
        resource_tracker.start_tracking(None);

        let has_ui = !job.ui_config.components.is_empty();
        let requires_user_input = job.ui_config.requires_user_input();

        let ui_response_rx = if has_ui {
            let request_id = format!("{}_{}", job.id, chrono::Utc::now().timestamp_millis());

            let ui_response_rx = if requires_user_input {
                let (tx, rx) = tokio::sync::oneshot::channel();
                {
                    let mut channels = crate::features::operator_ui::UI_RESPONSE_CHANNELS.lock().await;
                    channels.insert(request_id.clone(), tx);
                }

                log::debug!(
                    "Created UI response channel for native phase: {}",
                    request_id
                );
                Some(rx)
            } else {
                None
            };

            if let Some(app) = &app_handle {
                let event_data = crate::features::operator_ui::UiRequestData {
                    request_id: request_id.clone(),
                    job_id: job.id.to_string(),
                    pipe_path: String::new(),
                    config: job.ui_config.clone(),
                    phase_key: job.phase_key.clone(),
                    slot_id: job.slot_id.clone(),
                };

                if let Err(e) = crate::features::operator_ui::UiRequestEvent(event_data).emit(app) {
                    log::debug!(
                        "Failed to emit UI request: {}",
                        e
                    );
                }

                log::debug!(
                    "📺 Sent UI request {} for native phase {}",
                    request_id, job.phase_name
                );
            }

            ui_response_rx
        } else {
            None
        };

        let mut bound_measurements_to_merge: Option<HashMap<String, serde_json::Value>> = None;
        let mut unit_info: Option<crate::execution::types::UnitInfo> = None;

        let ui_result = if has_ui && requires_user_input {
            if let Some(rx) = ui_response_rx {
                match rx.await {
                    Ok(ui_values) => {
                        if let Some((ui_unit, bound)) = extract_bound_measurements(&ui_values) {
                            unit_info = ui_unit;
                            bound_measurements_to_merge = Some(bound);
                        }

                        (crate::execution::job::PhaseResult::Continue, None)
                    }
                    Err(_) => {
                        // Channel closed = phase was cancelled (e.g. on_first_failure: stop)
                        (crate::execution::job::PhaseResult::Stop, None)
                    }
                }
            } else {
                (
                    crate::execution::job::PhaseResult::Continue,
                    Some("No UI response channel available".to_string()),
                )
            }
        } else {
            (crate::execution::job::PhaseResult::Continue, None)
        };

        let (phase_result, execution_error) = ui_result;

        let end_time = chrono::Utc::now();

        let phase_config = crate::procedure::schema::PhaseDefinition {
            measurements: job.phase_measurements.clone(),
            key: String::new(),
            name: String::new(),
            scope: None,
            python: None,
            executable: None,
            description: None,
            depends_on: Vec::new(),
            ui: None,
            enabled: true,
            result: None,
            timeout: None,
            retry: None,
            then: None,
        };

        let mut all_measurements = Vec::new();
        if let Some(bound) = bound_measurements_to_merge {
            all_measurements = convert_bound_to_measurements(bound);
        }

        let evaluated_measurements = crate::measurements::auto_evaluate_measurements(all_measurements, &phase_config);

        let resource_metrics = resource_tracker.collect_metrics();

        Ok(JobResult {
            phase_result,
            phase_outcome: crate::execution::job::Outcome::PENDING_COMPLETION,
            next_action: None,
            timeout_secs: None,
            error: execution_error,
            exit_code: None,
            measurements: evaluated_measurements,
            logs: Vec::new(),
            started_at: start_time,
            completed_at: end_time,
            resource_metrics,
            unit: unit_info,
            input_unit_info: job.initial_unit_info.clone(),
            retry_count: job.retry_count,
        })
    }
}

fn extract_unit_info_from_json(
    unit_data: &serde_json::Map<String, serde_json::Value>,
) -> crate::execution::types::UnitInfo {
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
        if let Some(obj) = v.as_object() {
            let parsed: std::collections::HashMap<String, String> = obj
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
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

    crate::execution::types::UnitInfo {
        serial_number,
        batch_number,
        part_number,
        revision_number,
        sub_units,
        status: "tested".to_string(),
    }
}

fn extract_bound_measurements(
    ui_values: &HashMap<String, String>,
) -> Option<(Option<crate::execution::types::UnitInfo>, HashMap<String, serde_json::Value>)> {
    let bound_json = ui_values.get("__bound_measurements__")?;
    let mut bound: HashMap<String, serde_json::Value> =
        serde_json::from_str(bound_json).ok()?;

    let unit_info = if let Some(unit_value) = bound.remove("__unit__") {
        // Try to parse as object directly, or as JSON string
        let unit_data_opt = match &unit_value {
            serde_json::Value::Object(obj) => Some(obj.clone()),
            serde_json::Value::String(s) => serde_json::from_str(s).ok(),
            _ => None,
        };

        unit_data_opt.map(|unit_data| extract_unit_info_from_json(&unit_data))
    } else {
        None
    };

    Some((unit_info, bound))
}

fn convert_bound_to_measurements(
    bound: HashMap<String, serde_json::Value>,
) -> Vec<crate::measurements::Measurement> {
    bound
        .into_iter()
        .map(|(name, value)| {
            let measurement_value = match value {
                serde_json::Value::Null => crate::measurements::MeasurementValue::Null,
                serde_json::Value::Bool(b) => crate::measurements::MeasurementValue::Boolean(b),
                serde_json::Value::Number(n) => crate::measurements::MeasurementValue::Numeric(n.as_f64().unwrap_or(0.0)),
                serde_json::Value::String(s) => crate::measurements::MeasurementValue::String(s),
                serde_json::Value::Array(arr) => crate::measurements::MeasurementValue::Array(arr),
                serde_json::Value::Object(obj) => crate::measurements::MeasurementValue::Object(obj),
            };

            crate::measurements::Measurement {
                name: name.clone(),
                value: measurement_value,
                unit: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                validators: None,
                aggregations: None,
                description: None,
            }
        })
        .collect()
}

/// Merge UI unit info with existing unit info
/// UI values take precedence for sub_units, but are merged with existing values
fn merge_unit_info(
    existing: Option<crate::execution::types::UnitInfo>,
    ui_unit: crate::execution::types::UnitInfo,
) -> crate::execution::types::UnitInfo {
    match existing {
        Some(mut base) => {
            // Merge sub_units: UI values take precedence
            if let Some(ui_sub_units) = ui_unit.sub_units {
                let mut merged_sub_units = base.sub_units.unwrap_or_default();
                for (key, value) in ui_sub_units {
                    merged_sub_units.insert(key, value);
                }
                base.sub_units = Some(merged_sub_units);
            }
            // UI values override if present
            if ui_unit.serial_number.is_some() {
                base.serial_number = ui_unit.serial_number;
            }
            if ui_unit.part_number.is_some() {
                base.part_number = ui_unit.part_number;
            }
            if ui_unit.revision_number.is_some() {
                base.revision_number = ui_unit.revision_number;
            }
            if ui_unit.batch_number.is_some() {
                base.batch_number = ui_unit.batch_number;
            }
            base
        }
        None => ui_unit,
    }
}
