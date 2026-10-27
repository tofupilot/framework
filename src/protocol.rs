//! NDJSON protocol types for Rust-Python IPC over TCP.
//!
//! These replace the protobuf-generated types. JSON fields that were
//! previously double-serialized (e.g. `value_json: String`) are now
//! `serde_json::Value` to avoid the extra encode/decode round-trip.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// -- Worker protocol (Rust -> Python command, Python -> Rust events) --

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobCommand {
    pub job_id: String,
    pub slot_id: String,
    pub phase_name: String,
    pub module: String,
    pub function: String,
    pub plugs: HashMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub retry_count: u32,
    pub retry_limit: u32,
    pub unit_info: Option<UnitInfo>,
    pub phase_results: HashMap<String, String>,
    /// The phase's operator-screen components, reduced to what `ui.set`
    /// needs to refuse a bad call where the author sees it (a Python
    /// exception) instead of in the engine log: each key, its type, and
    /// the properties a phase may set on it, computed from
    /// `station_protocol::properties`. Rendering still reads nothing
    /// from here; the screen itself travels as a `UiRequest` event.
    #[serde(default)]
    pub ui_components: Vec<UiComponentSpec>,
    /// Whether the phase shows a `ui.panel`: `ui.<key> = v` on a key no
    /// component declares is then a panel value, and `ui.wait()` has a
    /// screen even without components.
    #[serde(default)]
    pub ui_panel: bool,
    /// The panel's button names (`UiPanel::events`): what `ui.pressed()`
    /// accepts.
    #[serde(default)]
    pub ui_events: Vec<String>,
    /// The resolved `inputs:` of the sequence the phase belongs to, exposed
    /// to the phase function as the `inputs` injectable. Empty outside a
    /// sequence.
    #[serde(default)]
    pub inputs: HashMap<String, serde_json::Value>,
    /// Plug parameter name in the phase to the plug key it is bound to,
    /// for a sequence whose plug requirement was bound under another name
    /// (`plugs: {dmm: multimeter}` on the call). The worker injects the
    /// bound plug under the phase's own parameter name.
    #[serde(default)]
    pub plug_aliases: HashMap<String, String>,
    /// Plugs of the run that did not open, by Python identifier, with why.
    #[serde(default)]
    pub unavailable_plugs: HashMap<String, String>,
    /// The phase's `with:`, markers expanded: keyword arguments of the
    /// phase function, by parameter name.
    #[serde(default)]
    pub with: HashMap<String, serde_json::Value>,
    /// The phase's own `env:`, expanded: set in `os.environ` while the
    /// phase runs, restored after.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Report the frame's variables if the phase raises.
    #[serde(default)]
    pub capture_locals: bool,
    /// The `run` object's read-only context: `slot`, `station`,
    /// `procedure` and `deployment` (see `RunContext::to_json`).
    #[serde(default)]
    pub run_context: serde_json::Value,
    /// The config the phase reads (the `config` injectable).
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
    /// The secrets the phase reads (the `secrets` injectable).
    #[serde(default)]
    pub secrets: HashMap<String, serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UiComponentSpec {
    pub key: String,
    #[serde(rename = "type")]
    pub component_type: String,
    pub settable: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UnitInfo {
    pub serial_number: Option<String>,
    pub part_number: Option<String>,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    /// Operator (member email or free-text name) forwarded to
    /// `runs.create` as `operated_by`. A RUN property carried on the
    /// unit pipeline for transport only — Python exposes it as
    /// `run.operated_by` (see `unit.rs`), not as a unit field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<String>,
    #[serde(default)]
    pub sub_units: HashMap<String, String>,
    /// Operator-entered identify-form metadata, threaded to Python so
    /// phases can read `unit.metadata` (internal Rust↔Python IPC only —
    /// not the station-protocol wire).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum WorkerEvent {
    /// First message on every job connection, sent by tp_worker.py right
    /// after it parses the command — before any user code runs. Proves the
    /// interpreter is alive and reading; its absence within
    /// `JOB_ACK_TIMEOUT` means the process is wedged (typically an
    /// endpoint-protection agent holding it).
    JobAck(JobAckEvent),
    /// Sent after the phase's module finished importing, before the phase
    /// function is invoked. Bounds the module-import window: module-level
    /// code that blocks forever (device open at import, EDR-stalled
    /// import) is the one stage phase timeouts can't cover, because the
    /// Python-side timeout loop only guards the phase call itself.
    ModuleLoaded(ModuleLoadedEvent),
    JobComplete(JobResult),
    Error(ErrorEvent),
    AttachFile(AttachFileEvent),
    AttachData(AttachDataEvent),
    UiUpdate(UiUpdateEvent),
    /// Live log line from the Python phase. Emitted per-line as the phase
    /// runs, so a force-killed phase's logs are still visible to the
    /// orchestrator rather than being lost with the discarded `JobResult`.
    PhaseLogLine(PhaseLogLineEvent),
    /// Live measurement write. Lets observers see values land as Python
    /// sets them, not only bundled in the final JobResult.
    MeasurementRecorded(MeasurementRecordedEvent),
    /// `ui.wait()`: the phase is blocked until the operator answers its
    /// screen. The engine shows the screen (re-arming it when the
    /// previous answer was already consumed) and replies on the same
    /// connection with an [`EngineMessage`] carrying this `wait_id`.
    UiWait(UiWaitEvent),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UiWaitEvent {
    pub job_id: String,
    pub wait_id: String,
}

/// Engine -> Python lines written on a job connection after the
/// `JobCommand`. The worker reads them on a thread of its own while the
/// phase runs; read by `UIContext.wait()` and `UIContext.pressed()`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum EngineMessage {
    /// The operator's answer to a `UiWait`, typed per component (see
    /// `worker::ui_wait::typed_answer`).
    UiAnswer {
        wait_id: String,
        values: serde_json::Map<String, serde_json::Value>,
    },
    /// The wait can never be answered (run stopped, slot stopped, prompt
    /// closed): `ui.wait()` raises `ui.WaitCancelled` with `reason`.
    UiCancel { wait_id: String, reason: String },
    /// The wait should never have been made (no screen to wait on, another
    /// thread already waiting): an authoring error, so `ui.wait()` raises
    /// `RuntimeError`, not `ui.WaitCancelled`.
    UiRefused { wait_id: String, reason: String },
    /// The operator pressed the panel button `name` (`ui.pressed(name)`).
    UiEvent { name: String },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobResult {
    pub success: bool,
    pub phase_result_json: Option<String>,
    #[serde(default)]
    pub measurements: Vec<Measurement>,
    #[serde(default)]
    pub logs: Vec<LogEntry>,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub unit_json: Option<String>,
    /// Run-level metadata set via `run.metadata[...]`, double-encoded JSON
    /// object (same convention as `unit_json`).
    pub run_metadata_json: Option<String>,
    /// Unit-level metadata set via `unit.metadata[...]`, double-encoded JSON.
    pub unit_metadata_json: Option<String>,
    /// The last `ui.wait()` whose answer the phase received. An answer
    /// the engine wrote after the phase had ended (a timeout racing the
    /// operator's submit) is not it, and `bind:` does not record it.
    #[serde(default)]
    pub ui_answered_wait_id: Option<String>,
    /// The failing frame's variables (`[{name, value}]`), double-encoded
    /// JSON like `unit_json`. Set only when the phase raised.
    #[serde(default)]
    pub locals_json: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Measurement {
    pub name: String,
    pub value: serde_json::Value,
    pub unit: Option<String>,
    pub timestamp: String,
    pub result: Option<String>,
    pub aggregations: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorEvent {
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobAckEvent {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModuleLoadedEvent {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MeasurementRecordedEvent {
    pub job_id: String,
    pub name: String,
    /// Value is sent as a JSON string by the Python side to avoid reshaping
    /// the tagged union (`{"Numeric": 5.0}` etc.); the consumer parses it.
    pub value_json: String,
    pub timestamp: String,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PhaseLogLineEvent {
    pub job_id: String,
    pub level: String,
    pub message: String,
    pub timestamp: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttachFileEvent {
    pub job_id: String,
    pub source_path: String,
    pub attachment_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AttachDataEvent {
    pub job_id: String,
    pub data: String,
    pub attachment_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UiUpdateEvent {
    pub job_id: String,
    pub action: String,
    pub data_json: String,
}

// -- Plug protocol (Rust -> Python request, Python -> Rust response) --

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum PlugRequest {
    CallMethod(MethodRequest),
    GetStatus,
    Cleanup,
    Shutdown,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MethodRequest {
    pub method: String,
    pub args_json: String,
    /// JSON object of keyword arguments. Optional and skipped when
    /// absent so pre-kwargs plug services keep parsing requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kwargs_json: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlugResponse {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub result_json: Option<String>,
    #[serde(default)]
    pub state: Option<HashMap<String, String>>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub cleanup_duration_seconds: Option<f64>,
    /// `Cleanup` only: `false` when a method still running kept the
    /// instance alive, so its `__del__` did not run. Absent from an older
    /// plug service: read as finalized.
    #[serde(default)]
    pub finalized: Option<bool>,
    /// `GetStatus` only: the plug's `__init__` is still running. Ready
    /// is `success`; anything else that is not initializing is a
    /// failure, whatever its text says.
    #[serde(default)]
    pub initializing: bool,
    /// A failed `CallMethod`: the exception's class and message, as the
    /// plug raised them (`error` is its traceback).
    #[serde(default)]
    pub error_module: Option<String>,
    #[serde(default)]
    pub error_qualname: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

impl PlugResponse {
    /// The exception's class as Python prints it: `ValueError` for a
    /// builtin, `module.Class` otherwise.
    pub fn error_type(&self) -> Option<String> {
        let name = self.error_qualname.as_deref()?;
        Some(match self.error_module.as_deref() {
            None | Some("builtins") | Some("__main__") => name.to_string(),
            Some(module) => format!("{module}.{name}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(module: &str, qualname: &str) -> PlugResponse {
        serde_json::from_value(serde_json::json!({
            "success": false,
            "error": "Traceback ...",
            "error_module": module,
            "error_qualname": qualname,
            "error_message": "bad\nvalue",
        }))
        .unwrap()
    }

    #[test]
    fn a_builtin_exception_is_named_alone() {
        let r = failed("builtins", "ValueError");
        assert_eq!(r.error_type().as_deref(), Some("ValueError"));
        assert_eq!(r.error_message.as_deref(), Some("bad\nvalue"));
    }

    #[test]
    fn a_driver_exception_carries_its_module() {
        let r = failed("drivers.psu", "Psu.Overload");
        assert_eq!(r.error_type().as_deref(), Some("drivers.psu.Psu.Overload"));
    }
}
