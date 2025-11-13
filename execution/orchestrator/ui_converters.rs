//! UI configuration conversion utilities
//!
//! Converts procedure UI definitions to execution UI types

use crate::schema::procedure::{
    ComponentValue as ProcedureComponentValue,
    UIComponent as ProcedureUIComponent, UIConfig as ProcedureUIConfig,
};
use crate::execution::ui_types::{ComponentType, ComponentValue, UiComponent, UiConfig, UiOption};

/// Convert procedure ComponentValue to execution ComponentValue
fn convert_component_value(proc_value: &ProcedureComponentValue) -> ComponentValue {
    match proc_value {
        ProcedureComponentValue::Boolean(b) => ComponentValue::Boolean(*b),
        ProcedureComponentValue::Number(n) => ComponentValue::Number(*n),
        ProcedureComponentValue::String(s) => ComponentValue::String(s.clone()),
    }
}

/// Convert procedure UIConfig to execution UiConfig
pub(super) fn convert_ui_config(proc_ui: &ProcedureUIConfig) -> UiConfig {
    crate::execution::cli_output::debug(format!(
        "Converting UI config with {} components",
        proc_ui.components.len()
    ));

    UiConfig {
        instructions: if proc_ui.instructions.is_empty() {
            None
        } else {
            Some(proc_ui.instructions.clone())
        },
        components: proc_ui
            .components
            .iter()
            .map(convert_ui_component)
            .collect(),
    }
}

/// Convert procedure UIComponent to execution UiComponent
fn convert_ui_component(proc_comp: &ProcedureUIComponent) -> UiComponent {
    UiComponent {
        key: proc_comp.key.clone(),
        label: proc_comp.label.clone(),
        component_type: ComponentType::from_str(&proc_comp.component_type)
            .unwrap_or(ComponentType::TextInput), // Use from_str with default fallback
        description: proc_comp.description.clone(),
        required: proc_comp.required,
        placeholder: proc_comp.placeholder.clone(),
        options: proc_comp.options.as_ref().map(|opts| {
            opts.iter()
                .map(|opt| UiOption {
                    label: opt.label.clone(),
                    value: opt.value.clone(),
                })
                .collect()
        }),
        default_value: proc_comp.default_value.as_ref().map(convert_component_value),
        value: None, // Runtime only
        width: proc_comp.width.clone(),
        height: proc_comp.height.clone(),
        aspect: proc_comp.aspect.clone(),
        fit: proc_comp.fit.clone(),
        min: proc_comp.min,
        max: proc_comp.max,
        step: proc_comp.step,
        bind: proc_comp.bind.clone(),
        min_length: proc_comp.min_length,
        max_length: proc_comp.max_length,
        pattern: proc_comp.pattern.clone(),
        prefix: proc_comp.prefix.clone(),
        suffix: proc_comp.suffix.clone(),
        trim: proc_comp.trim,
    }
}
