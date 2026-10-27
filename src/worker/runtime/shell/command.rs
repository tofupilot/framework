//! Unified shell command builder for consistent shell process spawning.
//!
//! Ensures all shell processes are spawned with:
//! - Cross-platform shell detection (bash/sh/powershell/cmd)
//! - Proper working directory resolution (absolute/relative)
//! - Windows console hiding
//! - Consistent error messages

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use command_group::AsyncGroupChild;
use tokio::io::{AsyncRead, AsyncReadExt};

/// How long the pipes are still read once the shell itself has exited.
/// A process the script backgrounded (`./server &`) inherits them and
/// keeps them open for its whole life, so reading to EOF never ends.
pub const PIPE_DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

/// Builder for creating shell process commands with consistent configuration
pub struct ShellCommandBuilder {
    cmd: tokio::process::Command,
    shell_type: String,
}

impl ShellCommandBuilder {
    /// Create a new shell command builder
    ///
    /// Automatically determines shell executable and flags based on shell type.
    /// Supported shells: bash, sh, zsh, powershell, pwsh, cmd
    ///
    /// If shell_type is None, uses platform default (powershell on Windows, sh on Unix)
    pub fn new(shell_type: Option<&str>) -> Result<Self, String> {
        let default_shell = if cfg!(target_os = "windows") {
            "powershell"
        } else {
            "sh"
        };

        let shell_name = shell_type.unwrap_or(default_shell);

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
                return Err(format!(
                    "Invalid shell `{shell_name}`: expected bash, sh, powershell, pwsh or cmd"
                ));
            }
        };

        let mut cmd = tokio::process::Command::new(shell_exe);
        cmd.arg(shell_flag);
        // Same rule as Python workers: a shell phase sees the procedure's
        // own `dashboard:` credentials or none, never the station's.
        crate::procedure_env::strip_inherited(&mut cmd);

        Ok(Self {
            cmd,
            shell_type: shell_name.to_string(),
        })
    }

    /// Add the procedure's environment (the `dashboard:` credentials).
    pub fn envs(mut self, vars: &[(String, String)]) -> Self {
        for (key, value) in vars {
            self.cmd.env(key, value);
        }
        self
    }

    /// Set the command to execute
    pub fn command(mut self, command: &str) -> Self {
        self.cmd.arg(command);
        self
    }

    /// Set the working directory for the shell process
    ///
    /// The directory must exist or spawn() will fail
    pub fn working_dir(mut self, dir: &Path) -> Self {
        self.cmd.current_dir(dir);
        self
    }

    /// Configure stdio (stdin, stdout, stderr)
    pub fn with_stdio(mut self, stdin: Stdio, stdout: Stdio, stderr: Stdio) -> Self {
        self.cmd.stdin(stdin).stdout(stdout).stderr(stderr);
        self
    }

    /// Add an environment variable
    pub fn env<K, V>(mut self, key: K, val: V) -> Self
    where
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.cmd.env(key, val);
        self
    }

    /// Build the final Command, ready to spawn
    pub fn build(self) -> tokio::process::Command {
        self.cmd
    }

    /// Spawn the command in its own process group. The returned
    /// [`ShellChild`] kills that group if it is dropped before the shell
    /// exits (a phase timeout drops the future that owns it).
    pub fn spawn(mut self) -> std::io::Result<ShellChild> {
        use command_group::AsyncCommandGroup;
        let mut group = self.cmd.group();
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            group.creation_flags(CREATE_NO_WINDOW);
        }
        // Windows only: closes the Job Object on drop. command_group
        // ignores it on unix, where `ShellChild`'s Drop does the kill.
        group.kill_on_drop(true);
        let child = group.spawn()?;
        let group_id = child.id();
        if let Some(id) = group_id {
            crate::plugs::process::register_group(id);
        }
        Ok(ShellChild {
            child,
            group_id,
            exited: false,
        })
    }

    /// Get the shell type being used
    pub fn shell_type(&self) -> &str {
        &self.shell_type
    }
}

/// A running shell phase. Registered with the exit sweep while alive;
/// dropped before the shell exits, it kills the whole process group.
#[derive(Debug)]
pub struct ShellChild {
    child: AsyncGroupChild,
    group_id: Option<u32>,
    exited: bool,
}

impl ShellChild {
    /// Process group id (the shell's pid), `None` if the OS gave none.
    pub fn id(&self) -> Option<u32> {
        self.group_id
    }

    /// Wait for the shell and collect what it wrote. Waits for the shell
    /// only, not its whole group: `AsyncGroupChild::wait` also waits for
    /// every background process the script left behind, which may never
    /// exit. Those keep running, as they did before; only the pipe
    /// reading is bounded (`PIPE_DRAIN_AFTER_EXIT`).
    pub async fn wait_with_output(&mut self) -> std::io::Result<Output> {
        let mut stdout = self.child.inner().stdout.take();
        let mut stderr = self.child.inner().stderr.take();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = {
            let reads = async {
                tokio::join!(
                    drain(stdout.as_mut(), &mut out),
                    drain(stderr.as_mut(), &mut err)
                )
            };
            tokio::pin!(reads);
            let child = self.child.inner();
            let exited_first = tokio::select! {
                status = child.wait() => Some(status),
                _ = &mut reads => None,
            };
            match exited_first {
                Some(status) => {
                    let status = status?;
                    let _ = tokio::time::timeout(PIPE_DRAIN_AFTER_EXIT, &mut reads).await;
                    status
                }
                None => child.wait().await?,
            }
        };
        self.exited = true;
        if let Some(id) = self.group_id {
            crate::plugs::process::unregister_group(id);
        }
        Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    }
}

impl Drop for ShellChild {
    fn drop(&mut self) {
        let Some(id) = self.group_id else { return };
        // Windows: the Job Object closes with `child` and takes the tree.
        #[cfg(unix)]
        if !self.exited {
            crate::plugs::process::kill_process_group(id);
        }
        crate::plugs::process::unregister_group(id);
    }
}

/// Read a pipe to EOF. Each chunk is appended as soon as it is read, so
/// dropping this future part way keeps everything read until then.
async fn drain<R: AsyncRead + Unpin>(reader: Option<&mut R>, buf: &mut Vec<u8>) {
    let Some(reader) = reader else { return };
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Resolve working directory from optional path and procedure directory
///
/// Resolution rules:
/// - If `working_directory` is Some(absolute), use it as-is
/// - If `working_directory` is Some(relative), resolve from `procedure_dir`
/// - If `working_directory` is None, use `procedure_dir`
/// - If both None, use current directory
pub fn resolve_working_directory(
    working_directory: Option<&str>,
    procedure_dir: Option<&str>,
) -> PathBuf {
    if let Some(dir) = working_directory {
        let path = Path::new(dir);
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(proc_dir) = procedure_dir {
            Path::new(proc_dir).join(path)
        } else {
            path.to_path_buf()
        }
    } else if let Some(proc_dir) = procedure_dir {
        PathBuf::from(proc_dir)
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}
