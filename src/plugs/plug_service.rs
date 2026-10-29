//! Per-plug service management
//! Each plug instance runs in its own isolated service process

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::event_sink::{EventSink, ExecutionEvent};
use crate::events::PlugLogEvent;
use crate::log::LogEntry;
use crate::plugs::process::ChildProcess;
use crate::protocol::{MethodRequest, PlugRequest, PlugResponse};
use crate::transport;
use serde_json;

pub type PlugService = ChildProcess;

/// Probe a plug service's liveness with a bounded `GetStatus` RPC.
/// Used by the station plug host before reusing a held instance across
/// runs — a dead or wedged process must be respawned, not handed to a
/// new run.
pub async fn probe_plug_health(port: u16) -> Result<(), String> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        plug_rpc(port, &PlugRequest::GetStatus),
    )
    .await
    .map_err(|_| "GetStatus timed out after 5s".to_string())??;

    if response.success {
        Ok(())
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "Plug unhealthy".to_string()))
    }
}

/// Call one method on a running plug service and wait for its answer,
/// however long that takes. Used by Studio debug sessions to drive a
/// plug outside any run. Deliberately unbounded: the method is
/// arbitrary user code driving real hardware, and nothing here can
/// interrupt it — a Python method cannot be cut from outside short of
/// killing the service, which is the operator's Disconnect, not a
/// timer's. A bound would only turn "still running" into a false
/// "timed out" while the next call queued behind the method anyway.
/// A dead service is still caught at once: its socket closes and the
/// read below fails.
pub async fn call_plug_method(
    port: u16,
    method: &str,
    args_json: Option<String>,
    kwargs_json: Option<String>,
) -> Result<PlugResponse, String> {
    let request = PlugRequest::CallMethod(MethodRequest {
        method: method.to_string(),
        // The wire field predates kwargs and is a plain string: "" is
        // its established spelling for "no positional arguments".
        args_json: args_json.unwrap_or_default(),
        kwargs_json,
    });
    plug_rpc(port, &request).await
}

/// Send a plug request and read a response over a fresh TCP connection.
async fn plug_rpc(port: u16, request: &PlugRequest) -> Result<PlugResponse, String> {
    let stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| format!("Plug unreachable: {}", e))?;

    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    transport::write_json_line(&mut write_half, request).await?;
    transport::read_json_line::<PlugResponse, _>(&mut reader)
        .await?
        .ok_or_else(|| "Plug closed connection without response".to_string())
}

/// A line of the plug service's stderr: `service` marks its own trace.
#[derive(serde::Deserialize)]
struct ServiceLine {
    #[serde(flatten)]
    entry: LogEntry,
    #[serde(default)]
    service: bool,
}

/// How long a plug's `__init__` may take before its open is given up, in
/// a run and in `tofupilot plug` alike, when the plug sets no
/// `init_timeout:`.
pub const PLUG_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Longest a single readiness probe may take. A plug whose `__init__`
/// holds the GIL (a C extension blocking without releasing it) cannot
/// answer at all: the connection sits in the listen backlog and an
/// unbounded read waited forever, with the resource manager's write
/// lock held, so even a force-kill of the run queued behind it.
const READY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Poll a plug service until it reports ready, fails, or `timeout`
/// elapses. `exited` reports the service's exit status once it is gone:
/// a service that died during `__init__` (a constructor segfaulting the
/// interpreter) will never answer, and naming the crash beats a timeout.
async fn poll_plug_ready(
    port: u16,
    plug_name: &str,
    timeout: std::time::Duration,
    mut exited: impl FnMut() -> Option<String>,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let poll_interval = std::time::Duration::from_millis(100);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(
            remaining.min(READY_PROBE_TIMEOUT),
            plug_rpc(port, &PlugRequest::GetStatus),
        )
        .await
        {
            Ok(Ok(response)) => {
                if response.success {
                    return Ok(());
                }
                if !response.initializing {
                    return Err(response
                        .error
                        .unwrap_or_else(|| format!("Plug {plug_name}: not ready")));
                }
            }
            Ok(Err(e)) => log::debug!("GetStatus failed for {}: {}", plug_name, e),
            Err(_) => log::debug!("GetStatus for {} got no answer", plug_name),
        }

        if let Some(status) = exited() {
            return Err(format!(
                "Plug {plug_name}: exited during init ({status}); see plug log"
            ));
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Plug {plug_name}: __init__ did not finish within {}; raise the plug's `init_timeout:` for a slower instrument",
                crate::procedure::schema::format_duration(timeout.as_millis() as u64)
            ));
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Manager for individual plug service processes
#[derive(Debug)]
pub struct PlugServiceManager {
    services: Mutex<HashMap<String, PlugService>>,
    project_dir: PathBuf,
    /// Pre-resolved Python interpreter. When set, plug services skip
    /// the engine's `resolve_python` walk-up and use this path
    /// directly — the CLI computes it deterministically per
    /// deployment, so the walk is never needed for pulled bundles.
    /// `None` keeps the legacy behavior for callers that haven't
    /// migrated (Studio, in-engine tests).
    python_path: Option<PathBuf>,
    id: String,
    /// The dashboard credentials, then the procedure's `env:`, set on
    /// every plug process this manager spawns.
    /// Interior mutability because the manager is built inside
    /// `ResourceManager` before the host knows the run's identity.
    procedure_env: std::sync::RwLock<crate::procedure_env::ProcedureEnv>,
}

/// The variable a plug service reads its spawn config from (tp_plug.py
/// `--plug-config-env`), and removes before any plug code runs.
pub const PLUG_CONFIG_ENV: &str = "TP_PLUG_CONFIG";

impl PlugServiceManager {
    pub fn new(project_dir: PathBuf) -> Self {
        Self::new_with_python(project_dir, None)
    }

    pub fn new_with_python(project_dir: PathBuf, python_path: Option<PathBuf>) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        log::debug!("Creating new PlugServiceManager with ID: {}", id);
        Self {
            services: Mutex::new(HashMap::new()),
            project_dir,
            python_path,
            id,
            procedure_env: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// The env (credentials, then the procedure's `env:`) for plug
    /// processes spawned from now on. Running services keep the env they
    /// started with.
    pub fn set_procedure_env(&self, env: crate::procedure_env::ProcedureEnv) {
        *self
            .procedure_env
            .write()
            .unwrap_or_else(|e| e.into_inner()) = env;
    }

    fn procedure_env(&self) -> crate::procedure_env::ProcedureEnv {
        self.procedure_env
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Poll a freshly spawned service until it reports ready.
    ///
    /// Takes the child, not just its port, because the interesting
    /// failure is the service DYING during `__init__` — a plug whose
    /// constructor segfaults the interpreter (the macOS librealsense
    /// backend does it when a device handle is dropped after a failed
    /// USB power-up) takes its own Python error down with it. Polling a
    /// dead port then yields nothing but transport errors, and the only
    /// thing left to report is a timeout that names neither the plug's
    /// error nor the crash. Watching the child turns those 30 seconds
    /// into an immediate, accurate answer.
    async fn wait_for_plug_ready(
        service: &mut ChildProcess,
        plug_name: &str,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let port = service.port;
        poll_plug_ready(port, plug_name, timeout, || {
            match service.process.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                // An errored waitpid is not proof of death: the next
                // tick asks again.
                _ => None,
            }
        })
        .await
    }

    const PLUG_SCRIPT: &'static str = include_str!("../../python/tp_plug.py");

    pub(crate) fn find_plug_script_cli() -> Result<PathBuf, String> {
        if let Some(p) = crate::worker::embedded_script::next_to_exe("tp_plug.py") {
            return Ok(p);
        }
        crate::worker::embedded_script::extract_to_runtime_dir("tp_plug.py", Self::PLUG_SCRIPT)
    }

    /// Start a plug service for a specific plug instance. `slot_id` is
    /// passed through verbatim — the caller already knows which slot
    /// (or `None` for execution-scope plugs); we used to recover it by
    /// stripping a `_<slot>` suffix from `instance_key`, which broke
    /// when the plug key itself contained underscore-suffixed text
    /// matching another slot id.
    pub async fn start_plug_service(
        &self,
        instance_key: String,
        plug_key: String,
        display_name: String,
        plug_config: serde_json::Value,
        slot_id: Option<String>,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<u16, String> {
        {
            let services = self.services.lock().await;
            if services.contains_key(&instance_key) {
                return Err(format!("Plug {}: already running", instance_key));
            }
        }

        let python_script = Self::find_plug_script_cli()?;

        let mut plug_config = plug_config;
        // The plug's `init_timeout:`, not the class's: taken off the config.
        let ready_timeout = plug_config
            .as_object_mut()
            .and_then(|c| c.remove("init_timeout_ms"))
            .and_then(|ms| ms.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(PLUG_READY_TIMEOUT);
        // The plug's own `env:` rides in the config: set on the process over
        // the procedure's, never handed to the class.
        let mut env = self.procedure_env();
        if let Some(serde_json::Value::Object(own)) =
            plug_config.as_object_mut().and_then(|c| c.remove("env"))
        {
            env.extend(
                own.into_iter()
                    .map(|(k, v)| (k, crate::procedure::markers::as_text(&v))),
            );
        }

        log::info!("Resolving Python for plug service: {}", instance_key);
        let python_path = crate::python::resolve_or_walk(&self.python_path, &self.project_dir)
            .await
            .map_err(|e| format!("Cannot resolve Python for plug {instance_key}: {e}"))?;

        log::info!(
            "Starting plug service '{}' with Python: {} (script: {:?})",
            instance_key,
            python_path,
            python_script
        );

        // `slot_id` comes from the caller — see fn doc.

        let plug_key_clone = plug_key.clone();
        let display_name_clone = display_name.clone();
        let slot_id_clone = slot_id.clone();
        let event_sink_clone = Arc::clone(event_sink);

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
                // The config (its `with:` may hold secrets) travels in
                // the environment, which only this user can read, not on
                // the command line, which every user of the machine can.
                "--plug-config-env".to_string(),
                PLUG_CONFIG_ENV.to_string(),
                // The service's parent watchdog keys on this pid. Passing
                // it beats letting the child read getppid(): if this
                // process dies before the child gets there, getppid()
                // already names the reaper and the watchdog never fires.
                "--parent-pid".to_string(),
                std::process::id().to_string(),
            ],
            None,
            {
                env.push((PLUG_CONFIG_ENV.to_string(), plug_config.to_string()));
                env
            },
            Some(Box::new(move |stderr| {
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stderr);
                    let mut buf = Vec::new();
                    while let Ok(Some(line)) =
                        crate::plugs::process::read_line_lossy(&mut reader, &mut buf).await
                    {
                        if let Ok(ServiceLine {
                            service: true,
                            entry,
                        }) = serde_json::from_str::<ServiceLine>(&line)
                        {
                            // The service's own trace (cleanup, shutdown):
                            // not the plug's, kept off its log.
                            log::debug!("[{}] {}", plug_key_clone, entry.message);
                        } else if let Ok(log_entry) = serde_json::from_str::<LogEntry>(&line) {
                            event_sink_clone.emit(&ExecutionEvent::PlugLog(PlugLogEvent {
                                plug_key: plug_key_clone.clone(),
                                plug_name: display_name_clone.clone(),
                                slot_id: slot_id_clone.clone(),
                                // Stage tracked downstream by the consumer
                                // (last seen `plug_status.stage` for this
                                // plug). The service spawns once and lives
                                // across stage transitions, so we don't
                                // know per-line whether we're in setup or
                                // teardown.
                                stage: None,
                                level: log_entry.level.to_lowercase(),
                                message: log_entry.message.clone(),
                                timestamp: Some(log_entry.timestamp.clone()),
                                file: log_entry.file.clone(),
                                line: log_entry.line,
                            }));
                        } else {
                            // Unstructured stderr from the plug subprocess
                            // (a C library, a stray print): not necessarily a
                            // warning. Treat it as info so consumers can
                            // filter it out of an operator-facing view.
                            event_sink_clone.emit(&ExecutionEvent::PlugLog(PlugLogEvent {
                                plug_key: plug_key_clone.clone(),
                                plug_name: display_name_clone.clone(),
                                slot_id: slot_id_clone.clone(),
                                stage: None,
                                level: "info".to_string(),
                                message: line.clone(),
                                timestamp: Some(chrono::Utc::now().to_rfc3339()),
                                file: None,
                                line: None,
                            }));
                        }
                    }
                });
            })),
        )
        .await?;

        let port = service.port;

        if let Err(e) = Self::wait_for_plug_ready(&mut service, &plug_key, ready_timeout).await {
            log::debug!("Plug '{}' initialization failed: {}", instance_key, e);
            service.force_kill().await.ok();
            return Err(e);
        }

        let mut services = self.services.lock().await;
        services.insert(instance_key.clone(), service);
        log::debug!(
            "Inserted {} into services HashMap (Manager ID: {})",
            instance_key,
            self.id
        );
        let service_count = services.len();
        drop(services);

        log::info!(
            "Started plug service {} on port {} (total services: {})",
            instance_key,
            port,
            service_count
        );

        Ok(port)
    }

    /// Stop a specific plug service with proper teardown
    /// Whether a started service's process is still running, or `None`
    /// when nothing is registered under `instance_key` — already
    /// stopped, or never started.
    ///
    /// `try_wait` is a non-blocking waitpid, so this is cheap enough to
    /// poll. The exit status is dropped: callers ask this to decide
    /// whether to tear an instance down, not to collect it. An ERRORED
    /// waitpid reads as alive — it is not proof of death, and the next
    /// poll will ask again — so nothing tears down a working plug on a
    /// transient errno.
    pub async fn is_service_alive(&self, instance_key: &str) -> Option<bool> {
        let mut services = self.services.lock().await;
        let service = services.get_mut(instance_key)?;
        Some(match service.process.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => true,
        })
    }

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

            let result = service
                .graceful_shutdown(
                    || async move {
                        let _ = tokio::time::timeout(
                            tokio::time::Duration::from_secs(5),
                            plug_rpc(port, &PlugRequest::Cleanup),
                        )
                        .await;

                        let _ = tokio::time::timeout(
                            tokio::time::Duration::from_secs(1),
                            plug_rpc(port, &PlugRequest::Shutdown),
                        )
                        .await;

                        Ok(())
                    },
                    3,
                )
                .await;

            log::info!("Stopped plug {} on port {}", plug_name, port);
            result
        } else {
            Err(format!("Plug {}: not running", plug_name))
        }
    }

    /// The cleanup a force kill still gives a plug: its `__del__` (an
    /// instrument's output off) within `grace`, then nothing more is
    /// waited for. A wedged plug gets no further than the timeout.
    /// True when the cleanup answered in time and its `__del__` ran.
    pub async fn try_cleanup(&self, plug_name: &str, grace: std::time::Duration) -> bool {
        let port = match self.services.lock().await.get(plug_name) {
            Some(service) => service.port,
            None => return false,
        };
        matches!(
            tokio::time::timeout(grace, plug_rpc(port, &PlugRequest::Cleanup)).await,
            Ok(Ok(response)) if response.finalized != Some(false)
        )
    }

    /// Force kill a plug service immediately without graceful teardown
    pub async fn force_kill_plug_service(&self, plug_name: &str) -> Result<u16, String> {
        let service = {
            let mut services = self.services.lock().await;
            services.remove(plug_name)
        };

        if let Some(mut service) = service {
            let port = service.port;

            log::debug!("Force killing plug service {} process group", plug_name);

            service.force_kill().await?;

            log::info!("Force killed plug {} on port {}", plug_name, port);
            Ok(port)
        } else {
            Err(format!("Plug {}: not running", plug_name))
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
            log::warn!("Cannot kill plugs: {}", failures.join("; "));
        }

        log::info!("Force killed {} plug services", service_count);

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
            log::debug!("No plug services to stop (Manager ID: {})", self.id);
            return Ok(());
        }

        log::info!("Stopping {} plug services", service_count);

        for plug_name in plug_names {
            if let Err(e) = self.stop_plug_service(&plug_name).await {
                log::warn!("Cannot stop plug {plug_name}: {e}");
            }
        }

        log::info!("Stopped all {} plug services", service_count);
        Ok(())
    }

    /// Get the port for a specific plug service
    pub async fn get_plug_port(&self, plug_name: &str) -> Option<u16> {
        let services = self.services.lock().await;
        services.get(plug_name).map(|service| service.port)
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
            log::debug!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A plug service double: every connection gets `reply` (one line),
    /// or nothing at all when `reply` is `None`, the connection held open
    /// the way a service whose `__init__` holds the GIL leaves it.
    ///
    /// The request is read before the reply, as `tp_plug.py` does. Closing
    /// a socket with unread data resets it on Windows, and the reset
    /// discards the reply the client has not read yet.
    async fn service(reply: Option<&'static str>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut conn, _)) = listener.accept().await {
                match reply {
                    Some(line) => {
                        let mut request = String::new();
                        let _ = BufReader::new(&mut conn).read_line(&mut request).await;
                        let _ = conn.write_all(format!("{line}\n").as_bytes()).await;
                    }
                    None => held.push(conn),
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn a_probe_that_never_answers_is_bounded_by_the_deadline() {
        let port = service(None).await;
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            poll_plug_ready(port, "camera", Duration::from_secs(3), || None),
        )
        .await
        .expect("readiness polling returns although the service never answers");
        let error = result.unwrap_err();
        assert!(error.contains("did not finish within 3s"), "{error}");
    }

    #[tokio::test]
    async fn an_init_error_mentioning_initializing_fails_at_once() {
        let port = service(Some(
            r#"{"success": false, "error": "Plug initialization failed: Error initializing camera"}"#,
        ))
        .await;
        let started = std::time::Instant::now();
        let error = poll_plug_ready(port, "camera", Duration::from_secs(3), || None)
            .await
            .unwrap_err();
        assert!(error.contains("Error initializing camera"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "polled {:?} on a plug that had already failed",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn an_initializing_plug_is_polled_until_the_deadline() {
        let port = service(Some(
            r#"{"success": false, "initializing": true, "error": "Plug initializing... (0.1s elapsed)"}"#,
        ))
        .await;
        let error = poll_plug_ready(port, "camera", Duration::from_secs(1), || None)
            .await
            .unwrap_err();
        assert!(error.contains("did not finish within 1s"), "{error}");
    }
}
