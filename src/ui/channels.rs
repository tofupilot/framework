use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use tokio::sync::{oneshot, Mutex, RwLock};

use super::types::UiComponent;

/// A prompt awaiting its operator response: the oneshot that resolves
/// the waiting phase, plus the component spec that rendered the form.
/// Carrying the spec here lets the submission chokepoint validate the
/// operator's values against exactly what was asked (reject-on-submit,
/// TP-760) with the same lifecycle as the channel itself — registered
/// together, removed together, no separate registry to leak or drift.
pub struct PendingUi {
    pub sender: oneshot::Sender<HashMap<String, String>>,
    pub components: Vec<UiComponent>,
    /// Unit identity fields of an identify form (serial, part, revision,
    /// batch, sub-unit serials): the server's charset applies to them on
    /// top of any authored pattern, so the submit gate checks it too.
    pub identity_keys: Vec<String>,
}

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
pub static UI_RESPONSE_CHANNELS: LazyLock<Arc<Mutex<HashMap<String, PendingUi>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Who a pending UI request belongs to, so a stop can close only the
/// prompts it cuts short.
#[derive(Debug, Clone)]
struct ChannelOwner {
    /// `None` = shared phase.
    slot_id: Option<String>,
    /// Teardown prompts are the work that runs BECAUSE of a stop ("remove
    /// the DUT, confirm"): a stop leaves them waiting, only a force kill
    /// closes them.
    teardown: bool,
}

/// Owner of each pending UI request. Kept beside `UI_RESPONSE_CHANNELS`
/// rather than in its key: the CLI and Studio resolve responses by bare
/// `request_id`, and identify-time prompts register straight into the
/// map without going through the worker.
static UI_CHANNEL_OWNERS: LazyLock<Arc<Mutex<HashMap<String, ChannelOwner>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Cancellation reason per request, written by a slot-scoped close so the
/// slot's cancelled prompts quote *their* slot's cause. Read via
/// `take_cancel_reason`, which falls back to `CANCEL_REASON`.
static CANCEL_REASON_BY_REQUEST: LazyLock<Arc<RwLock<HashMap<String, String>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

/// Requests closed because nobody can answer them (a headless run), as
/// opposed to a stop. A prompt-only phase has no body to fail, so its
/// worker reads this to end ERROR instead of STOP.
static UNANSWERABLE_REQUESTS: LazyLock<Arc<RwLock<std::collections::HashSet<String>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(std::collections::HashSet::new())));

/// Register a phase's UI response channel. Workers use this rather than
/// inserting into `UI_RESPONSE_CHANNELS` directly so the owner index stays
/// in step. Both maps are locked together, in the order every close takes
/// them, so a close never lands between the two inserts.
pub async fn register_ui_channel(
    request_id: String,
    slot_id: Option<String>,
    teardown: bool,
    tx: oneshot::Sender<HashMap<String, String>>,
    components: Vec<UiComponent>,
) {
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    let mut owners = UI_CHANNEL_OWNERS.lock().await;
    owners.insert(request_id.clone(), ChannelOwner { slot_id, teardown });
    channels.insert(
        request_id,
        PendingUi {
            sender: tx,
            components,
            identity_keys: Vec::new(),
        },
    );
}

/// Forget a request the worker is done with (answered, dismissed, or
/// closed under it).
pub async fn unregister_ui_channel(request_id: &str) {
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    channels.remove(request_id);
    UI_CHANNEL_OWNERS.lock().await.remove(request_id);
    drop(channels);
    unregister_press_sink(request_id);
}

/// A shown screen whose panel has buttons (`UiPanel::events`): the
/// phase's job loop takes the presses. Separate from
/// `UI_RESPONSE_CHANNELS` because a display-only screen has no response
/// channel, and its buttons still press.
struct PressSink {
    names: Vec<String>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Start of the current one-second window, and the presses in it.
    window: std::time::Instant,
    count: u32,
}

static PRESS_SINKS: LazyLock<std::sync::Mutex<HashMap<String, PressSink>>> =
    LazyLock::new(Default::default);

/// Presses taken per screen per second. Far over a hand on a button;
/// past it a surface is flooding the phase's socket, and the rest of the
/// second is dropped.
pub const MAX_PRESSES_PER_SECOND: u32 = 20;

fn press_sinks() -> std::sync::MutexGuard<'static, HashMap<String, PressSink>> {
    PRESS_SINKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Route the presses of `names` on the screen `request_id` to `tx` until
/// [`unregister_ui_channel`] forgets the screen.
pub fn register_press_sink(
    request_id: &str,
    names: Vec<String>,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    if names.is_empty() {
        return;
    }
    press_sinks().insert(
        request_id.to_string(),
        PressSink {
            names,
            tx,
            window: std::time::Instant::now(),
            count: 0,
        },
    );
}

fn unregister_press_sink(request_id: &str) {
    press_sinks().remove(request_id);
}

/// What became of a press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Delivered,
    /// No such screen showing, or a name its panel has no button for.
    NoSuchButton,
    /// Over [`MAX_PRESSES_PER_SECOND`] on that screen.
    TooFast,
}

/// A press of the panel button `name` on `request_id`.
pub fn press(request_id: &str, name: &str) -> Press {
    let mut sinks = press_sinks();
    let Some(sink) = sinks
        .get_mut(request_id)
        .filter(|s| s.names.iter().any(|n| n == name))
    else {
        return Press::NoSuchButton;
    };
    if sink.window.elapsed() >= std::time::Duration::from_secs(1) {
        sink.window = std::time::Instant::now();
        sink.count = 0;
    }
    if sink.count >= MAX_PRESSES_PER_SECOND {
        return Press::TooFast;
    }
    sink.count += 1;
    match sink.tx.send(name.to_string()) {
        Ok(()) => Press::Delivered,
        // The job is gone: as good as no screen.
        Err(_) => Press::NoSuchButton,
    }
}

/// Why a prompt the operator cancelled (Esc in the terminal UI) never got
/// its answer. The phase waiting on it fails with this message.
pub const OPERATOR_CANCELLED: &str = "The operator cancelled the prompt";

/// Close one pending UI channel nobody can answer, recording `reason` so
/// the phase waiting on it fails with that message rather than the
/// generic one, and fails even when it is a prompt-only phase.
pub async fn close_ui_channel_with_reason(request_id: &str, reason: String) {
    // Locks in the order every close takes them, held throughout: a submit
    // landing between the check and the write would leave a reason for a
    // prompt that was answered.
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    let mut owners = UI_CHANNEL_OWNERS.lock().await;
    // Nothing pending (a display-only screen, a prompt already answered):
    // no waiter will ever read the reason, so do not record one.
    let Some(pending) = channels.remove(request_id) else {
        return;
    };
    owners.remove(request_id);
    {
        let mut reasons = CANCEL_REASON_BY_REQUEST.write().await;
        // Same rule as the slot close: an earlier, more specific cause
        // (a slot stop) wins, and the phase then reads as stopped.
        if let std::collections::hash_map::Entry::Vacant(e) = reasons.entry(request_id.to_string())
        {
            e.insert(reason);
            UNANSWERABLE_REQUESTS
                .write()
                .await
                .insert(request_id.to_string());
        }
    }
    // Dropping the sender wakes the waiting worker: only once its reason
    // is recorded, so it always finds it.
    drop(pending);
}

/// Whether a request was closed because nobody can answer it (no surface,
/// stdin closed, `--ui-timeout`), not by a stop. Not consumed.
pub async fn is_unanswerable(request_id: &str) -> bool {
    UNANSWERABLE_REQUESTS.read().await.contains(request_id)
}

/// The reason a request was closed as unanswerable, consumed. `None` for
/// a request closed by a stop (run-wide or slot), which keeps STOP.
pub async fn take_unanswerable_reason(request_id: &str) -> Option<String> {
    if !UNANSWERABLE_REQUESTS.write().await.remove(request_id) {
        return None;
    }
    CANCEL_REASON_BY_REQUEST.write().await.remove(request_id)
}

/// Close the pending UI channels of one slot only, recording `reason` for
/// each so the cancelled phases can quote it. Prompts of other slots, of
/// shared phases and of teardown phases keep waiting.
pub async fn close_slot_ui_channels_with_reason(slot_id: &str, reason: String) {
    close_cut_channels(reason, |owner| owner.slot_id.as_deref() == Some(slot_id)).await;
}

/// Close every prompt a run-wide stop cuts short, recording `reason` for
/// each. Teardown prompts keep waiting: the teardown is what runs because
/// of the stop. Identify prompts and any request registered without an
/// owner are closed too.
pub async fn close_stopped_ui_channels_with_reason(reason: String) {
    {
        let mut r = CANCEL_REASON.write().await;
        if r.is_none() {
            *r = Some(reason.clone());
        }
    }
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    let mut owners = UI_CHANNEL_OWNERS.lock().await;
    let request_ids: Vec<String> = channels
        .keys()
        .filter(|id| !owners.get(*id).is_some_and(|o| o.teardown))
        .cloned()
        .collect();
    if request_ids.is_empty() {
        return;
    }
    log::debug!("Closing {} pending UI response channels", request_ids.len());
    for id in &request_ids {
        channels.remove(id);
        owners.remove(id);
    }
}

async fn close_cut_channels(reason: String, matches: impl Fn(&ChannelOwner) -> bool) {
    let mut channels = UI_RESPONSE_CHANNELS.lock().await;
    let mut owners = UI_CHANNEL_OWNERS.lock().await;
    let request_ids: Vec<String> = owners
        .iter()
        .filter(|(_, o)| !o.teardown && matches(o))
        .map(|(id, _)| id.clone())
        .collect();
    if request_ids.is_empty() {
        return;
    }
    log::debug!("Closing {} pending UI response channels", request_ids.len());
    {
        let mut reasons = CANCEL_REASON_BY_REQUEST.write().await;
        for id in &request_ids {
            reasons.entry(id.clone()).or_insert_with(|| reason.clone());
        }
    }
    for id in &request_ids {
        channels.remove(id);
        owners.remove(id);
    }
}

/// The reason a request's channel was closed under the waiting worker:
/// the slot-scoped one when there is one, else the run-wide one. `None`
/// means the channel closed for an unrelated reason (operator timeout,
/// agent stdin disconnect, …).
pub async fn take_cancel_reason(request_id: &str) -> Option<String> {
    UNANSWERABLE_REQUESTS.write().await.remove(request_id);
    if let Some(r) = CANCEL_REASON_BY_REQUEST.write().await.remove(request_id) {
        return Some(r);
    }
    CANCEL_REASON.read().await.clone()
}

/// Last cancellation reason set when `close_all_ui_channels` was called
/// with a populated reason. Workers waiting on a UI response read this
/// after their `rx.await` returns Err so the cancelled phase's error
/// message names *why* the run was aborted (e.g. "Run aborted by phase
/// 'capture_rail_settle': TypeError: ...") instead of the generic
/// "cancelled or timed out". `None` means the channel closed for an
/// unrelated reason — operator timeout, agent stdin disconnect, etc.
pub static CANCEL_REASON: LazyLock<Arc<RwLock<Option<String>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

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
    UI_CHANNEL_OWNERS.lock().await.clear();
}

/// Variant of `close_all_ui_channels` that records the reason. Workers
/// pick it up via `CANCEL_REASON` to surface a real cause on the
/// cancelled UI phase's `error` field.
pub async fn close_all_ui_channels_with_reason(reason: String) {
    {
        let mut r = CANCEL_REASON.write().await;
        // Don't clobber an earlier, more specific reason (e.g. plug
        // init failure) with a later generic one.
        if r.is_none() {
            *r = Some(reason);
        }
    }
    close_all_ui_channels().await;
}

/// Reset the cancel-reason slot. Called at run start so a stale reason
/// from a previous run can't leak into the next.
pub async fn clear_cancel_reason() {
    let mut r = CANCEL_REASON.write().await;
    *r = None;
    CANCEL_REASON_BY_REQUEST.write().await.clear();
    UNANSWERABLE_REQUESTS.write().await.clear();
    // No prompt of a previous run can still be waiting; drop any index
    // entry a consumer resolved without going through `unregister`.
    UI_CHANNEL_OWNERS.lock().await.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registries are process-wide; tests asserting on their contents
    /// take turns.
    static REGISTRY: Mutex<()> = Mutex::const_new(());

    /// A press reaches the phase only for a screen showing, a button its
    /// panel has, and under the rate; forgetting the screen stops it.
    #[tokio::test]
    async fn presses_reach_the_screen_that_has_the_button() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        register_press_sink("press-1", vec!["tare".into()], tx);
        assert_eq!(press("press-1", "tare"), Press::Delivered);
        assert_eq!(rx.recv().await.as_deref(), Some("tare"));
        assert_eq!(
            press("press-1", "zero"),
            Press::NoSuchButton,
            "not one of its buttons"
        );
        assert_eq!(
            press("press-2", "tare"),
            Press::NoSuchButton,
            "no such screen"
        );
        for _ in 1..MAX_PRESSES_PER_SECOND {
            assert_eq!(press("press-1", "tare"), Press::Delivered);
        }
        assert_eq!(press("press-1", "tare"), Press::TooFast);
        unregister_ui_channel("press-1").await;
        assert_eq!(press("press-1", "tare"), Press::NoSuchButton, "screen gone");
    }

    /// Slot A's stop closes A's prompt with A's reason and leaves B's
    /// prompt waiting; B's worker later finds no reason of its own.
    #[tokio::test]
    async fn slot_close_spares_the_other_slot() {
        let _turn = REGISTRY.lock().await;
        let (tx_a, rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        register_ui_channel("req-a".into(), Some("A".into()), false, tx_a, Vec::new()).await;
        register_ui_channel("req-b".into(), Some("B".into()), false, tx_b, Vec::new()).await;

        close_slot_ui_channels_with_reason("A", "Slot 'A' aborted by phase 'x'".into()).await;

        assert!(rx_a.await.is_err(), "A's prompt is cancelled");
        assert_eq!(
            take_cancel_reason("req-a").await.as_deref(),
            Some("Slot 'A' aborted by phase 'x'")
        );
        assert!(
            matches!(rx_b.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "B's prompt keeps waiting"
        );
        assert_eq!(take_cancel_reason("req-b").await, None);
        assert!(UI_RESPONSE_CHANNELS.lock().await.contains_key("req-b"));

        unregister_ui_channel("req-b").await;
        assert!(
            UI_CHANNEL_OWNERS.lock().await.is_empty(),
            "no index entry outlives its prompt"
        );
    }

    /// A headless close marks the prompt unanswerable (its phase fails);
    /// a slot stop does not (its phase stops), and a later headless close
    /// of the same prompt does not replace the stop's reason.
    #[tokio::test]
    async fn unanswerable_close_is_told_apart_from_a_stop() {
        let _turn = REGISTRY.lock().await;
        let (tx, rx) = oneshot::channel();
        register_ui_channel("req-headless".into(), None, false, tx, Vec::new()).await;
        close_ui_channel_with_reason("req-headless", "nobody can answer".into()).await;
        assert!(rx.await.is_err());
        assert_eq!(
            take_unanswerable_reason("req-headless").await.as_deref(),
            Some("nobody can answer")
        );
        assert_eq!(
            take_unanswerable_reason("req-headless").await,
            None,
            "consumed"
        );

        let (tx, rx) = oneshot::channel();
        register_ui_channel(
            "req-stopped".into(),
            Some("S".into()),
            false,
            tx,
            Vec::new(),
        )
        .await;
        close_slot_ui_channels_with_reason("S", "Slot 'S' aborted".into()).await;
        close_ui_channel_with_reason("req-stopped", "nobody can answer".into()).await;
        assert!(rx.await.is_err());
        assert_eq!(take_unanswerable_reason("req-stopped").await, None);
        assert_eq!(
            take_cancel_reason("req-stopped").await.as_deref(),
            Some("Slot 'S' aborted"),
            "the earlier reason is kept"
        );
    }
}
