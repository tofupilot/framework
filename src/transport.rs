//! NDJSON (newline-delimited JSON) transport over TCP.
//!
//! Simple line-based protocol: serialize to JSON, append newline, flush.
//! Read: read one line, deserialize.

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn write_json_line<T: Serialize>(
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    msg: &T,
) -> Result<(), String> {
    let mut line =
        serde_json::to_string(msg).map_err(|e| format!("JSON serialize error: {}", e))?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("TCP write error: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("TCP flush error: {}", e))?;
    Ok(())
}

// Generic over the buffered reader (not pinned to `TcpStream`) so
// protocol consumers can be tested against in-memory duplex pipes.
pub async fn read_json_line<T: DeserializeOwned, R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<T>, String> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| format!("TCP read error: {}", e))?;
    if n == 0 {
        return Ok(None); // EOF
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|e| format!("JSON parse error: {} (line: {})", e, trimmed))
}
