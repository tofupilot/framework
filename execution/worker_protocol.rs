use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// Protocol errors
#[derive(Debug)]
pub enum ProtocolError {
    Io(std::io::Error),
    Serialization(serde_json::Error),
    ConnectionClosed,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Io(e) => write!(f, "IO error: {}", e),
            ProtocolError::Serialization(e) => write!(f, "Serialization error: {}", e),
            ProtocolError::ConnectionClosed => write!(f, "Connection closed"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<std::io::Error> for ProtocolError {
    fn from(err: std::io::Error) -> Self {
        ProtocolError::Io(err)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(err: serde_json::Error) -> Self {
        ProtocolError::Serialization(err)
    }
}

/// Line-based JSON protocol - standard for text-based IPC
pub struct JsonLineProtocol;

impl JsonLineProtocol {
    pub async fn send_json<W, T>(writer: &mut W, value: &T) -> Result<(), ProtocolError>
    where
        W: AsyncWriteExt + Unpin,
        T: Serialize,
    {
        let json = serde_json::to_string(value)?;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn receive_json<R, T>(reader: &mut R) -> Result<T, ProtocolError>
    where
        R: AsyncBufReadExt + Unpin,
        T: for<'a> Deserialize<'a>,
    {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;

        if bytes_read == 0 {
            return Err(ProtocolError::ConnectionClosed);
        }

        // Debug: Log the raw JSON before deserialization
        crate::execution::cli_output::debug(format!("🔍 Raw JSON received: {}", line.trim()));

        match serde_json::from_str(&line) {
            Ok(value) => Ok(value),
            Err(e) => {
                crate::execution::cli_output::print_section(
                    crate::execution::cli_output::Section::Error,
                    format!("❌ Failed to deserialize JSON: {}", e),
                );
                crate::execution::cli_output::debug(format!("   Raw line was: {}", line));
                Err(ProtocolError::Serialization(e))
            }
        }
    }
}
