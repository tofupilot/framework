//! YAML-side `procedure::schema::UIConfig` → runtime `ui::UiConfig`.
//! `From` impls keep the conversion at the type boundary so call sites
//! read `cfg.into()` instead of importing a free function.
//!
//! `ComponentType` and `ComponentValue` are re-exported by
//! `procedure::schema` directly from `station-protocol`, so YAML
//! deserialization lands on the same enum the runtime + wire use —
//! no enum-to-enum conversion needed.

use crate::procedure::schema::{
    SelectOption, UIComponent as SchemaUIComponent, UIConfig as SchemaUIConfig,
};
use crate::ui::{UiComponent, UiConfig, UiOption};

impl From<&SelectOption> for UiOption {
    fn from(o: &SelectOption) -> Self {
        UiOption {
            label: o.label.clone(),
            value: o.value.clone(),
            image: o.image.clone(),
        }
    }
}

impl From<&SchemaUIComponent> for UiComponent {
    fn from(c: &SchemaUIComponent) -> Self {
        UiComponent {
            key: c.key.clone(),
            label: c.label.clone(),
            description: c.description.clone(),
            placeholder: c.placeholder.clone(),
            required: c.required,
            bind: c.bind.clone(),
            default_value: c.default_value.clone(),
            min_length: c.min_length,
            max_length: c.max_length,
            pattern: c.pattern.clone(),
            pattern_message: c.pattern_message.clone(),
            prefix: c.prefix.clone(),
            suffix: c.suffix.clone(),
            trim: c.trim,
            rows: c.rows,
            min: c.min,
            max: c.max,
            step: c.step,
            options: c
                .options
                .as_ref()
                .map(|opts| opts.iter().map(UiOption::from).collect()),
            columns: c.columns,
            width: c.width.clone(),
            height: c.height.clone(),
            aspect: c.aspect.clone(),
            fit: c.fit.clone(),
            size: c.size,
            color: c.color,
            font: c.font,
            ..UiComponent::new(c.component_type)
        }
    }
}

impl From<&SchemaUIConfig> for UiConfig {
    fn from(cfg: &SchemaUIConfig) -> Self {
        let components: Vec<UiComponent> = cfg
            .components
            .as_ref()
            .map(|comps| comps.iter().map(UiComponent::from).collect())
            .unwrap_or_default();
        log::debug!("Converted UI config with {} components", components.len());
        UiConfig {
            components,
            requires_input: cfg.requires_input,
            // Needs the procedure directory: loaded by the job builder.
            panel: None,
        }
    }
}
