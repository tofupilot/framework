use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::execution::constants::timeouts;
use crate::execution::job::ResourceMetrics;
use crate::execution::resource_tracker::ResourceTracker;

#[cfg(windows)]
use crate::execution::process_group::ProcessGroup;

#[derive(Debug)]
pub struct ProcessManager {
    worker_id: usize,
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    resource_tracker: ResourceTracker,
    process_pid: Option<u32>,
    #[cfg(windows)]
    process_group: Option<ProcessGroup>,
}

impl ProcessManager {
    pub fn new(worker_id: usize) -> Self {
        Self {
            worker_id,
            process: None,
            stdin: None,
            stdout: None,
            resource_tracker: ResourceTracker::new(),
            process_pid: None,
            #[cfg(windows)]
            process_group: None,
        }
    }

    pub fn get_pid(&self) -> Option<u32> {
        self.process_pid
    }

    fn find_worker_script(&self, app_handle: Option<&AppHandle>) -> Result<PathBuf, String> {
        if let Some(handle) = app_handle {
            // GUI mode: use Tauri to resolve the resource path
            handle.path().resolve("python/job_worker.py", tauri::path::BaseDirectory::Resource)
                .map_err(|e| format!("Failed to resolve job_worker.py: {}", e))
        } else {
            // CLI mode: worker script is relative to the executable
            let exe_path = std::env::current_exe()
                .map_err(|e| format!("Failed to get executable path: {}", e))?;
            let exe_dir = exe_path.parent()
                .ok_or_else(|| "Failed to get executable directory".to_string())?;

            // Try several common locations for the worker script
            let possible_paths = vec![
                exe_dir.join("python/job_worker.py"),  // Relative to exe
                exe_dir.parent().map(|p| p.join("python/job_worker.py")).unwrap_or_else(|| PathBuf::new()),  // One level up
                exe_dir.parent().and_then(|p| p.parent()).map(|p| p.join("python/job_worker.py")).unwrap_or_else(|| PathBuf::new()),  // Two levels up (for cargo run in dev)
                PathBuf::from("src-tauri/python/job_worker.py"),  // Development path
            ];

            for path in possible_paths {
                if path.exists() {
                    return Ok(path);
                }
            }

            Err("Failed to find job_worker.py in any expected location".to_string())
        }
    }

    pub async fn start(&mut self, procedure_dir: &Path, app_handle: Option<&AppHandle>) -> Result<(), String> {
        let abs_procedure_dir = procedure_dir
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize procedure dir: {}", e))?;

        let worker_script = self.find_worker_script(app_handle)?;
        log::debug!("Worker {} using script: {}", self.worker_id, worker_script.display());

        // Validate script exists and is readable
        if !worker_script.exists() {
            return Err(format!("Worker script not found: {}", worker_script.display()));
        }

        let python_cmd = crate::python::resolve_python_executable(app_handle, &abs_procedure_dir).await?;
        log::debug!("Worker {} using Python: {}", self.worker_id, python_cmd);

        let mut command = Command::new(&python_cmd);
        command
            .arg(&worker_script)
            .arg(&abs_procedure_dir)
            .env("WORKER_ID", self.worker_id.to_string())
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&abs_procedure_dir);

        #[cfg(unix)]
        {
            unsafe {
                command.pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                });
            }
        }

        crate::utils::configure_no_window_tokio(&mut command);

        let mut child = command
            .spawn()
            .map_err(|e| format!("Failed to spawn Python process: {}", e))?;

        if let Some(pid) = child.id() {
            self.process_pid = Some(pid);

            #[cfg(windows)]
            {
                let pg = ProcessGroup::new()?;
                pg.add_process(pid).await?;
                self.process_group = Some(pg);
                crate::execution::cli_output::debug(format!(
                    "Worker {} added to job object (PID: {})",
                    self.worker_id, pid
                ));
            }

            crate::execution::cli_output::debug(format!(
                "Worker {} started Python process with PID: {}",
                self.worker_id, pid
            ));
        } else {
            crate::execution::cli_output::warning(format!(
                "Worker {} failed to get PID for Python process",
                self.worker_id
            ));
        }

        self.stdin = child.stdin.take();
        self.stdout = child.stdout.take();

        // Handle stderr in a separate task
        if let Some(stderr) = child.stderr.take() {
            self.spawn_stderr_reader(stderr);
        }

        self.process = Some(child);

        Ok(())
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    fn spawn_stderr_reader(&self, stderr: ChildStderr) {
        let worker_id = self.worker_id;
        tokio::spawn(async move {
            let mut stderr_reader = BufReader::new(stderr);
            let mut line = String::new();
            while stderr_reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    crate::execution::cli_output::print_section(
                        crate::execution::cli_output::Section::Worker,
                        format!("[{}] Python: {}", worker_id, trimmed)
                    );
                    log::warn!("Worker {} Python stderr: {}", worker_id, trimmed);
                }
                line.clear();
            }
        });
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        // Wait for process to exit gracefully
        if let Some(mut process) = self.process.take() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(timeouts::WORKER_SHUTDOWN_TIMEOUT_SECS),
                process.wait(),
            )
            .await;

            // Force kill if still running
            let _ = process.kill().await;
        }

        Ok(())
    }

    pub async fn force_kill(&mut self) -> Result<(), String> {
        #[cfg(windows)]
        {
            if let Some(ref pg) = self.process_group {
                pg.kill_all().await?;
            }
        }

        if let Some(mut process) = self.process.take() {
            if let Some(_pid) = self.process_pid {
                #[cfg(unix)]
                {
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid as NixPid;

                    let pgid = NixPid::from_raw(-(_pid as i32));
                    let _ = kill(pgid, Signal::SIGKILL);
                }

                let wait_result =
                    tokio::time::timeout(std::time::Duration::from_millis(200), process.wait())
                        .await;

                match wait_result {
                    Ok(_) => {}
                    Err(_) => {
                        tokio::spawn(async move {
                            let _ = process.wait().await;
                        });
                    }
                }
            } else {
                let _ = process.kill().await;
            }
        }

        #[cfg(windows)]
        {
            self.process_group = None;
        }

        Ok(())
    }

    pub fn send_sigterm(&mut self) -> Result<(), String> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid as NixPid;

            if let Some(pid) = self.process_pid {
                let nix_pid = NixPid::from_raw(pid as i32);
                kill(nix_pid, Signal::SIGTERM)
                    .map_err(|e| format!("Failed to send SIGTERM to PID {}: {}", pid, e))?;
                crate::execution::cli_output::debug(format!(
                    "Sent SIGTERM to Python process PID: {}",
                    pid
                ));
                Ok(())
            } else {
                Err("No process PID available".to_string())
            }
        }
        #[cfg(not(unix))]
        {
            Err("SIGTERM not supported on non-Unix platforms".to_string())
        }
    }

    pub fn start_resource_tracking(&mut self) {
        self.resource_tracker.start_tracking(self.process_pid);
    }

    pub fn collect_resource_metrics(&mut self) -> Option<ResourceMetrics> {
        self.resource_tracker.collect_metrics()
    }
}
