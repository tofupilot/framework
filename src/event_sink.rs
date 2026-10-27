use crate::events::{PlugLogEvent, PlugStatusUpdateEvent, UiUpdateEvent};
use crate::job::{JobStatus, Outcome};
use crate::log::LogEntry;
use crate::measurements::Measurement;
use crate::procedure::schema::StageScope;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// Unified execution event emitted by the engine.
/// Every consumer (Studio, CLI TUI, WebSocket, web UI) must handle all variants.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// `Plan` and `JobComplete` are large and every consumer destructures
// them; an event is built once and moved, so boxing buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum ExecutionEvent {
    /// Execution plan computed, about to start
    Plan {
        phases: Vec<PlannedPhase>,
        plugs_all: Vec<PlannedPlug>,
        plugs_each: Vec<PlannedPlug>,
        slots: Vec<String>,
        total_expected_jobs: u32,
        /// The sequence calls of the procedure, parents before children:
        /// the tree consumers group the planned phases by (a phase's
        /// `sequence_path` is one of these paths).
        #[serde(default)]
        sequences: Vec<PlannedSequence>,
    },

    /// A sequence call finished in one scope: every job under it is done
    /// (ran, skipped or was cancelled). One per call and per slot, or one
    /// with no slot for a call made at execution scope. `outcome` follows
    /// the run's own aggregation applied to the call's phases, plus Skip
    /// when none of them ran.
    SequenceComplete {
        slot_id: Option<String>,
        /// Instance keys from the root to this call; `sequence_path` of
        /// the phases it holds starts with it.
        sequence_path: Vec<String>,
        key: String,
        name: String,
        /// The `uses:` reference as written.
        source: String,
        outcome: Outcome,
        /// First start and last end among the call's phases that ran;
        /// `None` when none did.
        started_at: Option<String>,
        ended_at: Option<String>,
    },

    /// Job lifecycle update (queued, running, stopping)
    JobProgress {
        job_id: String,
        slot_id: Option<String>,
        phase_key: String,
        phase_name: String,
        stage_scope: StageScope,
        status: JobStatus,
        worker_id: Option<usize>,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        timeout_ms: Option<u64>,
        outcome: Option<Outcome>,
        retry_count: usize,
        error: Option<String>,
    },

    /// Job completed with full results
    JobComplete {
        job_id: String,
        slot_id: Option<String>,
        phase_key: String,
        phase_name: String,
        stage_scope: StageScope,
        outcome: Outcome,
        action: String,
        next_action: Option<String>,
        /// The phase asked for the run to stop (`JobResult::requested_stop`).
        #[serde(default)]
        requested_stop: bool,
        measurements: Vec<Measurement>,
        logs: Vec<LogEntry>,
        resource_metrics: Option<crate::job::ResourceMetrics>,
        retry_count: usize,
        retry_limit: usize,
        started_at: String,
        completed_at: String,
        duration_ms: u64,
        worker_id: usize,
        error: Option<String>,
        /// Run-level metadata set by the phase via `run.metadata[...]`.
        run_metadata: std::collections::HashMap<String, serde_json::Value>,
        /// Unit-level metadata set by the phase via `unit.metadata[...]`.
        unit_metadata: std::collections::HashMap<String, serde_json::Value>,
        /// The sequence calls the phase came through; empty for a phase of
        /// the procedure. What the upload stores as the phase's sequence.
        #[serde(default)]
        sequence_path: Vec<String>,
        /// The failing frame's variables (`JobResult::failure_locals`).
        #[serde(default)]
        failure_locals: Vec<crate::job::FrameLocal>,
    },

    /// Execution statistics snapshot
    Stats {
        total_jobs: usize,
        completed_jobs: usize,
        failed_jobs: usize,
        running_jobs: usize,
        queued_jobs: usize,
        workers_busy: usize,
        workers_total: usize,
        run_outcome: Option<Outcome>,
        run_dir: Option<String>,
        run_id: Option<String>,
        slot_outcomes: HashMap<String, Outcome>,
        slot_run_ids: HashMap<String, String>,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
    },

    /// Execution complete (final stats)
    Complete {
        total_jobs: usize,
        completed_jobs: usize,
        failed_jobs: usize,
        running_jobs: usize,
        queued_jobs: usize,
        workers_busy: usize,
        workers_total: usize,
        run_outcome: Option<Outcome>,
        run_dir: Option<String>,
        run_id: Option<String>,
        slot_outcomes: HashMap<String, Outcome>,
        slot_run_ids: HashMap<String, String>,
        start_time: Option<chrono::DateTime<chrono::Utc>>,
        end_time: Option<chrono::DateTime<chrono::Utc>>,
    },

    /// Plug lifecycle update
    PlugStatus(PlugStatusUpdateEvent),

    /// Plug log output
    PlugLog(PlugLogEvent),

    /// UI request for operator input
    UiRequest(crate::ui::UiRequestData),

    /// UI update (progress bar, display refresh)
    UiUpdate(UiUpdateEvent),

    /// Live log line from a running phase. Emitted as each line is
    /// produced so observers (TUI, agent protocol, dashboard) see logs
    /// in real time — and surviving a force-kill.
    PhaseLogLine {
        job_id: String,
        phase_key: String,
        slot_id: Option<String>,
        level: String,
        message: String,
        timestamp: String,
        file: Option<String>,
        line: Option<u32>,
    },

    /// Live measurement write. Emitted when Python does
    /// `measurements.voltage = 5.0`, not only at phase end.
    MeasurementRecorded {
        job_id: String,
        phase_key: String,
        slot_id: Option<String>,
        name: String,
        /// Raw JSON value shape produced by the Python side
        /// (e.g. `{"Numeric": 5.0}`, `{"String": "ok"}`).
        value: serde_json::Value,
        unit: Option<String>,
        timestamp: String,
        /// The value judged against its declared limits as it is recorded
        /// (`judge_recorded`), for screens drawn while the phase runs.
        /// Phase end judges again with everything; that verdict is final.
        outcome: crate::procedure::schema::ValidatorOutcome,
        /// The declared limits, judged; empty when none are declared.
        validators: Vec<crate::procedure::schema::ValidatorSpec>,
    },

    /// Live attachment added to the current phase. Agents can see which
    /// files / data blobs the phase produced before the run ends.
    AttachmentAdded {
        phase_key: String,
        slot_id: Option<String>,
        name: String,
        path: Option<String>,
        mimetype: Option<String>,
    },

    /// Unit identity resolved (operator prompt, `auto_identify` defaults,
    /// or an external connector). Emitted by the runner before the
    /// orchestrator starts processing phases so consumers (TUI, agent,
    /// upload layer) have the resolved serial / part / revision /
    /// batch / sub-units in a structured event rather than having to
    /// reconstruct it from a `UiResponse`.
    UnitIdentified {
        slot_id: Option<String>,
        unit_info: crate::unit::UnitInfo,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedPhase {
    pub phase_key: String,
    pub phase_name: String,
    pub stage_scope: StageScope,
    /// The sequence calls the phase came through (see `Job`); empty for a
    /// phase of the procedure. Consumers group the plan by it.
    #[serde(default)]
    pub sequence_path: Vec<String>,
}

/// A sequence call in the plan. `path.join("/")` is the dashboard's
/// `sequence_path`; `parent` is `path` minus its last key.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedSequence {
    pub path: Vec<String>,
    pub key: String,
    pub name: String,
    pub source: String,
    /// The list of its parent the call sits in.
    pub stage: crate::procedure::schema::PhaseStage,
    /// Set on a call made from the procedure's setup or teardown.
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedPlug {
    pub plug_key: String,
    pub plug_name: String,
    pub scope: String,
}

/// Trait for receiving execution events.
/// Implemented by each consumer: Studio (Tauri), CLI (TUI + WebSocket), etc.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &ExecutionEvent);
}

/// No-op sink for tests and headless execution.
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &ExecutionEvent) {}
}

/// Sink that fans out to multiple sinks.
pub struct MultiSink(pub Vec<Arc<dyn EventSink>>);

impl EventSink for MultiSink {
    fn emit(&self, event: &ExecutionEvent) {
        for sink in &self.0 {
            sink.emit(event);
        }
    }
}

/// Masks the run's secret values in what the engine forwards from user
/// code: log lines (phase and plug), errors, live measurement and UI
/// values. Exact matches of values of `MASK_MIN_LEN` characters or more
/// (`RunValues::masked_values`); accident protection, not a boundary: a
/// phase can always transform a secret before printing it.
pub struct MaskingSink {
    inner: Arc<dyn EventSink>,
    masks: Arc<std::sync::RwLock<Vec<String>>>,
}

impl MaskingSink {
    pub fn new(inner: Arc<dyn EventSink>, masks: Arc<std::sync::RwLock<Vec<String>>>) -> Self {
        Self { inner, masks }
    }

    /// `event` with every secret masked; `None` when nothing changed.
    fn masked(event: &ExecutionEvent, masks: &[String]) -> Option<ExecutionEvent> {
        use crate::procedure::values::{mask_json, mask_python_repr, mask_secrets};
        use std::borrow::Cow;
        let text = |s: &str| match mask_secrets(s, masks) {
            Cow::Owned(m) => Some(m),
            Cow::Borrowed(_) => None,
        };
        let mut out = event.clone();
        let mut changed = false;
        let mut fix = |s: &mut String| {
            if let Some(m) = text(s) {
                *s = m;
                changed = true;
            }
        };
        match &mut out {
            ExecutionEvent::PhaseLogLine { message, .. } => fix(message),
            ExecutionEvent::PlugLog(log) => fix(&mut log.message),
            ExecutionEvent::JobProgress { error, .. } => {
                if let Some(e) = error.as_mut() {
                    fix(e);
                }
            }
            ExecutionEvent::JobComplete {
                logs,
                error,
                failure_locals,
                ..
            } => {
                for entry in logs.iter_mut() {
                    fix(&mut entry.message);
                }
                if let Some(e) = error.as_mut() {
                    fix(e);
                }
                // A local's value is a `repr`: a secret in it is escaped.
                for local in failure_locals.iter_mut() {
                    if let Cow::Owned(m) = mask_python_repr(&local.value, masks) {
                        local.value = m;
                        changed = true;
                    }
                }
            }
            ExecutionEvent::MeasurementRecorded { value, .. } => {
                let before = value.clone();
                mask_json(value, masks);
                changed |= *value != before;
            }
            ExecutionEvent::UiUpdate(update) => {
                let before = update.data.clone();
                mask_json(&mut update.data, masks);
                changed |= update.data != before;
            }
            _ => {}
        }
        changed.then_some(out)
    }
}

impl EventSink for MaskingSink {
    fn emit(&self, event: &ExecutionEvent) {
        let masks = self.masks.read().unwrap_or_else(|e| e.into_inner());
        if masks.is_empty() {
            drop(masks);
            return self.inner.emit(event);
        }
        let masked = Self::masked(event, &masks);
        drop(masks);
        match masked {
            Some(event) => self.inner.emit(&event),
            None => self.inner.emit(event),
        }
    }
}

#[cfg(test)]
mod masking_tests {
    use super::*;
    use std::sync::Mutex;

    struct Capture(Mutex<Vec<ExecutionEvent>>);

    impl EventSink for Capture {
        fn emit(&self, event: &ExecutionEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn log_lines_and_errors_are_masked() {
        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        let masks = Arc::new(std::sync::RwLock::new(vec!["hunter2hunter2".to_string()]));
        let sink = MaskingSink::new(capture.clone(), masks);
        sink.emit(&ExecutionEvent::PhaseLogLine {
            job_id: "j".into(),
            phase_key: "p".into(),
            slot_id: None,
            level: "info".into(),
            message: "token=hunter2hunter2".into(),
            timestamp: String::new(),
            file: None,
            line: None,
        });
        sink.emit(&ExecutionEvent::MeasurementRecorded {
            job_id: "j".into(),
            phase_key: "p".into(),
            slot_id: None,
            name: "m".into(),
            value: serde_json::json!({"String": "hunter2hunter2"}),
            unit: None,
            timestamp: String::new(),
            outcome: crate::procedure::schema::ValidatorOutcome::Unset,
            validators: Vec::new(),
        });
        let events = capture.0.lock().unwrap();
        match &events[0] {
            ExecutionEvent::PhaseLogLine { message, .. } => assert_eq!(message, "token=••••"),
            other => panic!("{other:?}"),
        }
        match &events[1] {
            ExecutionEvent::MeasurementRecorded { value, .. } => {
                assert_eq!(value, &serde_json::json!({"String": "••••"}))
            }
            other => panic!("{other:?}"),
        }
    }
}
