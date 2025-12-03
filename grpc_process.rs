use command_group::AsyncGroupChild;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tonic::transport::Channel;

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

        let stdout = process
            .inner()
            .stdout
            .take()
            .ok_or("Failed to get stdout from gRPC process")?;

        let mut stderr = process.inner().stderr.take();

        // Spawn stderr reader before reading port
        if let Some(stderr_handle) = stderr.take() {
            if let Some(handler) = stderr_handler {
                handler(stderr_handle);
            }
        }

        let mut stdout_reader = BufReader::new(stdout);
        let mut port_line = String::new();
        stdout_reader
            .read_line(&mut port_line)
            .await
            .map_err(|e| format!("Failed to read port from gRPC process: {}", e))?;

        let port = port_line
            .trim()
            .strip_prefix("GRPC_PORT:")
            .ok_or_else(|| {
                format!(
                    "Invalid port line from gRPC process: {}\nPython worker may have crashed during startup.\nCheck logs above for Python errors.",
                    port_line.trim()
                )
            })?
            .parse::<u16>()
            .map_err(|e| format!("Failed to parse port: {}", e))?;

        let client = client_factory(port).await?;

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

        tokio::time::sleep(Duration::from_millis(500)).await;

        Ok(())
    }

    /// Force kill - immediately kills the process group
    /// Takes &mut self so caller retains ownership
    pub async fn force_kill(&mut self) -> Result<(), String> {
        self.process
            .kill()
            .await
            .map_err(|e| format!("Failed to kill process: {}", e))
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
