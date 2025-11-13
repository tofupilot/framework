use super::worker_protocol::JsonLineProtocol;
use serde::{Deserialize, Serialize};
use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout};

#[derive(Debug)]
pub struct IpcHandler {
    stdin: Option<ChildStdin>,
    reader: Option<BufReader<ChildStdout>>,
}

impl Default for IpcHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl IpcHandler {
    pub fn new() -> Self {
        Self {
            stdin: None,
            reader: None,
        }
    }

    pub fn connect(
        &mut self,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
    ) -> Result<(), String> {
        self.stdin = stdin;
        if let Some(stdout) = stdout {
            self.reader = Some(BufReader::new(stdout));
        }

        if self.stdin.is_none() || self.reader.is_none() {
            return Err("Failed to establish IPC connection".to_string());
        }

        Ok(())
    }

    pub async fn send_json<T: Serialize>(&mut self, value: &T) -> Result<(), String> {
        // Use standard JSON-per-line protocol
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "No stdin available".to_string())?;

        JsonLineProtocol::send_json(stdin, value)
            .await
            .map_err(|e| format!("Protocol error: {}", e))?;

        Ok(())
    }

    pub async fn receive_json<T: for<'a> Deserialize<'a>>(&mut self) -> Result<T, String> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| "No reader available".to_string())?;

        // Use standard JSON-per-line protocol
        JsonLineProtocol::receive_json(reader)
            .await
            .map_err(|e| format!("Protocol error: {}", e))
    }
}
