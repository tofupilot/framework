use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

/// Global map of UI response channels keyed by request_id.
///
/// # Timeout Handling
/// Note: Channels are created when a UI request is sent and removed when:
/// - A response is received from the frontend
/// - The phase times out (handled by orchestrator timeout mechanism)
///
/// If the frontend never responds AND the phase has no timeout, the channel
/// will remain in memory. This is acceptable as native UI phases are typically
/// user-facing and have configured timeouts.
pub static UI_RESPONSE_CHANNELS: Lazy<
    Arc<Mutex<HashMap<String, oneshot::Sender<HashMap<String, String>>>>>,
> = Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Close all pending UI response channels.
/// This unblocks any phases waiting for UI input by dropping the senders,
/// causing the receivers to get a RecvError.
pub async fn close_all_ui_channels() {
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    let count = channels.len();
    if count > 0 {
        log::debug!("Closing {} pending UI response channels", count);
        channels.clear();
    }
}
