//! Per-plug service management
//! Each plug instance runs in its own isolated service process

use crate::execution::log::LogEntry;
use serde_json;

#[cfg(windows)]
use crate::execution::process_group::ProcessGroup;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use tauri::{Emitter, Manager};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Information about a running plug service
#[derive(Debug)]
struct PlugService {
    port: u16,
    process: Child,
    #[cfg(windows)]
    process_group: Option<ProcessGroup>,
}

/// Manager for individual plug service processes
#[derive(Debug)]
pub struct PlugServiceManager {
    services: Mutex<HashMap<String, PlugService>>,
    project_dir: PathBuf,
    used_ports: Mutex<HashSet<u16>>,
    id: String, // Unique ID for debugging
}

impl PlugServiceManager {
    pub fn new(project_dir: PathBuf) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        crate::cli_output::debug(format!("Creating new PlugServiceManager with ID: {}", id));
        Self {
            services: Mutex::new(HashMap::new()),
            project_dir,
            used_ports: Mutex::new(HashSet::new()),
            id,
        }
    }

    /// Start a plug service for a specific plug instance
    pub async fn start_plug_service(
        &self,
        instance_key: String,
        base_plug_name: String,
        plug_config: serde_json::Value,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<u16, String> {
        // Check if service already exists (minimal lock time)
        {
            let services = self.services.lock().await;
            if services.contains_key(&instance_key) {
                return Err(format!("Plug service {} already running", instance_key));
            }
        } // Release lock immediately

        // Get next available port using portpicker
        let port = {
            let mut used_ports = self.used_ports.lock().await;
            let port = portpicker::pick_unused_port()
                .ok_or_else(|| "No available ports found".to_string())?;

            used_ports.insert(port);
            crate::cli_output::debug(format!("Allocated port {} for plug service", port));
            port
        };

        let python_script = if let Some(handle) = app_handle {
            // GUI mode: use Tauri to resolve the resource path
            handle
                .path()
                .resolve(
                    "python/persistent_plug_service.py",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| format!("Failed to resolve persistent_plug_service.py: {}", e))?
        } else {
            // CLI mode: try several common locations for the plug service script
            let exe_path = std::env::current_exe()
                .map_err(|e| format!("Failed to get executable path: {}", e))?;
            let exe_dir = exe_path
                .parent()
                .ok_or_else(|| "Failed to get executable directory".to_string())?;

            let possible_paths = vec![
                exe_dir.join("python/persistent_plug_service.py"), // Relative to exe
                exe_dir
                    .parent()
                    .map(|p| p.join("python/persistent_plug_service.py"))
                    .unwrap_or_else(|| std::path::PathBuf::new()), // One level up
                exe_dir
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.join("python/persistent_plug_service.py"))
                    .unwrap_or_else(|| std::path::PathBuf::new()), // Two levels up (for cargo run in dev)
                std::path::PathBuf::from("src-tauri/python/persistent_plug_service.py"), // Development path
            ];

            let mut found_path = None;
            for path in possible_paths {
                if path.exists() {
                    found_path = Some(path);
                    break;
                }
            }

            found_path.ok_or_else(|| {
                "Failed to find persistent_plug_service.py in any expected location".to_string()
            })?
        };

        // Check if the Python script exists
        if !python_script.exists() {
            crate::cli_output::warning(format!("Python script not found at: {:?}", python_script));
            crate::cli_output::debug(format!("Current directory: {:?}", std::env::current_dir()));
            // Clean up port before returning error
            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);
            return Err(format!("Python script not found at: {:?}", python_script));
        }

        // Resolve Python executable using UV
        log::info!("Resolving Python for plug service: {}", instance_key);
        let python_path = if let Some(handle) = app_handle {
            crate::python::resolve_python_executable(Some(handle), &self.project_dir)
                .await
                .map_err(|e| {
                    log::error!(
                        "Failed to resolve Python for plug service '{}': {}",
                        instance_key,
                        e
                    );
                    // Clean up port before returning error
                    let used_ports_guard = self.used_ports.try_lock();
                    if let Ok(mut used_ports) = used_ports_guard {
                        used_ports.remove(&port);
                    }
                    format!(
                        "Failed to resolve Python for plug service '{}':\n\n{}",
                        instance_key, e
                    )
                })?
        } else {
            // Fail explicitly instead of fallback - CLI mode requires proper configuration
            log::error!("No app_handle provided for plug service - cannot resolve Python");
            // Clean up port before returning error
            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);
            return Err(
                "Cannot start plug service: app_handle required for Python resolution".to_string(),
            );
        };

        log::info!(
            "Starting plug service '{}' with Python: {}",
            instance_key,
            python_path
        );

        let mut command = Command::new(&python_path);
        command
            .arg(&python_script)
            .arg("--procedure-dir")
            .arg(&self.project_dir)
            .arg("--port")
            .arg(port.to_string())
            .arg("--plug-name")
            .arg(&base_plug_name)
            .arg("--plug-config")
            .arg(plug_config.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        crate::utils::configure_no_window_tokio(&mut command);

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            format!(
                "Starting plug service '{}' with Python: {} (script: {:?})",
                instance_key, python_path, python_script
            ),
        );
        let mut process = match command.spawn() {
            Ok(p) => p,
            Err(e) => {
                // Clean up port on spawn failure
                let mut used_ports = self.used_ports.lock().await;
                used_ports.remove(&port);
                return Err(format!("Failed to spawn plug service: {}", e));
            }
        };

        #[cfg(windows)]
        let process_group = if let Some(pid) = process.id() {
            let pg = ProcessGroup::new()?;
            pg.add_process(pid).await?;
            Some(pg)
        } else {
            None
        };

        // Extract slot_id from instance_key if present
        // Pattern: "plug_name_slot_id" for each-scope plugs, or just "plug_name" for all-scope
        let slot_id = if instance_key.len() > base_plug_name.len()
            && instance_key.starts_with(&base_plug_name)
        {
            let suffix = &instance_key[base_plug_name.len()..];
            if suffix.starts_with('_') {
                Some(suffix[1..].to_string())
            } else {
                None
            }
        } else {
            None
        };

        // Spawn async tasks to monitor stdout/stderr for real-time logging
        if let Some(stdout) = process.stdout.take() {
            let instance_key_clone = instance_key.clone();
            let base_plug_name_clone = base_plug_name.clone();
            let slot_id_clone = slot_id.clone();
            let app_handle_clone = app_handle.cloned();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Try to parse as LogEntry
                    if let Ok(log_entry) = serde_json::from_str::<LogEntry>(&line) {
                        // Structured log message
                        crate::cli_output::print_section(
                            crate::cli_output::Section::Plugs,
                            format!(
                                "[{}] [{}] {}",
                                instance_key_clone,
                                log_entry.level.to_uppercase(),
                                log_entry.message
                            ),
                        );

                        if let Some(ref app) = app_handle_clone {
                            let mut event_data = serde_json::json!({
                                "timestamp": log_entry.timestamp,
                                "level": log_entry.level.to_lowercase(),
                                "message": log_entry.message,
                                "plug_name": base_plug_name_clone,
                            });

                            if let Some(ref slot) = slot_id_clone {
                                event_data["slot_id"] = serde_json::Value::String(slot.clone());
                            }
                            if let Some(ref file) = log_entry.file {
                                event_data["file"] = serde_json::Value::String(file.clone());
                            }
                            if let Some(line_num) = log_entry.line {
                                event_data["line"] = serde_json::Value::Number(line_num.into());
                            }

                            let _ = app.emit("plug-log", event_data);
                        }
                    } else {
                        // Plain text fallback (backwards compatibility)
                        crate::cli_output::print_section(
                            crate::cli_output::Section::Plugs,
                            format!("[{}] {}", instance_key_clone, line),
                        );

                        if let Some(ref app) = app_handle_clone {
                            let mut event_data = serde_json::json!({
                                "timestamp": chrono::Utc::now().to_rfc3339(),
                                "level": "info",
                                "message": line,
                                "plug_name": base_plug_name_clone,
                            });

                            if let Some(ref slot) = slot_id_clone {
                                event_data["slot_id"] = serde_json::Value::String(slot.clone());
                            }

                            let _ = app.emit("plug-log", event_data);
                        }
                    }
                }
            });
        }

        if let Some(stderr) = process.stderr.take() {
            let instance_key_clone = instance_key.clone();
            let base_plug_name_clone = base_plug_name.clone();
            let slot_id_clone = slot_id.clone();
            let app_handle_clone = app_handle.cloned();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Try to parse as LogEntry
                    if let Ok(log_entry) = serde_json::from_str::<LogEntry>(&line) {
                        // Structured log message
                        let cli_output_fn = match log_entry.level.to_uppercase().as_str() {
                            "ERROR" => crate::cli_output::error,
                            "WARNING" => crate::cli_output::warning,
                            _ => crate::cli_output::warning,
                        };

                        cli_output_fn(format!("[{}] {}", instance_key_clone, log_entry.message));

                        if let Some(ref app) = app_handle_clone {
                            let mut event_data = serde_json::json!({
                                "timestamp": log_entry.timestamp,
                                "level": log_entry.level.to_lowercase(),
                                "message": log_entry.message,
                                "plug_name": base_plug_name_clone,
                            });

                            if let Some(ref slot) = slot_id_clone {
                                event_data["slot_id"] = serde_json::Value::String(slot.clone());
                            }
                            if let Some(ref file) = log_entry.file {
                                event_data["file"] = serde_json::Value::String(file.clone());
                            }
                            if let Some(line_num) = log_entry.line {
                                event_data["line"] = serde_json::Value::Number(line_num.into());
                            }

                            let _ = app.emit("plug-log", event_data);
                        }
                    } else {
                        // Plain text fallback (backwards compatibility)
                        crate::cli_output::warning(format!("[{}] {}", instance_key_clone, line));

                        if let Some(ref app) = app_handle_clone {
                            let mut event_data = serde_json::json!({
                                "timestamp": chrono::Utc::now().to_rfc3339(),
                                "level": "warning",
                                "message": line,
                                "plug_name": base_plug_name_clone,
                            });

                            if let Some(ref slot) = slot_id_clone {
                                event_data["slot_id"] = serde_json::Value::String(slot.clone());
                            }

                            let _ = app.emit("plug-log", event_data);
                        }
                    }
                }
            });
        }

        // Wait for the service to be ready to accept connections
        let mut attempts = 0;
        let max_attempts = 50; // 5 seconds max wait time
        let mut service_ready = false;

        while attempts < max_attempts {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Check if process is still alive
            if let Ok(Some(status)) = process.try_wait() {
                crate::cli_output::warning(format!(
                    "Plug service {} exited with status: {:?} (check logs above for details)",
                    instance_key, status
                ));

                // Clean up the port allocation since the service failed
                let mut used_ports = self.used_ports.lock().await;
                used_ports.remove(&port);
                drop(used_ports);

                return Err(format!(
                    "Plug service {} exited during startup with status: {:?}",
                    instance_key, status
                ));
            }

            // Try to connect to the service
            match tokio::net::TcpStream::connect(("localhost", port)).await {
                Ok(_) => {
                    service_ready = true;
                    break;
                }
                Err(_) => {
                    attempts += 1;
                }
            }
        }

        if !service_ready {
            // Kill the process if it didn't become ready
            let _ = process.kill().await;

            // Clean up the port allocation
            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);

            return Err(format!(
                "Plug service {} failed to become ready within 5 seconds",
                instance_key
            ));
        }

        // NOW insert into services HashMap (only after successful startup)
        let service = PlugService {
            port,
            process,
            #[cfg(windows)]
            process_group,
        };
        let mut services = self.services.lock().await;
        services.insert(instance_key.clone(), service);
        crate::cli_output::debug(format!(
            "Inserted {} into services HashMap (Manager ID: {})",
            instance_key, self.id
        ));
        let service_count = services.len();
        drop(services);

        crate::cli_output::system_operation(
            format!("Started plug service {}", instance_key),
            format!("port {} (total services: {})", port, service_count),
            true,
        );

        // Emit event for UI to know plug is now active
        // Note: We'd need to pass app_handle here to emit events
        // For now, return the service list

        Ok(port)
    }

    /// Send teardown command to a plug service  
    async fn send_teardown_command(&self, plug_name: &str, port: u16) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;

        crate::cli_output::debug(format!(
            "Sending teardown command to plug {} on port {}",
            plug_name, port
        ));

        // Try to connect and send teardown command with 1s timeout
        let connect_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            tokio::net::TcpStream::connect(("localhost", port)),
        )
        .await;

        match connect_result {
            Ok(Ok(mut stream)) => {
                let teardown_command = serde_json::json!({
                    "action": "teardown"
                });
                let command_str = teardown_command.to_string() + "\n";

                // Send teardown command
                if let Err(e) = stream.write_all(command_str.as_bytes()).await {
                    crate::cli_output::error(format!(
                        "Failed to send teardown command to {}: {}",
                        plug_name, e
                    ));
                    return Err(format!("Failed to send teardown command: {}", e));
                }

                // Read response (one line)
                let mut reader = tokio::io::BufReader::new(stream);
                let mut response = String::new();
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(5), // 5 second timeout for teardown
                    reader.read_line(&mut response),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        crate::cli_output::debug(format!(
                            "Teardown command sent to {}, response: {}",
                            plug_name,
                            response.trim()
                        ));
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        crate::cli_output::error(format!(
                            "Failed to read teardown response from {}: {}",
                            plug_name, e
                        ));
                        Ok(()) // Still consider it successful since we sent the command
                    }
                    Err(_) => {
                        crate::cli_output::warning(format!(
                            "Teardown command timed out for {}",
                            plug_name
                        ));
                        Ok(()) // Timeout is okay, teardown might take time
                    }
                }
            }
            Ok(Err(e)) => {
                crate::cli_output::warning(format!(
                    "Failed to connect to {} for teardown: {}",
                    plug_name, e
                ));
                Err(format!("Failed to connect for teardown: {}", e))
            }
            Err(_) => {
                crate::cli_output::warning(format!(
                    "Connection timeout for {} teardown (plug likely already terminated)",
                    plug_name
                ));
                Err(format!("Connection timeout for teardown"))
            }
        }
    }

    /// Stop a specific plug service with proper teardown
    pub async fn stop_plug_service(&self, plug_name: &str) -> Result<(), String> {
        let mut services = self.services.lock().await;
        crate::cli_output::debug(format!(
            "stop_plug_service called for {} (Manager ID: {}), current services: {:?}",
            plug_name,
            self.id,
            services.keys().cloned().collect::<Vec<_>>()
        ));

        if let Some(mut service) = services.remove(plug_name) {
            let port = service.port;

            crate::cli_output::print_section(
                crate::cli_output::Section::Plugs,
                format!("Stopping plug service {} on port {}", plug_name, port),
            );

            // Step 1: Send teardown command
            let teardown_start = std::time::Instant::now();
            if let Err(e) = self.send_teardown_command(plug_name, port).await {
                crate::cli_output::warning(format!("Teardown command failed: {}", e));
            }

            // Step 2: Poll for graceful exit (max 3 seconds)
            let max_wait = tokio::time::Duration::from_secs(3);
            let poll_interval = tokio::time::Duration::from_millis(100);
            let mut waited = tokio::time::Duration::ZERO;

            while waited < max_wait {
                match service.process.try_wait() {
                    Ok(Some(status)) => {
                        let elapsed = teardown_start.elapsed();
                        crate::cli_output::success(format!(
                            "OK: Plug {} exited gracefully in {:.1}s (status: {:?})",
                            plug_name,
                            elapsed.as_secs_f32(),
                            status
                        ));

                        // Clean exit - release port and return
                        let mut used_ports = self.used_ports.lock().await;
                        used_ports.remove(&port);
                        return Ok(());
                    }
                    Ok(None) => {
                        // Still running, wait and retry
                        tokio::time::sleep(poll_interval).await;
                        waited += poll_interval;
                    }
                    Err(e) => {
                        crate::cli_output::error(format!("Error checking process: {}", e));
                        break;
                    }
                }
            }

            // Step 3: Graceful timeout exceeded - send SIGTERM
            crate::cli_output::warning(format!(
                "WARNING:  Plug {} did not exit after {:.1}s, sending SIGTERM",
                plug_name,
                max_wait.as_secs_f32()
            ));

            if let Err(e) = service.process.kill().await {
                crate::cli_output::error(format!("SIGTERM failed: {}", e));
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Step 4: Check if SIGTERM worked
            match service.process.try_wait() {
                Ok(Some(status)) => {
                    crate::cli_output::debug(format!(
                        "Process terminated via SIGTERM: {:?}",
                        status
                    ));
                }
                Ok(None) => {
                    // Step 5: Force kill with SIGKILL
                    crate::cli_output::error(format!(
                        "WARNING:  Force killing {} with SIGKILL",
                        plug_name
                    ));

                    #[cfg(unix)]
                    if let Some(pid) = service.process.id() {
                        use std::process::Command as StdCommand;
                        let _ = StdCommand::new("kill")
                            .arg("-9")
                            .arg(pid.to_string())
                            .output();
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }
                Err(e) => {
                    crate::cli_output::error(format!("Error after SIGTERM: {}", e));
                }
            }

            // Release port
            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);

            crate::cli_output::success(format!("Stopped plug {} on port {}", plug_name, port));
            Ok(())
        } else {
            Err(format!("Plug service {} not found", plug_name))
        }
    }

    /// Force kill a plug service immediately without graceful teardown
    /// Returns the port that was released
    pub async fn force_kill_plug_service(&self, plug_name: &str) -> Result<u16, String> {
        let service = {
            let mut services = self.services.lock().await;
            services.remove(plug_name)
        };

        #[cfg_attr(unix, allow(unused_mut))]
        if let Some(mut service) = service {
            let port = service.port;
            let pid = service.process.id();

            crate::cli_output::warning(format!(
                "WARNING:  Force killing plug service {} (PID: {:?}) with SIGKILL",
                plug_name, pid
            ));

            #[cfg(windows)]
            {
                if let Some(ref pg) = service.process_group {
                    let _ = pg.kill_all().await;
                }
                let _ = service.process.kill().await;
            }

            #[cfg(unix)]
            if let Some(pid) = pid {
                use std::process::Command as StdCommand;
                let _ = StdCommand::new("kill")
                    .arg("-9")
                    .arg(pid.to_string())
                    .output();
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);

            crate::cli_output::success(format!("Force killed plug {} on port {}", plug_name, port));
            Ok(port)
        } else {
            Err(format!("Plug service {} not found", plug_name))
        }
    }

    /// Force kill all plug services without graceful teardown
    pub async fn force_kill_all_services(&self) -> Result<(), String> {
        let plug_names: Vec<String> = {
            let services = self.services.lock().await;
            services.keys().cloned().collect()
        };

        let service_count = plug_names.len();

        if service_count == 0 {
            crate::cli_output::debug(format!(
                "No plug services to force kill (Manager ID: {})",
                self.id
            ));
            return Ok(());
        }

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            format!("Force killing {} plug services", service_count),
        );

        let mut failures = Vec::new();
        for plug_name in &plug_names {
            if let Err(e) = self.force_kill_plug_service(plug_name).await {
                failures.push(e);
            }
        }

        if !failures.is_empty() {
            crate::cli_output::warning(format!(
                "Some plug services failed to stop: {:?}",
                failures
            ));
        }

        crate::cli_output::success(format!("Force killed {} plug services", service_count));

        Ok(())
    }

    /// Stop all plug services with proper teardown
    pub async fn stop_all_services(&self) -> Result<(), String> {
        let plug_names: Vec<String> = {
            let services = self.services.lock().await;
            services.keys().cloned().collect()
        };

        let service_count = plug_names.len();

        if service_count == 0 {
            crate::cli_output::debug(format!(
                "No plug services to stop (Manager ID: {})",
                self.id
            ));
            return Ok(());
        }

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            format!("Stopping {} plug services", service_count),
        );

        for plug_name in plug_names {
            if let Err(e) = self.stop_plug_service(&plug_name).await {
                crate::cli_output::warning(format!(
                    "Failed to stop plug service {}: {}",
                    plug_name, e
                ));
            }
        }

        crate::cli_output::success(format!("Stopped all {} plug services", service_count));
        Ok(())
    }

    /// Get the port for a specific plug service
    pub async fn get_plug_port(&self, plug_name: &str) -> Option<u16> {
        let services = self.services.lock().await;
        services.get(plug_name).map(|service| service.port)
    }

    /// List all running services
    pub async fn list_services(&self) -> Vec<String> {
        let services = self.services.lock().await;
        let service_names: Vec<String> = services.keys().cloned().collect();
        crate::cli_output::debug(format!(
            "PlugServiceManager returning {} services: {:?}",
            service_names.len(),
            service_names
        ));
        service_names
    }
}

/// Kill all Python processes that might be persistent plug services
pub fn kill_all_plug_processes() -> Result<(), String> {
    crate::cli_output::print_section(
        crate::cli_output::Section::System,
        "Attempting to kill all persistent plug service processes",
    );

    #[cfg(unix)]
    {
        use std::process::Command;
        let output = Command::new("pgrep")
            .arg("-f")
            .arg("persistent_plug_service")
            .output();

        match output {
            Ok(output) => {
                let pids_str = String::from_utf8_lossy(&output.stdout);
                if !pids_str.trim().is_empty() {
                    let pids: Vec<&str> = pids_str.trim().split('\n').collect();
                    crate::cli_output::debug(format!(
                        "Found {} persistent plug service processes",
                        pids.len()
                    ));

                    for pid in pids {
                        if !pid.trim().is_empty() {
                            crate::cli_output::debug(format!("Killing process {}", pid.trim()));
                            let _ = Command::new("kill").arg("-9").arg(pid.trim()).output();
                        }
                    }
                } else {
                    crate::cli_output::success("No persistent plug service processes found");
                }
            }
            Err(e) => {
                crate::cli_output::warning(format!("Failed to search for plug processes: {}", e));
            }
        }
    }

    #[cfg(windows)]
    {
        crate::cli_output::debug("Windows process teardown not fully implemented");
    }

    Ok(())
}

impl Drop for PlugServiceManager {
    fn drop(&mut self) {
        crate::cli_output::debug(format!(
            "PlugServiceManager {} being dropped, attempting teardown",
            self.id
        ));

        // We can't use async in Drop, so we'll do a synchronous teardown attempt
        if let Ok(services) = self.services.try_lock() {
            if !services.is_empty() {
                crate::cli_output::warning(format!(
                    "Found {} services still running during drop, attempting force teardown",
                    services.len()
                ));

                #[cfg(unix)]
                {
                    use std::process::Command as StdCommand;
                    for (plug_name, service) in services.iter() {
                        if let Some(pid) = service.process.id() {
                            crate::cli_output::debug(format!(
                                "Force killing process {} for plug {}",
                                pid, plug_name
                            ));
                            let _ = StdCommand::new("kill")
                                .arg("-9")
                                .arg(pid.to_string())
                                .output();
                        }
                    }
                }
            }
        }

        crate::cli_output::debug(format!("PlugServiceManager {} teardown complete", self.id));
    }
}
