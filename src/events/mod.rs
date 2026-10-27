//! Event types for execution and plug status updates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum PlugStatusValue {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "initializing")]
    Initializing,
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "destructing")]
    Destructing,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "skipped")]
    Skipped,
}

/// Wire mirror of `procedure::schema::Scope` for plug status events.
/// Same serde as the schema enum: `slot`/`execution`/`station`.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlugScope {
    Slot,
    Execution,
    Station,
}

impl PlugScope {
    pub fn as_str(self) -> &'static str {
        match self {
            PlugScope::Slot => "slot",
            PlugScope::Execution => "execution",
            PlugScope::Station => "station",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum PlugStage {
    #[serde(rename = "setup")]
    Setup,
    #[serde(rename = "teardown")]
    Teardown,
    #[serde(rename = "manual")]
    Manual,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlugStatusUpdateEvent {
    pub plug_key: String,
    pub plug_name: String,
    pub scope: PlugScope,
    pub slot_id: Option<String>,
    pub stage: PlugStage,
    pub status: PlugStatusValue,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlugLogEvent {
    pub plug_key: String,
    pub plug_name: String,
    pub slot_id: Option<String>,
    /// Lifecycle stage the plug was in when this line was emitted.
    /// `setup` / `teardown` / `manual`. None for legacy emitters that
    /// don't track the active stage at log-line time.
    pub stage: Option<PlugStage>,
    pub level: String,
    pub message: String,
    pub timestamp: Option<String>,
    /// Source file of the line, relative to the procedure dir, as on a
    /// phase log line.
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UiUpdateEvent {
    pub job_id: String,
    pub slot_id: String,
    pub phase_key: String,
    pub worker_id: usize,
    pub action: String,
    #[serde(serialize_with = "serialize_json_value")]
    pub data: serde_json::Value,
}

fn serialize_json_value<S>(value: &serde_json::Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}
