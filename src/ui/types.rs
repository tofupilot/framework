//! UI runtime types. The component-level types (`UiComponent`,
//! `ComponentType`, `ComponentValue`, `UiOption`, `TextSize`,
//! `TextColor`, `FontFamily`) live in `station-protocol` so the engine,
//! the Centrifugo wire, the local-websocket wire, and the operator-UI
//! React renderer share a single definition. The engine only owns
//! `UiConfig` (a per-phase wrapper with input-detection helpers) and
//! the request/response/Python-result envelopes.

use serde::{Deserialize, Serialize};

pub use station_protocol::{
    ComponentType, ComponentValue, FontFamily, TextColor, TextSize, UiComponent, UiOption, UiPanel,
};

/// UI configuration for a phase.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiConfig {
    #[serde(default)]
    pub components: Vec<UiComponent>,

    /// Override whether this UI requires user input (shows Continue button).
    /// If not set, auto-detected from component types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_input: Option<bool>,

    /// The phase's `ui.panel`, read from the procedure directory when the
    /// job is built (`orchestrator::jobs`). `None` when the phase has none
    /// or the file could not be read: the components render as usual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<UiPanel>,
}

impl UiConfig {
    /// Check if this UI configuration has input components that require user interaction
    pub fn has_input_components(&self) -> bool {
        self.components.iter().any(|c| c.is_input)
    }

    /// Check if this UI requires user input.
    /// If any input component exists, always requires input (cannot be overridden).
    /// Otherwise, use `requires_input` from the procedure YAML (defaults to false).
    pub fn requires_user_input(&self) -> bool {
        if self.has_input_components() {
            return true;
        }
        self.requires_input.unwrap_or(false)
    }
}

/// UI request data sent to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiRequestData {
    pub request_id: String,
    pub job_id: String,
    pub pipe_path: String,
    pub config: UiConfig,
    pub phase_key: String,
    pub slot_id: Option<String>,
}

// Event wrapper for UI request
#[derive(Debug, Clone, Serialize)]
pub struct UiRequestEvent(pub UiRequestData);

/// Phase result value from Python
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PythonPhaseResult {
    Bool(bool),
    String(String),
    Null,
}
