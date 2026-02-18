use command_group::AsyncGroupChild;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tonic::transport::Channel;

#[cfg(unix)]
use nix::sys::signal::{self, Signal};
#[cfg(unix)]
use nix::unistd::Pid;

/// Kill a process group by its PID. Cross-platform.
fn kill_process_group(id: u32) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(id) {
            let _ = signal::kill(Pid::from_raw(-pid), Signal::SIGKILL);
        } else {
            log::error!("PID {} exceeds i32::MAX, cannot kill process group", id);
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .output();
    }
}

#[derive(Debug)]
pub struct GrpcProcess<C> {
    pub port: u16,
    pub process: AsyncGroupChild,
    pub client: C,
}

impl<C> GrpcProcess<C> {
    pub async fn spawn<F, Fut>(
        python_path: &str,
        script_path: PathBuf,
        args: Vec<String>,
        working_dir: Option<&PathBuf>,
        env_vars: Vec<(String, String)>,
        stderr_handler: Option<Box<dyn FnOnce(ChildStderr) + Send>>,
        client_factory: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(u16) -> Fut,
        Fut: std::future::Future<Output = Result<C, String>>,
    {
        let mut cmd = crate::execution::runtime::python::PythonCommandBuilder::new(python_path)
            .unbuffered()
            .with_stdio(Stdio::null(), Stdio::piped(), Stdio::piped())
            .arg(&script_path);

        for arg in args {
            cmd = cmd.arg(&arg);
        }

        for (key, value) in env_vars {
            cmd = cmd.env(&key, &value);
        }

        if let Some(dir) = working_dir {
            cmd = cmd.working_dir(dir);
        }

        let mut process = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn gRPC process: {}", e))?;

        let kill_on_err = |mut p: AsyncGroupChild| {
            if let Some(id) = p.inner().id() {
                kill_process_group(id);
            }
        };

        let stdout = match process.inner().stdout.take() {
            Some(s) => s,
            None => {
                kill_on_err(process);
                return Err("Failed to get stdout from gRPC process".to_string());
            }
        };

        let mut stderr = process.inner().stderr.take();

        // Spawn stderr reader before reading port
        if let Some(stderr_handle) = stderr.take() {
            if let Some(handler) = stderr_handler {
                handler(stderr_handle);
            }
        }

        let mut stdout_reader = BufReader::new(stdout);
        let mut port_line = String::new();
        if let Err(e) = stdout_reader.read_line(&mut port_line).await {
            kill_on_err(process);
            return Err(format!("Failed to read port from gRPC process: {}", e));
        }

        let port = match port_line
            .trim()
            .strip_prefix("GRPC_PORT:")
            .and_then(|s| s.parse::<u16>().ok())
        {
            Some(p) => p,
            None => {
                kill_on_err(process);
                return Err(format!(
                    "Invalid port line from gRPC process: {}\nPython worker may have crashed during startup.\nCheck logs above for Python errors.",
                    port_line.trim()
                ));
            }
        };

        let client = match client_factory(port).await {
            Ok(c) => c,
            Err(e) => {
                kill_on_err(process);
                return Err(e);
            }
        };

        Ok(Self {
            port,
            process,
            client,
        })
    }

    /// Graceful shutdown - sends RPC, polls for exit, kills if needed
    /// Takes &mut self so caller retains ownership for fallback force_kill
    pub async fn graceful_shutdown<F, Fut>(
        &mut self,
        shutdown_rpc: F,
        timeout_secs: u64,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        shutdown_rpc().await.ok();

        let max_wait = Duration::from_secs(timeout_secs);
        let poll_interval = Duration::from_millis(100);
        let mut waited = Duration::ZERO;

        while waited < max_wait {
            match self.process.try_wait() {
                Ok(Some(_status)) => {
                    return Ok(());
                }
                Ok(None) => {
                    tokio::time::sleep(poll_interval).await;
                    waited += poll_interval;
                }
                Err(e) => {
                    log::error!("Error checking process: {}", e);
                    break;
                }
            }
        }

        log::warn!(
            "gRPC process did not exit after {:.1}s, killing process group",
            max_wait.as_secs_f32()
        );

        if let Err(e) = self.process.kill().await {
            log::error!("Failed to kill process group: {}", e);
        }

        // Wait for process to be fully reaped instead of sleeping
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.process.wait()
        ).await;

        Ok(())
    }

    /// Force kill - immediately kills the process group
    /// Takes &mut self so caller retains ownership
    pub async fn force_kill(&mut self) -> Result<(), String> {
        self.process
            .kill()
            .await
            .map_err(|e| format!("Failed to kill process: {}", e))?;
        // Reap the process to prevent zombies
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.process.wait()
        ).await;
        Ok(())
    }
}

impl<C> Drop for GrpcProcess<C> {
    fn drop(&mut self) {
        // Safety net: kill the process group if it wasn't explicitly shut down.
        // Prevents orphaned Python processes when tasks are cancelled or the runtime exits.
        // If the process was already killed/reaped, id() returns None and this is a no-op.
        if let Some(id) = self.process.inner().id() {
            kill_process_group(id);
        }
    }
}

pub async fn connect_grpc_channel(port: u16) -> Result<Channel, String> {
    let endpoint = format!("http://127.0.0.1:{}", port);
    Channel::from_shared(endpoint)
        .map_err(|e| format!("Invalid gRPC endpoint: {}", e))?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|e| format!("Failed to connect to gRPC service: {}", e))
}
