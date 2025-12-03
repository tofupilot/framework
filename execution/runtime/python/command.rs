//! Unified Python command builder for consistent process spawning.
//!
//! Ensures all Python processes are spawned with:
//! - AppImage environment variable cleanup (PYTHONHOME/PYTHONPATH)
//! - Unbuffered output for real-time logs
//! - Proper process group management for clean shutdown
//! - Windows console hiding

use std::path::Path;
use std::process::Stdio;
use command_group::{AsyncCommandGroup, AsyncGroupChild};

/// Builder for creating Python process commands with consistent configuration
pub struct PythonCommandBuilder {
    cmd: tokio::process::Command,
}

impl PythonCommandBuilder {
    /// Create a new Python command builder
    ///
    /// Automatically removes PYTHONHOME and PYTHONPATH to prevent AppImage pollution
    pub fn new(python_path: &str) -> Self {
        let mut cmd = tokio::process::Command::new(python_path);

        // CRITICAL: Remove AppImage environment variables that break venv Python
        // When running inside AppImage, these point to AppImage's mounted filesystem
        // which causes venv Python to load incomplete stdlib
        cmd.env_remove("PYTHONHOME").env_remove("PYTHONPATH");

        Self { cmd }
    }

    /// Enable unbuffered Python output for real-time logging
    ///
    /// Without this, Python buffers stdout/stderr which delays log visibility
    pub fn unbuffered(mut self) -> Self {
        self.cmd.env("PYTHONUNBUFFERED", "1");
        self.cmd.env("PYTHONIOENCODING", "utf-8");
        self
    }

    /// Set the working directory for the Python process
    pub fn working_dir(mut self, dir: &Path) -> Self {
        self.cmd.current_dir(dir);
        self
    }

    /// Configure stdio (stdin, stdout, stderr)
    pub fn with_stdio(mut self, stdin: Stdio, stdout: Stdio, stderr: Stdio) -> Self {
        self.cmd.stdin(stdin).stdout(stdout).stderr(stderr);
        self
    }

    /// Add command line arguments
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.cmd.args(args);
        self
    }

    /// Add a single command line argument
    pub fn arg<S: AsRef<std::ffi::OsStr>>(mut self, arg: S) -> Self {
        self.cmd.arg(arg);
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
    /// Returns AsyncGroupChild which wraps tokio::process::Child
    /// Process groups are always enabled for proper cleanup
    /// kill_on_drop ensures processes are killed if handle is dropped
    pub fn spawn(mut self) -> std::io::Result<AsyncGroupChild> {
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let mut group = self.cmd.group();
            group.creation_flags(CREATE_NO_WINDOW);
            group.kill_on_drop(true);
            group.spawn()
        }
        #[cfg(not(windows))]
        {
            let mut group = self.cmd.group();
            group.kill_on_drop(true);
            group.spawn()
        }
    }
}

/// Builder for synchronous Python commands (using std::process::Command)
pub struct PythonCommandBuilderSync {
    cmd: std::process::Command,
}

impl PythonCommandBuilderSync {
    /// Create a new synchronous Python command builder
    pub fn new(python_path: &str) -> Self {
        let mut cmd = std::process::Command::new(python_path);

        // CRITICAL: Remove AppImage environment variables
        cmd.env_remove("PYTHONHOME").env_remove("PYTHONPATH");

        Self { cmd }
    }

    /// Enable unbuffered Python output
    pub fn unbuffered(mut self) -> Self {
        self.cmd.env("PYTHONUNBUFFERED", "1");
        self.cmd.env("PYTHONIOENCODING", "utf-8");
        self
    }

    /// Add command line arguments
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.cmd.args(args);
        self
    }

    /// Add a single command line argument
    pub fn arg<S: AsRef<std::ffi::OsStr>>(mut self, arg: S) -> Self {
        self.cmd.arg(arg);
        self
    }

    /// Build the final Command
    pub fn build(self) -> std::process::Command {
        self.cmd
    }

    /// Convenience method: run the command and get output
    pub fn output(mut self) -> std::io::Result<std::process::Output> {
        self.cmd.output()
    }
}
