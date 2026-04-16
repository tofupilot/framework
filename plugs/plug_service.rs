//! Per-plug service management
//! Each plug instance runs in its own isolated service process

use crate::execution::log::LogEntry;
use crate::execution::events::PlugLogEvent;
use crate::child_process::ChildProcess;
use execution_engine::protocol::{PlugRequest, PlugResponse};
use execution_engine::transport;
use serde_json;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tauri::Manager;
use tauri_specta::Event;
use tokio::io::BufReader;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub type PlugService = ChildProcess;

/// Send a plug request and read a response over a fresh TCP connection.
async fn plug_rpc(port: u16, request: &PlugRequest) -> Result<PlugResponse, String> {
    let stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| format!("TCP connect to plug failed: {}", e))?;

    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    transport::write_json_line(&mut write_half, request).await?;
    transport::read_json_line::<PlugResponse>(&mut reader)
        .await?
        .ok_or_else(|| "Plug closed connection without response".to_string())
}

/// Manager for individual plug service processes
#[derive(Debug)]
pub struct PlugServiceManager {
    services: Mutex<HashMap<String, PlugService>>,
    project_dir: PathBuf,
    used_ports: Mutex<HashSet<u16>>,
    id: String,
}

impl PlugServiceManager {
    pub fn new(project_dir: PathBuf) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        log::debug!("Creating new PlugServiceManager with ID: {}", id);
        Self {
            services: Mutex::new(HashMap::new()),
            project_dir,
            used_ports: Mutex::new(HashSet::new()),
            id,
        }
    }

    async fn wait_for_plug_ready(
        port: u16,
        plug_name: &str,
        timeout_secs: u64,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let poll_interval = std::time::Duration::from_millis(100);

        loop {
            match plug_rpc(port, &PlugRequest::GetStatus).await {
                Ok(response) => {
                    if response.success {
                        return Ok(());
                    }
                    if let Some(ref err) = response.error {
                        if !err.contains("initializing") {
                            return Err(err.clone());
                        }
                    }
                }
                Err(e) => {
                    log::warn!("GetStatus failed for {}: {}", plug_name, e);
                }
            }

            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "Plug '{}' initialization timed out after {}s",
                    plug_name, timeout_secs
                ));
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    fn find_plug_script_cli() -> Result<std::path::PathBuf, String> {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current executable path: {}", e))?;
        let exe_dir = exe_path
            .parent()
            .ok_or("Failed to get executable directory")?;

        let script_path = exe_dir.join("python").join("tp_plug.py");
        if script_path.exists() {
            return Ok(script_path);
        }

        Err(format!(
            "tp_plug.py not found at {}. Ensure the Python resources are built.",
            script_path.display()
        ))
    }

    /// Start a plug service for a specific plug instance
    pub async fn start_plug_service(
        &self,
        instance_key: String,
        plug_key: String,
        display_name: String,
        plug_config: serde_json::Value,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<u16, String> {
        {
            let services = self.services.lock().await;
            if services.contains_key(&instance_key) {
                return Err(format!("Plug service {} already running", instance_key));
            }
        }

        let python_script = if let Some(handle) = app_handle {
            handle
                .path()
                .resolve(
                    "python/tp_plug.py",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| format!("Failed to resolve tp_plug.py: {}", e))?
        } else {
            Self::find_plug_script_cli()?
        };

        log::info!("Resolving Python for plug service: {}", instance_key);
        let python_path = crate::python::resolve_python_internal(app_handle, &self.project_dir)
            .await
            .map_err(|e| {
                log::error!(
                    "Failed to resolve Python for plug service '{}': {}",
                    instance_key, e
                );
                format!(
                    "Failed to resolve Python for plug service '{}':\n\n{}",
                    instance_key, e
                )
            })?;

        log::info!(
            "Starting plug service '{}' with Python: {} (script: {:?})",
            instance_key, python_path, python_script
        );

        let slot_id = if instance_key.len() > plug_key.len()
            && instance_key.starts_with(&plug_key)
        {
            let suffix = &instance_key[plug_key.len()..];
            if suffix.starts_with('_') {
                Some(suffix[1..].to_string())
            } else {
                None
            }
        } else {
            None
        };

        let instance_key_clone = instance_key.clone();
        let plug_key_clone = plug_key.clone();
        let display_name_clone = display_name.clone();
        let slot_id_clone = slot_id.clone();
        let app_handle_opt = app_handle.cloned();

        let mut service = ChildProcess::spawn(
            &python_path,
            python_script,
            vec![
                "--procedure-dir".to_string(),
                self.project_dir.to_string_lossy().to_string(),
                "--plug-name".to_string(),
                plug_key.clone(),
                "--display-name".to_string(),
                display_name.clone(),
                "--plug-config".to_string(),
                plug_config.to_string(),
            ],
            None,
            vec![],
            Some(Box::new(move |stderr| {
                tokio::spawn(async move {
                    let reader = tokio::io::BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Ok(log_entry) = serde_json::from_str::<LogEntry>(&line) {
                            match log_entry.level.to_uppercase().as_str() {
                                "ERROR" => log::error!("[{}] {}", instance_key_clone, log_entry.message),
                                "WARNING" => log::warn!("[{}] {}", instance_key_clone, log_entry.message),
                                _ => log::warn!("[{}] {}", instance_key_clone, log_entry.message),
                            };

                            if let Some(ref app) = app_handle_opt {
                                let _ = PlugLogEvent {
                                    plug_key: plug_key_clone.clone(),
                                    plug_name: display_name_clone.clone(),
                                    slot_id: slot_id_clone.clone(),
                                    level: log_entry.level.to_lowercase(),
                                    message: log_entry.message.clone(),
                                    timestamp: Some(log_entry.timestamp.clone()),
                                    line: log_entry.line,
                                }
                                .emit(app);
                            }
                        } else {
                            log::warn!("[{}] {}", instance_key_clone, line);

                            if let Some(ref app) = app_handle_opt {
                                let _ = PlugLogEvent {
                                    plug_key: plug_key_clone.clone(),
                                    plug_name: display_name_clone.clone(),
                                    slot_id: slot_id_clone.clone(),
                                    level: "warning".to_string(),
                                    message: line.clone(),
                                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                                    line: None,
                                }
                                .emit(app);
                            }
                        }
                    }
                });
            })),
        )
        .await?;

        let port = service.port;

        if let Err(e) = Self::wait_for_plug_ready(port, &instance_key, 30).await {
            log::error!("Plug '{}' initialization failed: {}", instance_key, e);
            service.force_kill().await.ok();
            return Err(e);
        }

        {
            let mut used_ports = self.used_ports.lock().await;
            used_ports.insert(port);
        }

        let mut services = self.services.lock().await;
        services.insert(instance_key.clone(), service);
        log::debug!(
            "Inserted {} into services HashMap (Manager ID: {})",
            instance_key, self.id
        );
        let service_count = services.len();
        drop(services);

        log::info!(
            "Started plug service {} on port {} (total services: {})",
            instance_key, port, service_count
        );

        Ok(port)
    }

    /// Stop a specific plug service with proper teardown
    pub async fn stop_plug_service(&self, plug_name: &str) -> Result<(), String> {
        let mut services = self.services.lock().await;
        log::debug!(
            "stop_plug_service called for {} (Manager ID: {}), current services: {:?}",
            plug_name,
            self.id,
            services.keys().cloned().collect::<Vec<_>>()
        );

        if let Some(mut service) = services.remove(plug_name) {
            let port = service.port;
            log::info!("Stopping plug service {} on port {}", plug_name, port);
            drop(services);

            let result = service.graceful_shutdown(
                || async move {
                    let _ = tokio::time::timeout(
                        tokio::time::Duration::from_secs(5),
                        plug_rpc(port, &PlugRequest::Cleanup)
                    ).await;

                    let _ = tokio::time::timeout(
                        tokio::time::Duration::from_secs(1),
                        plug_rpc(port, &PlugRequest::Shutdown)
                    ).await;

                    Ok(())
                },
                3,
            ).await;

            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);

            log::info!("Stopped plug {} on port {}", plug_name, port);
            result
        } else {
            Err(format!("Plug service {} not found", plug_name))
        }
    }

    pub async fn force_kill_plug_service(&self, plug_name: &str) -> Result<u16, String> {
        let service = {
            let mut services = self.services.lock().await;
            services.remove(plug_name)
        };

        if let Some(mut service) = service {
            let port = service.port;

            log::warn!(
                "WARNING: Force killing plug service {} process group",
                plug_name
            );

            service.force_kill().await?;

            let mut used_ports = self.used_ports.lock().await;
            used_ports.remove(&port);

            log::info!("Force killed plug {} on port {}", plug_name, port);
            Ok(port)
        } else {
            Err(format!("Plug service {} not found", plug_name))
        }
    }

    pub async fn force_kill_all_services(&self) -> Result<(), String> {
        let plug_names: Vec<String> = {
            let services = self.services.lock().await;
            services.keys().cloned().collect()
        };

        let service_count = plug_names.len();

        if service_count == 0 {
            log::debug!("No plug services to force kill (Manager ID: {})", self.id);
            return Ok(());
        }

        log::info!("Force killing {} plug services", service_count);

        let mut failures = Vec::new();
        for plug_name in &plug_names {
            if let Err(e) = self.force_kill_plug_service(plug_name).await {
                failures.push(e);
            }
        }

        if !failures.is_empty() {
            log::warn!("Some plug services failed to stop: {:?}", failures);
        }

        log::info!("Force killed {} plug services", service_count);
        Ok(())
    }

    pub async fn stop_all_services(&self) -> Result<(), String> {
        let plug_names: Vec<String> = {
            let services = self.services.lock().await;
            services.keys().cloned().collect()
        };

        let service_count = plug_names.len();

        if service_count == 0 {
            log::debug!("No plug services to stop (Manager ID: {})", self.id);
            return Ok(());
        }

        log::info!("Stopping {} plug services", service_count);

        for plug_name in plug_names {
            if let Err(e) = self.stop_plug_service(&plug_name).await {
                log::warn!("Failed to stop plug service {}: {}", plug_name, e);
            }
        }

        log::info!("Stopped all {} plug services", service_count);
        Ok(())
    }

    pub async fn get_plug_port(&self, plug_name: &str) -> Option<u16> {
        let services = self.services.lock().await;
        services.get(plug_name).map(|service| service.port)
    }

    pub async fn list_services(&self) -> Vec<String> {
        let services = self.services.lock().await;
        let service_names: Vec<String> = services.keys().cloned().collect();
        log::debug!(
            "PlugServiceManager returning {} services: {:?}",
            service_names.len(),
            service_names
        );
        service_names
    }
}

impl Drop for PlugServiceManager {
    fn drop(&mut self) {
        log::debug!(
            "PlugServiceManager {} being dropped, attempting teardown",
            self.id
        );

        let services = self.services.get_mut();
        if !services.is_empty() {
            log::warn!(
                "Found {} services still running during drop, attempting force teardown",
                services.len()
            );

            for (plug_name, mut service) in services.drain() {
                log::debug!("Force killing plug {}", plug_name);
                let _ = service.process.start_kill();
            }
        }

        log::debug!("PlugServiceManager {} teardown complete", self.id);
    }
}
