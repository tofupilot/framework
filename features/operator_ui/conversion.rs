//! UI configuration conversion from procedure to execution types.

use super::types::{ComponentType, ComponentValue, UiComponent, UiConfig, UiOption};
use crate::procedure::schema::{
    ComponentValue as ProcedureComponentValue,
    UIComponent as ProcedureUIComponent,
    UIConfig as ProcedureUIConfig,
};
/// Convert procedure ComponentValue to execution ComponentValue
fn convert_component_value(proc_value: &ProcedureComponentValue) -> ComponentValue {
    match proc_value {
        ProcedureComponentValue::Boolean(b) => ComponentValue::Boolean(*b),
        ProcedureComponentValue::Number(n) => ComponentValue::Number(*n),
        ProcedureComponentValue::String(s) => ComponentValue::String(s.clone()),
        ProcedureComponentValue::Array(a) => ComponentValue::Array(a.clone()),
    }
}

/// Convert procedure UIConfig to execution UiConfig
pub fn convert_ui_config(proc_ui: &ProcedureUIConfig) -> UiConfig {
    let component_count = proc_ui.components.as_ref().map(|c| c.len()).unwrap_or(0);
    log::debug!(
        "Converting UI config with {} components",
        component_count
    );

    UiConfig {
        components: proc_ui
            .components
            .as_ref()
            .map(|comps| comps.iter().map(convert_ui_component).collect())
            .unwrap_or_default(),
        requires_input: proc_ui.requires_input,
    }
}

/// Convert procedure UIComponent to execution UiComponent
fn convert_ui_component(proc_comp: &ProcedureUIComponent) -> UiComponent {
    let component_type = match &proc_comp.component_type {
        crate::procedure::schema::UIComponentType::TextInput => ComponentType::TextInput,
        crate::procedure::schema::UIComponentType::NumberInput => ComponentType::NumberInput,
        crate::procedure::schema::UIComponentType::Switch => ComponentType::Switch,
        crate::procedure::schema::UIComponentType::Textarea => ComponentType::Textarea,
        crate::procedure::schema::UIComponentType::Radio => ComponentType::Radio,
        crate::procedure::schema::UIComponentType::Select => ComponentType::Select,
        crate::procedure::schema::UIComponentType::Multiselect => ComponentType::Multiselect,
        crate::procedure::schema::UIComponentType::Checklist => ComponentType::Checklist,
        crate::procedure::schema::UIComponentType::Slider => ComponentType::Slider,
        crate::procedure::schema::UIComponentType::Text => ComponentType::Text,
        crate::procedure::schema::UIComponentType::Image => ComponentType::Image,
        crate::procedure::schema::UIComponentType::Progress => ComponentType::Progress,
    };

    UiComponent {
        key: proc_comp.key.clone(),
        label: proc_comp.label.clone(),
        component_type: component_type.clone(),
        is_input: component_type.is_input(),
        description: proc_comp.description.clone(),
        required: proc_comp.required,
        placeholder: proc_comp.placeholder.clone(),
        options: proc_comp.options.as_ref().map(|opts| {
            opts.iter()
                .map(|opt| UiOption {
                    label: opt.label.clone(),
                    value: opt.value.clone(),
                    image: opt.image.clone(),
                })
                .collect()
        }),
        columns: proc_comp.columns,
        default_value: proc_comp
            .default_value
            .as_ref()
            .map(convert_component_value),
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
        rows: proc_comp.rows,
        size: proc_comp.size.clone(),
        color: proc_comp.color.clone(),
        font: proc_comp.font.clone(),
    }
}
