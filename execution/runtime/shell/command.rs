//! Unified shell command builder for consistent shell process spawning.
//!
//! Ensures all shell processes are spawned with:
//! - Cross-platform shell detection (bash/sh/powershell/cmd)
//! - Proper working directory resolution (absolute/relative)
//! - Windows console hiding
//! - Consistent error messages

use std::path::{Path, PathBuf};
use std::process::Stdio;

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
                return Err(format!("Unsupported shell type: {}", shell_name));
            }
        };

        let mut cmd = tokio::process::Command::new(shell_exe);
        cmd.arg(shell_flag);

        Ok(Self {
            cmd,
            shell_type: shell_name.to_string(),
        })
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

    /// Convenience method: spawn the command immediately
    /// kill_on_drop ensures processes are killed if handle is dropped
    pub fn spawn(mut self) -> std::io::Result<command_group::AsyncGroupChild> {
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            use command_group::AsyncCommandGroup;
            let mut group = self.cmd.group();
            group.creation_flags(CREATE_NO_WINDOW);
            group.kill_on_drop(true);
            group.spawn()
        }
        #[cfg(not(windows))]
        {
            use command_group::AsyncCommandGroup;
            let mut group = self.cmd.group();
            group.kill_on_drop(true);
            group.spawn()
        }
    }

    /// Get the shell type being used
    pub fn shell_type(&self) -> &str {
        &self.shell_type
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
