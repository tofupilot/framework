use command_group::AsyncGroupChild;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;

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

/// A managed child process that advertises an NDJSON TCP port on stdout.
#[derive(Debug)]
pub struct ChildProcess {
    pub port: u16,
    pub process: AsyncGroupChild,
}

impl ChildProcess {
    pub async fn spawn(
        python_path: &str,
        script_path: PathBuf,
        args: Vec<String>,
        working_dir: Option<&PathBuf>,
        env_vars: Vec<(String, String)>,
        stderr_handler: Option<Box<dyn FnOnce(ChildStderr) + Send>>,
    ) -> Result<Self, String> {
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
            .map_err(|e| format!("Failed to spawn process: {}", e))?;

        let kill_on_err = |mut p: AsyncGroupChild| {
            if let Some(id) = p.inner().id() {
                kill_process_group(id);
            }
        };

        let stdout = match process.inner().stdout.take() {
            Some(s) => s,
            None => {
                kill_on_err(process);
                return Err("Failed to get stdout from process".to_string());
            }
        };

        let mut stderr = process.inner().stderr.take();

        if let Some(stderr_handle) = stderr.take() {
            if let Some(handler) = stderr_handler {
                handler(stderr_handle);
            }
        }

        let mut stdout_reader = BufReader::new(stdout);
        let mut port_line = String::new();
        if let Err(e) = stdout_reader.read_line(&mut port_line).await {
            kill_on_err(process);
            return Err(format!("Failed to read port from process: {}", e));
        }

        let port = match port_line
            .trim()
            .strip_prefix("NDJSON_PORT:")
            .and_then(|s| s.parse::<u16>().ok())
        {
            Some(p) => p,
            None => {
                kill_on_err(process);
                return Err(format!(
                    "Invalid port line from process: {}\nPython worker may have crashed during startup.\nCheck logs above for Python errors.",
                    port_line.trim()
                ));
            }
        };

        Ok(Self { port, process })
    }

    /// Graceful shutdown by sending SIGTERM, poll for exit, kill if needed.
    pub async fn graceful_shutdown_signal(
        &mut self,
        timeout_secs: u64,
    ) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(id) = self.process.inner().id() {
            if let Ok(pid) = i32::try_from(id) {
                let _ = signal::kill(Pid::from_raw(-pid), Signal::SIGTERM);
            }
        }

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
            "Process did not exit after {:.1}s, killing process group",
            max_wait.as_secs_f32()
        );

        if let Err(e) = self.process.kill().await {
            log::error!("Failed to kill process group: {}", e);
        }

        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.process.wait()
        ).await;

        Ok(())
    }

    /// Graceful shutdown with a custom shutdown function, then poll/kill.
    pub async fn graceful_shutdown<F, Fut>(
        &mut self,
        shutdown_fn: F,
        timeout_secs: u64,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        shutdown_fn().await.ok();

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
            "Process did not exit after {:.1}s, killing process group",
            max_wait.as_secs_f32()
        );

        if let Err(e) = self.process.kill().await {
            log::error!("Failed to kill process group: {}", e);
        }

        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.process.wait()
        ).await;

        Ok(())
    }

    /// Force kill - immediately kills the process group
    pub async fn force_kill(&mut self) -> Result<(), String> {
        self.process
            .kill()
            .await
            .map_err(|e| format!("Failed to kill process: {}", e))?;
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            self.process.wait()
        ).await;
        Ok(())
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if let Some(id) = self.process.inner().id() {
            kill_process_group(id);
        }
    }
}
