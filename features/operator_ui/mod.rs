//! Operator UI module - types, conversion, channels, and commands.
//!
//! This module consolidates all operator UI logic that was previously
//! scattered across execution/ui_types.rs, orchestrator/ui_conversion.rs,
//! and worker/support/ui.rs.

pub mod channels;
pub mod commands;
pub mod conversion;
pub mod types;

// Re-export all public types
pub use channels::UI_RESPONSE_CHANNELS;
pub use commands::{submit_native_ui_response, UiResponseValue};
pub use conversion::convert_ui_config;
pub use types::{
    ComponentType, ComponentValue, PythonPhaseResult, ResponseValue, UiComponent, UiConfig,
    UiOption, UiRequestData, UiRequestEvent, UiResponseData,
};
