//! Budget-driven degradation for oversized `StationEvent`s.
//!
//! Why this exists
//! ---------------
//! The CLI publishes every `StationEvent` as ONE inbound WebSocket frame
//! to Centrifugo, which caps client frames at `message_size_limit`
//! (65536 bytes by default, not overridden in our deployment). Over that,
//! Centrifugo closes the connection with close code 1009 -- and 1009 is
//! terminal in the client SDK (`reconnect: false`), while the station
//! daemon's broker supervisor only connects once at boot. Net effect of a
//! single oversized event: the station goes offline on the dashboard and
//! stops streaming for the rest of the process's life, silently. The run
//! itself survives and still uploads over HTTP, which is exactly why the
//! failure reads as a flaky dashboard rather than as a bug.
//!
//! Several variants carry unbounded operator data and can cross the line:
//! `UiRequest` (an inline `data:` image), `PhaseLog` / `PlugLog`
//! (a verbose dump), `PhaseComplete` (per-attempt logs and measurement
//! values), `MeasurementUpdate` (a spectrum or trace), `RunCrashed`
//! (a long traceback).
//!
//! So instead of publishing a frame we know is fatal, we publish a
//! degraded copy: the prompt survives without its image, the log line
//! survives truncated. Degradation always beats a dead station.
//!
//! Wire compatibility
//! ------------------
//! Every reduction here only removes or shortens EXISTING fields, or
//! swaps a component for another already-known `ComponentType`. No new
//! field, no new variant -- so the generated TypeScript
//! (`packages/shared/types/generated/station-protocol.ts`) is unchanged
//! and no dashboard/kiosk work is needed to consume this.

use crate::{ComponentType, ComponentValue, StationEvent, UiComponent};

/// Largest serialized `StationEvent` the CLI will put on the wire.
///
/// Centrifugo's limit is 65536 for the whole publish frame, which wraps
/// the event: `{"id":N,"publish":{"channel":"station_health:<uuid>",
/// "data":<event>}}` -- about 150 bytes of envelope. 60000 keeps ~8%
/// of headroom on top of that for a longer channel name or a future
/// field, and is still far above any healthy event (measured: a typical
/// `phase_complete` is 300-700 bytes, an `identify_request` ~500).
pub const MAX_EVENT_BYTES: usize = 60_000;

/// An image component's source is either a short relative path resolved
/// by the host (`captures/board.png`, served from the procedure bundle) or
/// an inline `data:` URI carrying the whole picture. Anything past this is
/// unambiguously the second kind — no filesystem path is 512 bytes long.
const IMAGE_SRC_PATH_MAX: usize = 512;

/// For NON-image components there is no type signal, so size is the only
/// one: past this, a string is a pasted blob rather than a label, a
/// description or a textarea default. Deliberately generous — anything
/// below it that still needs shrinking is handled by text truncation,
/// which preserves the beginning of the content instead of discarding it.
const INLINE_PAYLOAD_MAX: usize = 8_192;

/// Bytes reserved for the "… [N bytes, truncated]" marker appended to a
/// shortened string. The real marker is ~45 bytes; 64 covers the widest
/// byte count without the marker itself busting the allowance.
const MARKER_RESERVE: usize = 64;

impl StationEvent {
    /// Serialized size on the wire, in bytes.
    pub fn wire_size(&self) -> usize {
        serde_json::to_vec(self).map(|v| v.len()).unwrap_or(0)
    }

    /// Return a copy that serializes to at most `budget` bytes, degrading
    /// content as needed, or `None` when even the most aggressive
    /// reduction can't get there (the caller must then drop the event
    /// rather than kill the connection with it).
    ///
    /// Returns the event unchanged when it already fits, so callers can
    /// use this unconditionally.
    ///
    /// Known limitation: only inline payloads and free text are
    /// reducible. An event that is large by STRUCTURE — a `RunStarted`
    /// with hundreds of `PhasePlan`s, a `UiRequest` with hundreds of
    /// options — has nothing this function knows how to cut without
    /// corrupting what reducers key on, so it comes back `None` and the
    /// caller drops it (logged). The same applies to a few text fields
    /// pass 2 deliberately leaves alone because degrading them is not
    /// worth handling their (improbable) oversized shapes: `UiOption`
    /// labels/values, and a component's `prefix` / `suffix` / `pattern`
    /// (a truncated regex is a corrupted validator, not a shorter one).
    /// Dropping is safe here — the run continues and the station's
    /// local operator UI still shows the full prompt; only the
    /// dashboard misses the event. If any of this ever bites in
    /// practice, the fix belongs at the emitter (cap the plan size),
    /// not here.
    pub fn shrink_to_fit(&self, budget: usize) -> Option<Self> {
        if self.wire_size() <= budget {
            return Some(self.clone());
        }

        // Pass 1 -- drop the pathological payloads (inline images, pasted
        // blobs) that account for essentially all real overruns. Keeps
        // every text field intact.
        //
        // Strictly largest-first, re-measuring after each: a prompt that
        // busts the budget because of ONE 470 KB image must not also lose
        // the 5 KB icon sitting next to it. We stop the moment the event
        // fits, so nothing is sacrificed that didn't need to be.
        //
        // A step that reports work but sheds no bytes would select the
        // same payload again on the next round, forever — this publish
        // path must never be able to hang the daemon, so any round
        // without measurable progress falls through to pass 2 instead.
        let mut event = self.clone();
        loop {
            let before = event.wire_size();
            if before <= budget {
                return Some(event);
            }
            if !event.strip_largest_inline_payload() || event.wire_size() >= before {
                break;
            }
        }

        // Pass 2 -- truncate free text, halving the per-field allowance
        // until the whole event fits. Starting at half the budget means a
        // single dominant field converges in one or two rounds; the floor
        // stops us from returning something with no content left.
        let mut allowance = budget / 2;
        while allowance >= 128 {
            let mut candidate = event.clone();
            candidate.truncate_text(allowance);
            if candidate.wire_size() <= budget {
                return Some(candidate);
            }
            allowance /= 2;
        }

        // Pass 3 -- a `phase_complete` can be big on the measurement AXIS
        // (thousands of rows, or verbose names) where pass 2 has nothing
        // safe to cut: names key the consumer's row-merge, so truncating
        // them would fork rows instead of shrinking them. Shed the whole
        // list instead. This leans on a consumer contract: the
        // `phase_complete` reducer treats an EMPTY batched list as "keep
        // the measurements accumulated from live `measurement_update`
        // events" (see packages/operator-ui/src/state/run-state.ts; pinned by
        // packages/operator-ui/src/state/shrink-contract.test.ts), so the rows
        // usually survive anyway -- and the phase OUTCOME, the thing this
        // event exists to deliver, always does. Other variants have no
        // equivalent recovery path, so only `phase_complete` gets this
        // tier.
        if matches!(self, Self::PhaseComplete { .. }) {
            let mut candidate = event;
            candidate.truncate_text(128);
            if let Self::PhaseComplete { measurements, .. } = &mut candidate {
                measurements.clear();
            }
            if candidate.wire_size() <= budget {
                return Some(candidate);
            }
        }

        None
    }

    /// Degrade the single largest inline payload in this event and report
    /// whether anything was found to degrade.
    ///
    /// One payload per call, so the caller can re-measure between each and
    /// stop as soon as the event fits — the difference between "lost the
    /// 470 KB image" and "lost the 470 KB image AND the small icon that
    /// was never the problem".
    fn strip_largest_inline_payload(&mut self) -> bool {
        // A panel is one payload, dropped whole: half an HTML file is a
        // broken screen, and without it the components render as usual.
        if let Self::RunStarted { panel, .. } = self {
            return panel.take().is_some();
        }
        if let Self::UiRequest {
            panel, components, ..
        } = self
        {
            let panel_len = panel.as_ref().map_or(0, |p| p.html.len());
            let largest_component = components
                .iter()
                .flatten()
                .map(component_payload_len)
                .max()
                .unwrap_or(0);
            if panel_len > 0 && panel_len >= largest_component {
                *panel = None;
                return true;
            }
        }
        match self {
            Self::UiRequest { components, .. } | Self::IdentifyRequest { components, .. } => {
                let Some(components) = components.as_mut() else {
                    return false;
                };
                let largest = components
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (i, component_payload_len(c)))
                    .filter(|(_, len)| *len > 0)
                    .max_by_key(|(_, len)| *len);
                match largest {
                    Some((index, _)) => {
                        degrade_component(&mut components[index]);
                        true
                    }
                    None => false,
                }
            }
            Self::UiUpdate { data, .. } => {
                let Some(payload) = data.as_ref() else {
                    return false;
                };
                if payload.len() <= INLINE_PAYLOAD_MAX {
                    return false;
                }
                *data = degrade_ui_update_payload(payload);
                true
            }
            Self::MeasurementUpdate { measurement, .. } => {
                degrade_measured_value(&mut measurement.measured_value)
            }
            Self::PhaseComplete { measurements, .. } => {
                let largest = measurements
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (i, measured_value_len(&m.measured_value)))
                    .filter(|(_, len)| *len > INLINE_PAYLOAD_MAX)
                    .max_by_key(|(_, len)| *len);
                match largest {
                    Some((index, _)) => {
                        degrade_measured_value(&mut measurements[index].measured_value)
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// Truncate every unbounded text field to `allowance` bytes.
    fn truncate_text(&mut self, allowance: usize) {
        match self {
            Self::PhaseLog { message, .. } | Self::PlugLog { message, .. } => {
                truncate_in_place(message, allowance);
            }
            Self::RunCrashed { error, .. } => {
                truncate_in_place(error, allowance);
            }
            Self::PhaseComplete { error, logs, measurements, .. } => {
                if let Some(error) = error.as_mut() {
                    truncate_in_place(error, allowance);
                }
                // Pass 1 only strips blob-sized values (> INLINE_PAYLOAD_MAX
                // each). An event that is big because of MANY medium values
                // reaches here instead; shed those against the shrinking
                // allowance so the phase outcome survives. Validator
                // expressions are display-only (per the protocol doc on
                // `ValidatorResult`), so truncating them keeps the row and
                // its verdict intact. Measurement NAMES are deliberately
                // left alone: consumers merge rows by name, and a truncated
                // name would fork the row instead of shrinking it -- a
                // name-heavy event falls through to pass 3.
                for measurement in measurements.iter_mut() {
                    degrade_measured_value_over(&mut measurement.measured_value, allowance);
                    for validator in measurement.validators.iter_mut() {
                        truncate_in_place(&mut validator.expression, allowance);
                    }
                }
                // Log lines are the usual bulk here. Truncate each line,
                // then drop from the MIDDLE if there are still too many:
                // the first lines carry setup context and the last ones
                // carry the failure, so the middle is what an operator
                // misses least.
                for line in logs.iter_mut() {
                    truncate_in_place(&mut line.message, allowance);
                }
                let max_lines = (allowance / 256).max(4);
                if logs.len() > max_lines {
                    let head = max_lines / 2;
                    let tail = max_lines - head;
                    let dropped = logs.len() - max_lines;
                    let mut kept: Vec<_> = logs[..head].to_vec();
                    let mut marker = logs[head].clone();
                    marker.message = format!("… [{dropped} log lines dropped for live streaming]");
                    marker.file = None;
                    marker.line = None;
                    kept.push(marker);
                    kept.extend_from_slice(&logs[logs.len() - tail..]);
                    *logs = kept;
                }
            }
            Self::UiRequest { components, .. } | Self::IdentifyRequest { components, .. } => {
                // Every free-text field a YAML author controls, not just
                // `value`: a prompt bulked by a verbose label, placeholder
                // or pre-filled default (each under pass 1's blob floor)
                // must degrade field by field, not be dropped whole -- a
                // dropped `ui_request` is an operator prompt the dashboard
                // never shows.
                if let Some(components) = components.as_mut() {
                    for component in components.iter_mut() {
                        for text in [
                            component.label.as_mut(),
                            component.description.as_mut(),
                            component.placeholder.as_mut(),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            truncate_in_place(text, allowance);
                        }
                        for value in [&mut component.value, &mut component.default_value] {
                            match value {
                                Some(ComponentValue::String(value)) => {
                                    truncate_in_place(value, allowance);
                                }
                                Some(ComponentValue::Array(items)) => {
                                    truncate_array(items, allowance);
                                }
                                _ => {}
                            }
                        }
                        // Option thumbnails each below pass 1's path/data
                        // threshold can still be the bulk in AGGREGATE
                        // (many options, each with a small inline image).
                        // Against a shrinking allowance they stop looking
                        // like paths and go.
                        if let Some(options) = component.options.as_mut() {
                            for option in options.iter_mut() {
                                if option.image.as_ref().is_some_and(|i| i.len() > allowance) {
                                    option.image = None;
                                }
                            }
                        }
                    }
                }
            }
            // `UiUpdate` is deliberately absent: its `data` is a JSON
            // document, and raw text truncation would corrupt it into
            // something the client can't fold. Pass 1 already handles the
            // oversized case JSON-safely (`degrade_ui_update_payload`);
            // an update that still doesn't fit after that falls through
            // to the drop path, which at least keeps the wire valid.
            _ => {}
        }
    }
}

/// Swap an oversized component for something renderable.
///
/// An `image` whose source is a multi-KB `data:` URI becomes a `text`
/// component saying so -- the operator gets an explanation in place of
/// the picture, and the prompt (and its inputs) still works. Any other
/// component just loses its oversized value.
fn degrade_component(component: &mut UiComponent) {
    // Radio / checklist image grids carry their images per option.
    if let Some(options) = component.options.as_mut() {
        for option in options.iter_mut() {
            if option
                .image
                .as_ref()
                .is_some_and(|i| i.len() > IMAGE_SRC_PATH_MAX)
            {
                option.image = None;
            }
        }
    }

    let payload_len = component_value_payload_len(component);
    if payload_len == 0 {
        return;
    }

    if matches!(component.component_type, ComponentType::Image) {
        *component = UiComponent {
            key: component.key.clone(),
            label: component.label.clone(),
            required: false,
            value: Some(ComponentValue::String(format!(
                "⚠ Image not streamed: {} exceeds the live-stream limit. \
                 It is still displayed on the station's local operator UI. \
                 Ship it with the procedure and reference it by relative \
                 path to show it here.",
                human_bytes(payload_len)
            ))),
            ..UiComponent::new(ComponentType::Text)
        };
        return;
    }

    for value in [&mut component.value, &mut component.default_value] {
        match value {
            Some(ComponentValue::String(value)) => {
                truncate_in_place(value, INLINE_PAYLOAD_MAX);
            }
            Some(ComponentValue::Array(items)) => {
                truncate_array(items, INLINE_PAYLOAD_MAX);
            }
            _ => {}
        }
    }
}

/// Keep the leading entries of an array value that fit in `max_total`
/// serialized bytes and drop the rest. No marker entry is added: these
/// arrays carry selection keys the dashboard may match verbatim, so a
/// synthetic entry would be worse than a silent cut — the publish path
/// already logs that the event was degraded.
fn truncate_array(items: &mut Vec<String>, max_total: usize) {
    let mut total = 0;
    let mut keep = 0;
    for item in items.iter() {
        total += item.len() + 3;
        if total > max_total {
            break;
        }
        keep += 1;
    }
    items.truncate(keep);
}

/// A `ui_update` body is JSON (`{"id":…,"value":…}` for `set_value`).
/// Truncating the raw string would produce invalid JSON the client can't
/// fold in, so rebuild a well-formed payload with the value replaced.
/// Returns `None` when the body isn't a shape we recognise, or when the
/// bulk sits outside `value` and the rewrite therefore sheds nothing --
/// an update with no body is inert, which is the point.
fn degrade_ui_update_payload(payload: &str) -> Option<String> {
    let mut parsed: serde_json::Value = serde_json::from_str(payload).ok()?;
    let object = parsed.as_object_mut()?;
    let original = payload.len();
    if let Some(properties) = object
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        return degrade_set_properties(properties).then(|| serde_json::to_string(&parsed).ok())?;
    }
    object.insert(
        "value".to_string(),
        serde_json::Value::String(format!(
            "⚠ Value not streamed: {} exceeds the live-stream limit.",
            human_bytes(original)
        )),
    );
    let rebuilt = serde_json::to_string(&parsed).ok()?;
    if rebuilt.len() > INLINE_PAYLOAD_MAX {
        return None;
    }
    Some(rebuilt)
}

/// A `set_properties` body (`{"id":…,"properties":{…}}`, from
/// `ui.set`) is bulky through its `options`: a run-time list of hundreds
/// of orders, or option thumbnails. Called once per pass-1 round, so each
/// call makes one step of progress and the caller re-measures: first the
/// option images go (the operator can still choose), then the list loses
/// its tail by halves (leading entries survive, same posture as
/// `truncate_array`; a cut list still submits a valid choice, an absent
/// update leaves the dashboard with an empty select the engine would
/// reject every answer to). Returns whether anything changed; `false`
/// means the bulk is elsewhere and the body is dropped.
fn degrade_set_properties(properties: &mut serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(options) = properties
        .get_mut("options")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut stripped = false;
    for option in options.iter_mut() {
        if let Some(o) = option.as_object_mut() {
            if o.remove("image").is_some() {
                stripped = true;
            }
        }
    }
    if stripped {
        return true;
    }
    if options.len() > 1 {
        options.truncate(options.len() / 2);
        return true;
    }
    false
}

/// Replace an oversized measurement value with a readable stand-in,
/// keeping the JSON valid so consumers don't have to special-case it.
/// Returns whether anything was replaced.
fn degrade_measured_value(value: &mut Option<serde_json::Value>) -> bool {
    degrade_measured_value_over(value, INLINE_PAYLOAD_MAX)
}

/// Same as [`degrade_measured_value`] with an explicit floor. Pass 2
/// calls this with its shrinking per-field allowance: a `phase_complete`
/// whose bulk is MANY medium values (none individually blob-sized, so
/// pass 1 has nothing to strip) must lose those values rather than be
/// dropped whole — the phase outcome row matters more than any value,
/// and the full data still reaches the server over HTTP.
fn degrade_measured_value_over(value: &mut Option<serde_json::Value>, floor: usize) -> bool {
    let len = measured_value_len(value);
    if len <= floor {
        return false;
    }
    *value = Some(serde_json::Value::String(format!(
        "⚠ Value not streamed: {} exceeds the live-stream limit.",
        human_bytes(len)
    )));
    true
}

fn measured_value_len(value: &Option<serde_json::Value>) -> usize {
    value
        .as_ref()
        .and_then(|v| serde_json::to_vec(v).ok())
        .map(|v| v.len())
        .unwrap_or(0)
}

/// Size of this component's own oversized payload (`value` /
/// `default_value`), or 0 when it carries none.
///
/// The eligibility floor depends on the component type: for an `image`,
/// the type itself says the value is a picture source, so anything past a
/// plausible file path is inline data. Every other type has no such
/// signal, so only a clearly-blob-sized string qualifies — smaller ones
/// are text, and text gets truncated rather than discarded.
fn component_value_payload_len(component: &UiComponent) -> usize {
    let floor = if matches!(component.component_type, ComponentType::Image) {
        IMAGE_SRC_PATH_MAX
    } else {
        INLINE_PAYLOAD_MAX
    };
    [&component.value, &component.default_value]
        .into_iter()
        .map(inline_len)
        .filter(|len| *len > floor)
        .max()
        .unwrap_or(0)
}

/// Largest oversized payload anywhere in this component, including the
/// per-option images of a radio / checklist image grid.
fn component_payload_len(component: &UiComponent) -> usize {
    let options = component
        .options
        .iter()
        .flatten()
        .filter_map(|o| o.image.as_ref())
        .map(|i| i.len())
        .filter(|len| *len > IMAGE_SRC_PATH_MAX)
        .max()
        .unwrap_or(0);
    component_value_payload_len(component).max(options)
}

fn inline_len(value: &Option<ComponentValue>) -> usize {
    match value {
        Some(ComponentValue::String(s)) => s.len(),
        Some(ComponentValue::Array(items)) => items.iter().map(|i| i.len() + 3).sum(),
        _ => 0,
    }
}

/// Truncate `s` to at most `max` bytes INCLUDING the marker, cutting on a
/// UTF-8 char boundary so the result is never invalid text.
fn truncate_in_place(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let original = s.len();
    let mut cut = max.saturating_sub(MARKER_RESERVE);
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(&format!(
        " … [{} truncated for live streaming]",
        human_bytes(original)
    ));
}

fn human_bytes(bytes: usize) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhaseLogLine, RunMeasurement};

    fn image_component(bytes: usize) -> UiComponent {
        UiComponent {
            key: "image".to_string(),
            value: Some(ComponentValue::String(format!(
                "data:image/png;base64,{}",
                "A".repeat(bytes)
            ))),
            ..UiComponent::new(ComponentType::Image)
        }
    }

    fn text_component(key: &str) -> UiComponent {
        UiComponent {
            key: key.to_string(),
            value: Some(ComponentValue::String("Acknowledge the capture".to_string())),
            ..UiComponent::new(ComponentType::Text)
        }
    }

    fn ui_request(components: Vec<UiComponent>) -> StationEvent {
        StationEvent::UiRequest {
            request_id: "8d49d26e-8095-4873-b712-3757b4f60c33".to_string(),
            phase_key: "4_capture_review".to_string(),
            slot_id: None,
            components: Some(components),
            requires_input: true,
            timestamp: Some("2026-07-30T15:03:45Z".to_string()),
            execution_id: Some("91962b89-b5b2-4bbe-826f-6753ce110c3c".to_string()),
            job_id: None,
            panel: None,
        }
    }

    fn with_panel(event: StationEvent, bytes: usize) -> StationEvent {
        let StationEvent::UiRequest {
            request_id,
            phase_key,
            slot_id,
            components,
            requires_input,
            timestamp,
            execution_id,
            job_id,
            ..
        } = event
        else {
            unreachable!()
        };
        StationEvent::UiRequest {
            request_id,
            phase_key,
            slot_id,
            components,
            requires_input,
            timestamp,
            execution_id,
            job_id,
            panel: Some(crate::UiPanel {
                html: format!("<div>{}</div>", "x".repeat(bytes)),
                sha256: "0".repeat(64),
                events: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_panel_that_fits_is_kept() {
        let event = with_panel(ui_request(vec![text_component("message")]), 40_000);
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("fits");
        let StationEvent::UiRequest { panel, .. } = shrunk else { unreachable!() };
        assert!(panel.is_some());
    }

    /// The panel is never truncated: dropped whole, the prompt keeps its
    /// components and renders the standard screen.
    #[test]
    fn an_oversized_panel_is_dropped_whole() {
        let event = with_panel(ui_request(vec![text_component("message")]), 70_000);
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("fits without the panel");
        let StationEvent::UiRequest { panel, components, .. } = shrunk else { unreachable!() };
        assert!(panel.is_none());
        assert_eq!(components.unwrap().len(), 1);
    }

    /// Largest first: an inline image bigger than the panel goes before it.
    #[test]
    fn a_bigger_image_goes_before_the_panel() {
        let event = with_panel(ui_request(vec![image_component(70_000)]), 20_000);
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("fits");
        let StationEvent::UiRequest { panel, .. } = shrunk else { unreachable!() };
        assert!(panel.is_some(), "the panel was not the problem");
    }

    fn run_started_with_panel(bytes: usize) -> StationEvent {
        serde_json::from_value(serde_json::json!({
            "type": "run_started",
            "procedure_id": "p",
            "execution_id": "e",
            "phases": [{ "key": "charge", "name": "Charge", "stage": "main" }],
            "panel": { "html": format!("<div>{}</div>", "x".repeat(bytes)), "sha256": "0".repeat(64) },
        }))
        .unwrap()
    }

    /// The run-level panel rides on `run_started`: kept when it fits,
    /// dropped whole when it does not, the run itself intact.
    #[test]
    fn the_run_panel_is_kept_or_dropped_whole() {
        let fits = run_started_with_panel(40_000).shrink_to_fit(MAX_EVENT_BYTES).expect("fits");
        let StationEvent::RunStarted { panel, .. } = fits else { unreachable!() };
        assert!(panel.is_some());

        let over = run_started_with_panel(70_000).shrink_to_fit(MAX_EVENT_BYTES).expect("fits without it");
        let StationEvent::RunStarted { panel, phases, .. } = over else { unreachable!() };
        assert!(panel.is_none());
        assert_eq!(phases.len(), 1);
    }

    #[test]
    fn small_event_is_returned_unchanged() {
        let event = ui_request(vec![text_component("message")]);
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("fits");
        assert_eq!(shrunk.wire_size(), event.wire_size());
    }

    /// The real incident: a 362 KB PNG inlined as a data URI produced a
    /// 483511-byte `ui_request`, 7.4x over Centrifugo's limit.
    #[test]
    fn oversized_inline_image_is_replaced_and_prompt_survives() {
        let event = ui_request(vec![
            image_component(482_840),
            text_component("message"),
            UiComponent {
                key: "acknowledge".to_string(),
                label: Some("Acknowledge".to_string()),
                ..UiComponent::new(ComponentType::Switch)
            },
        ]);
        assert!(event.wire_size() > 400_000);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);

        let StationEvent::UiRequest { components, requires_input, .. } = shrunk else {
            panic!("variant changed");
        };
        let components = components.expect("components kept");
        // The input the operator must act on is still there -- that is the
        // whole point of degrading instead of dropping.
        assert!(requires_input);
        assert_eq!(components.len(), 3);
        assert!(matches!(components[2].component_type, ComponentType::Switch));
        // The image became an explanatory text component.
        assert!(matches!(components[0].component_type, ComponentType::Text));
        let Some(ComponentValue::String(value)) = &components[0].value else {
            panic!("no explanation");
        };
        assert!(value.contains("not streamed"));
        assert!(value.contains("MB") || value.contains("KB"));
    }

    /// Only the payload that actually busts the budget may be sacrificed.
    /// A prompt carrying one huge photo and one small inline icon must
    /// come out with the icon intact.
    #[test]
    fn a_small_inline_image_survives_next_to_an_oversized_one() {
        let icon = UiComponent {
            key: "icon".to_string(),
            ..image_component(4_096)
        };
        let icon_src = icon.value.clone();
        let event = ui_request(vec![
            icon,
            UiComponent {
                key: "photo".to_string(),
                ..image_component(482_840)
            },
            text_component("message"),
        ]);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiRequest { components, .. } = shrunk else {
            panic!("variant changed");
        };
        let components = components.expect("components kept");

        // The small icon is untouched: same type, same bytes.
        assert!(matches!(components[0].component_type, ComponentType::Image));
        assert_eq!(components[0].value, icon_src);
        // Only the oversized one was swapped for an explanation.
        assert!(matches!(components[1].component_type, ComponentType::Text));
    }

    #[test]
    fn oversized_log_message_is_truncated_not_dropped() {
        let event = StationEvent::PhaseLog {
            phase_key: "2_rail_check".to_string(),
            attempt: 1,
            slot_id: None,
            level: "INFO".to_string(),
            message: "X".repeat(200_000),
            timestamp: None,
            file: Some("phases/rails.py".to_string()),
            line: Some(42),
            execution_id: None,
        };
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::PhaseLog { message, level, file, .. } = shrunk else {
            panic!("variant changed");
        };
        // Context survives; only the body is shortened.
        assert_eq!(level, "INFO");
        assert_eq!(file.as_deref(), Some("phases/rails.py"));
        assert!(message.contains("truncated for live streaming"));
        assert!(message.starts_with("XXX"));
    }

    #[test]
    fn truncation_never_splits_a_utf8_char() {
        let event = StationEvent::PhaseLog {
            phase_key: "p".to_string(),
            attempt: 1,
            slot_id: None,
            level: "INFO".to_string(),
            // 3-byte chars: a naive byte cut lands mid-character.
            message: "é€漢".repeat(40_000),
            timestamp: None,
            file: None,
            line: None,
            execution_id: None,
        };
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        // Serializing at all proves the string stayed valid UTF-8.
        assert!(serde_json::to_string(&shrunk).is_ok());
    }

    #[test]
    fn phase_complete_keeps_head_and_tail_of_its_logs() {
        let line = |i: usize| PhaseLogLine {
            level: "INFO".to_string(),
            message: format!("line {i} {}", "y".repeat(400)),
            timestamp: None,
            file: None,
            line: None,
        };
        let event = StationEvent::PhaseComplete {
            phase_key: "1_supply_check".to_string(),
            name: "supply_check".to_string(),
            outcome: "PASS".to_string(),
            measurements: vec![],
            slot_id: None,
            attempt: 1,
            started_at: None,
            ended_at: None,
            duration_ms: Some(12),
            error: None,
            logs: (0..1_000).map(line).collect(),
            execution_id: None,
            run_metadata: Default::default(),
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::PhaseComplete { logs, outcome, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(outcome, "PASS");
        assert!(logs.first().unwrap().message.starts_with("line 0"));
        assert!(logs.last().unwrap().message.starts_with("line 999"));
        assert!(logs.iter().any(|l| l.message.contains("log lines dropped")));
    }

    #[test]
    fn oversized_measurement_value_becomes_a_stand_in() {
        let trace: Vec<serde_json::Value> = (0..20_000)
            .map(|i| serde_json::json!(i as f64 * 0.001))
            .collect();
        let event = StationEvent::MeasurementUpdate {
            phase_key: "2_rail_check".to_string(),
            attempt: 1,
            slot_id: None,
            measurement: RunMeasurement {
                name: "spectrum".to_string(),
                outcome: "PASS".to_string(),
                measured_value: Some(serde_json::Value::Array(trace)),
                units: Some("V".to_string()),
                validators: vec![],
                aggregations: vec![],
                low: None,
                high: None,
            },
            execution_id: None,
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::MeasurementUpdate { measurement, .. } = shrunk else {
            panic!("variant changed");
        };
        // Name, outcome and units survive -- the row still renders.
        assert_eq!(measurement.name, "spectrum");
        assert_eq!(measurement.outcome, "PASS");
        assert_eq!(measurement.units.as_deref(), Some("V"));
        assert!(matches!(
            measurement.measured_value,
            Some(serde_json::Value::String(ref s)) if s.contains("not streamed")
        ));
    }

    #[test]
    fn ui_update_payload_stays_valid_json() {
        let event = StationEvent::UiUpdate {
            phase_key: "4_capture_review".to_string(),
            action: "set_value".to_string(),
            data: Some(format!(
                r#"{{"id":"image","value":"data:image/png;base64,{}"}}"#,
                "A".repeat(300_000)
            )),
            job_id: None,
            slot_id: None,
            execution_id: None,
        };
        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiUpdate { data, action, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(action, "set_value");
        let body: serde_json::Value =
            serde_json::from_str(&data.expect("body kept")).expect("valid JSON");
        // The client folds on `id`; keeping it means the update still
        // targets the right component instead of being silently inert.
        assert_eq!(body["id"], "image");
        assert!(body["value"].as_str().unwrap().contains("not streamed"));
    }

    /// Regression: a `phase_complete` whose bulk is MANY medium values
    /// (each under `INLINE_PAYLOAD_MAX`, so pass 1 strips nothing) was
    /// dropped whole, and the dashboard lost the phase OUTCOME — worse
    /// than losing the values. Pass 2 must shed the values instead.
    #[test]
    fn phase_complete_with_many_medium_measurements_keeps_its_outcome() {
        let measurement = |i: usize| RunMeasurement {
            name: format!("point_{i}"),
            outcome: "PASS".to_string(),
            measured_value: Some(serde_json::Value::Array(
                (0..500).map(|j| serde_json::json!(j as f64 * 0.001)).collect(),
            )),
            units: Some("V".to_string()),
            validators: vec![],
            aggregations: vec![],
            low: None,
            high: None,
        };
        let event = StationEvent::PhaseComplete {
            phase_key: "3_sweep".to_string(),
            name: "sweep".to_string(),
            outcome: "PASS".to_string(),
            measurements: (0..30).map(measurement).collect(),
            slot_id: None,
            attempt: 1,
            started_at: None,
            ended_at: None,
            duration_ms: Some(950),
            error: None,
            logs: vec![],
            execution_id: None,
            run_metadata: Default::default(),
        };
        // The premise: over budget, but no single value is blob-sized.
        assert!(event.wire_size() > MAX_EVENT_BYTES);
        let StationEvent::PhaseComplete { measurements, .. } = &event else {
            unreachable!()
        };
        assert!(measurements
            .iter()
            .all(|m| measured_value_len(&m.measured_value) <= INLINE_PAYLOAD_MAX));

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::PhaseComplete { outcome, measurements, .. } = shrunk else {
            panic!("variant changed");
        };
        // The row still renders: outcome, every name, every verdict.
        assert_eq!(outcome, "PASS");
        assert_eq!(measurements.len(), 30);
        assert!(measurements.iter().all(|m| m.outcome == "PASS"));
        assert!(measurements.iter().any(|m| matches!(
            m.measured_value,
            Some(serde_json::Value::String(ref s)) if s.contains("not streamed")
        )));
    }

    /// A budget no degradation can reach must yield `None` so the caller
    /// drops the event instead of publishing a frame that kills the link.
    #[test]
    fn impossible_budget_returns_none() {
        let event = ui_request(vec![image_component(482_840)]);
        assert!(event.shrink_to_fit(64).is_none());
    }

    /// Regression: an `Array` value was counted as an inline payload but
    /// never degraded, so pass 1 selected the same component forever and
    /// the publish path hung the daemon. It must terminate, and the value
    /// must stay an array (a cut selection, not a swapped type).
    #[test]
    fn oversized_array_value_is_cut_not_looped() {
        let items: Vec<String> = (0..2_000)
            .map(|i| format!("option-{i}-{}", "x".repeat(40)))
            .collect();
        let event = ui_request(vec![UiComponent {
            key: "checklist".to_string(),
            value: Some(ComponentValue::Array(items)),
            ..UiComponent::new(ComponentType::Checklist)
        }]);
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiRequest { components, .. } = shrunk else {
            panic!("variant changed");
        };
        let Some(ComponentValue::Array(kept)) = &components.expect("components kept")[0].value
        else {
            panic!("value is no longer an array");
        };
        // Leading entries survive; only the tail is sacrificed.
        assert!(!kept.is_empty());
        assert!(kept.len() < 2_000);
        assert_eq!(kept[0], format!("option-0-{}", "x".repeat(40)));
    }

    /// A `set_properties` update bulked by its options (a run-time list
    /// of hundreds of orders) keeps a prefix of the list, never an empty
    /// body: the dashboard can still pick a valid order, where a dropped
    /// update would leave it an empty select.
    #[test]
    fn set_properties_bulked_by_options_keeps_a_prefix() {
        let options: Vec<String> = (0..3_000)
            .map(|i| format!(r#"{{"value":"MO-{i}","label":"Order MO-{i}, pack 48V"}}"#))
            .collect();
        let event = StationEvent::UiUpdate {
            phase_key: "1_pick_order".to_string(),
            action: "set_properties".to_string(),
            data: Some(format!(
                r#"{{"id":"mo","properties":{{"placeholder":"Choose","options":[{}]}}}}"#,
                options.join(",")
            )),
            job_id: None,
            slot_id: None,
            execution_id: None,
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiUpdate { data, action, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(action, "set_properties");
        let body: serde_json::Value = serde_json::from_str(&data.expect("body kept")).unwrap();
        assert_eq!(body["id"], "mo");
        assert_eq!(body["properties"]["placeholder"], "Choose");
        let kept = body["properties"]["options"].as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 3_000);
        assert_eq!(kept[0]["value"], "MO-0");
    }

    /// Option thumbnails go before any option does.
    #[test]
    fn set_properties_sheds_option_images_before_options() {
        let image = format!("data:image/png;base64,{}", "A".repeat(2_000));
        let options: Vec<String> = (0..50)
            .map(|i| format!(r#"{{"value":"v{i}","label":"V {i}","image":"{image}"}}"#))
            .collect();
        let event = StationEvent::UiUpdate {
            phase_key: "1_pick".to_string(),
            action: "set_properties".to_string(),
            data: Some(format!(
                r#"{{"id":"variant","properties":{{"options":[{}]}}}}"#,
                options.join(",")
            )),
            job_id: None,
            slot_id: None,
            execution_id: None,
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        let StationEvent::UiUpdate { data, .. } = shrunk else {
            panic!("variant changed");
        };
        let body: serde_json::Value = serde_json::from_str(&data.unwrap()).unwrap();
        let kept = body["properties"]["options"].as_array().unwrap();
        assert_eq!(kept.len(), 50);
        assert!(kept.iter().all(|o| o.get("image").is_none()));
    }

    /// Regression: a `ui_update` whose bulk sits outside `value` gained
    /// nothing from the value rewrite, so pass 1 reprocessed it forever.
    /// The body must be dropped (inert update) rather than looped on.
    #[test]
    fn ui_update_bulk_outside_value_is_dropped_not_looped() {
        let event = StationEvent::UiUpdate {
            phase_key: "4_capture_review".to_string(),
            action: "set_options".to_string(),
            data: Some(format!(
                r#"{{"id":"table","rows":"{}"}}"#,
                "A".repeat(100_000)
            )),
            job_id: None,
            slot_id: None,
            execution_id: None,
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiUpdate { data, action, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(action, "set_options");
        assert!(data.is_none());
    }

    /// Regression: a prompt bulked by free text OUTSIDE `value` /
    /// `description` — a verbose label, placeholder, or many pre-filled
    /// defaults each under pass 1's blob floor — was dropped whole, so
    /// the dashboard never showed an operator prompt it was waiting on.
    /// Pass 2 must shed those fields instead.
    #[test]
    fn prompt_bulked_by_labels_and_defaults_survives() {
        let field = |i: usize| UiComponent {
            key: format!("field_{i}"),
            label: Some("L".repeat(4_000)),
            placeholder: Some("P".repeat(4_000)),
            default_value: Some(ComponentValue::String("d".repeat(7_000))),
            ..UiComponent::new(ComponentType::Textarea)
        };
        let event = ui_request((0..12).map(field).collect());
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiRequest { components, requires_input, .. } = shrunk else {
            panic!("variant changed");
        };
        assert!(requires_input);
        let components = components.expect("components kept");
        // Every field the operator must fill in is still present, with
        // the head of each text preserved.
        assert_eq!(components.len(), 12);
        assert!(components[0].label.as_ref().unwrap().starts_with('L'));
        assert!(matches!(
            components[0].default_value,
            Some(ComponentValue::String(ref s)) if s.starts_with('d')
        ));
    }

    /// Regression: option thumbnails each under `IMAGE_SRC_PATH_MAX`
    /// (so pass 1 sees only "paths") can be the bulk in aggregate; the
    /// whole prompt was dropped. Pass 2 must shed the images and keep
    /// the options selectable.
    #[test]
    fn prompt_bulked_by_many_small_option_images_survives() {
        let option = |i: usize| crate::UiOption {
            label: format!("option {i}"),
            value: format!("opt_{i}"),
            image: Some(format!("data:image/png;base64,{}", "A".repeat(400))),
        };
        let event = ui_request(vec![UiComponent {
            key: "grid".to_string(),
            options: Some((0..200).map(option).collect()),
            ..UiComponent::new(ComponentType::Radio)
        }]);
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::UiRequest { components, .. } = shrunk else {
            panic!("variant changed");
        };
        let components = components.expect("components kept");
        let options = components[0].options.as_ref().expect("options kept");
        // Selection keys and labels intact; only the thumbnails went.
        assert_eq!(options.len(), 200);
        assert_eq!(options[0].value, "opt_0");
        assert!(options.iter().all(|o| o.image.is_none()));
    }

    /// Regression: a `phase_complete` bulked on the measurement axis
    /// (verbose names — which pass 2 must NOT truncate, consumers merge
    /// rows by name) was dropped whole, losing the phase outcome. Pass 3
    /// sheds the list instead: the consumer's reducer falls back to the
    /// rows it accumulated from live `measurement_update`s when the
    /// batched list is empty, and the outcome lands.
    #[test]
    fn phase_complete_bulked_by_measurement_names_keeps_its_outcome() {
        let measurement = |i: usize| RunMeasurement {
            name: format!("m{i}_{}", "n".repeat(4_000)),
            outcome: "PASS".to_string(),
            measured_value: Some(serde_json::json!(1.0)),
            units: None,
            validators: vec![],
            aggregations: vec![],
            low: None,
            high: None,
        };
        let event = StationEvent::PhaseComplete {
            phase_key: "3_sweep".to_string(),
            name: "sweep".to_string(),
            outcome: "FAIL".to_string(),
            measurements: (0..20).map(measurement).collect(),
            slot_id: None,
            attempt: 1,
            started_at: None,
            ended_at: None,
            duration_ms: Some(950),
            error: Some("threshold exceeded on m17".to_string()),
            logs: vec![],
            execution_id: None,
            run_metadata: Default::default(),
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::PhaseComplete { outcome, error, measurements, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(outcome, "FAIL");
        assert!(error.as_deref().unwrap().contains("threshold exceeded"));
        assert!(measurements.is_empty());
    }

    /// Verbose validator expressions are display-only, so pass 2 truncates
    /// them and the rows — names and verdicts — all survive without ever
    /// reaching pass 3.
    #[test]
    fn phase_complete_with_verbose_validators_keeps_every_row() {
        let measurement = |i: usize| RunMeasurement {
            name: format!("point_{i}"),
            outcome: "PASS".to_string(),
            measured_value: Some(serde_json::json!(1.0)),
            units: Some("V".to_string()),
            validators: vec![crate::ValidatorResult {
                expression: "x >= 3.0 and x <= 4.5 ".repeat(400),
                outcome: "PASS".to_string(),
                is_decisive: Some(true),
            }],
            aggregations: vec![],
            low: None,
            high: None,
        };
        let event = StationEvent::PhaseComplete {
            phase_key: "3_sweep".to_string(),
            name: "sweep".to_string(),
            outcome: "PASS".to_string(),
            measurements: (0..20).map(measurement).collect(),
            slot_id: None,
            attempt: 1,
            started_at: None,
            ended_at: None,
            duration_ms: Some(950),
            error: None,
            logs: vec![],
            execution_id: None,
            run_metadata: Default::default(),
        };
        assert!(event.wire_size() > MAX_EVENT_BYTES);

        let shrunk = event.shrink_to_fit(MAX_EVENT_BYTES).expect("degradable");
        assert!(shrunk.wire_size() <= MAX_EVENT_BYTES);
        let StationEvent::PhaseComplete { measurements, .. } = shrunk else {
            panic!("variant changed");
        };
        assert_eq!(measurements.len(), 20);
        assert!(measurements.iter().all(|m| m.outcome == "PASS"));
        assert!(measurements
            .iter()
            .all(|m| m.validators[0].expression.contains("truncated")));
    }
}
