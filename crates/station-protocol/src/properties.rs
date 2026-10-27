//! Run-time property updates: a phase changing what an operator-screen
//! component *is* (its options, label, bounds), not only what it holds.
//!
//! Python calls `ui.set("mo", options=[...])`, the worker ships a
//! `UiUpdate{set_properties}` carrying a [`UiPropertyPatch`], and three
//! consumers apply the same patch with the same rules: the engine (on the
//! components it validates the submit against, so the screen and the
//! check never disagree), the kiosk TUI, and the web operator UI (via the
//! exported constants). One allow-list, one validation, written here.
//!
//! What a phase may change is decided per field, not per component:
//!
//! * **Universal** (`label`, `description`): every component reads them.
//! * **Type-specific content and rules**: the fields of
//!   [`COMPONENT_FIELD_READERS`] that describe *what the operator chooses
//!   from or is held to* — options, placeholder, affixes, numeric and
//!   text bounds, pattern. Settable on the types that read them.
//! * **Layout and styling** (`rows`, `columns`, image sizing, text
//!   styling): closed. They are the screen's shape, and the shape stays
//!   in `procedure.yaml` so the Studio preview, the Inspector and the
//!   load-time lint keep describing what the operator will see.
//! * **Structure** (`key`, type, `bind`, `required`, `default_value`,
//!   `trim`, adding or removing a component): closed. Not in the table
//!   and not settable; the YAML is the source of what the screen
//!   contains and of the submit contract.
//!
//! Every field of the reader table must appear in exactly one of the two
//! lists below. `every_reader_field_has_a_decision` fails otherwise, so a
//! new field cannot become settable, or silently closed, by accident.

use serde::{Deserialize, Deserializer, Serialize};

use crate::pattern_messages::compile_field_pattern;
use crate::{ComponentType, UiComponent, UiOption, COMPONENT_FIELD_READERS};

/// Wire name of `UiUpdateAction::SetProperties`, for the producers and
/// consumers that match on the action string.
pub const SET_PROPERTIES_ACTION: &str = "set_properties";

/// Caps the YAML loader puts on an option list, enforced on run-time lists
/// too so `ui.set` cannot build a screen the loader would have refused.
/// Options with images render as a card grid; past this many the grid is
/// unusable on an operator screen.
pub const MAX_OPTIONS_WITH_IMAGES: usize = 12;
/// Longest option `value` or `label`, mirroring `SelectOption`'s
/// `#[validate(length(max = 200))]` in the engine's YAML schema.
pub const MAX_OPTION_TEXT_CHARS: usize = 200;

/// Properties every component type reads, hence settable on every type.
pub const UNIVERSAL_SETTABLE_PROPERTIES: &[&str] = &["label", "description"];

/// Type-specific fields a phase may set at run time, on the types that
/// read them (per `COMPONENT_FIELD_READERS`).
pub const SETTABLE_TYPE_SPECIFIC_FIELDS: &[&str] = &[
    "options",
    "placeholder",
    "prefix",
    "suffix",
    "min",
    "max",
    "step",
    "min_length",
    "max_length",
    "pattern",
    "pattern_message",
];

/// Type-specific fields that stay closed to run-time updates: the
/// screen's layout and styling, decided in the YAML.
pub const CLOSED_TYPE_SPECIFIC_FIELDS: &[&str] = &[
    "rows", "columns", "width", "height", "aspect", "fit", "size", "color", "font",
];

/// The properties a phase may set on a component of this type, in a
/// stable order (universal first, then the table's order).
pub fn settable_properties(component_type: ComponentType) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = UNIVERSAL_SETTABLE_PROPERTIES.to_vec();
    for (field, readers) in COMPONENT_FIELD_READERS {
        if SETTABLE_TYPE_SPECIFIC_FIELDS.contains(field) && readers.contains(&component_type) {
            out.push(field);
        }
    }
    out
}

/// `null` on the wire clears the property; an absent key leaves it
/// alone. Serde's `Option` cannot tell the two apart, so each field is
/// an `Option<Option<T>>` read through this helper.
fn present<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The properties one `ui.set(...)` call changes on one component. Outer
/// `None` = not mentioned, `Some(None)` = cleared, `Some(Some(v))` = set.
/// Unknown keys fail to parse: a typo must not become a silent no-op.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UiPropertyPatch {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub placeholder: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub prefix: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub suffix: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<Option<Vec<UiOption>>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub min: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub max: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub step: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_length: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_length: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub pattern: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub pattern_message: Option<Option<String>>,
}

/// Payload of a `UiUpdate{set_properties}`: the component's `key` (named
/// `id` like `set_value`'s payload) and what changes on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiSetPropertiesPayload {
    pub id: String,
    pub properties: UiPropertyPatch,
}

impl UiPropertyPatch {
    /// Names of the properties this patch mentions (set or cleared).
    pub fn mentioned(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.label.is_some() {
            out.push("label");
        }
        if self.description.is_some() {
            out.push("description");
        }
        if self.placeholder.is_some() {
            out.push("placeholder");
        }
        if self.prefix.is_some() {
            out.push("prefix");
        }
        if self.suffix.is_some() {
            out.push("suffix");
        }
        if self.options.is_some() {
            out.push("options");
        }
        if self.min.is_some() {
            out.push("min");
        }
        if self.max.is_some() {
            out.push("max");
        }
        if self.step.is_some() {
            out.push("step");
        }
        if self.min_length.is_some() {
            out.push("min_length");
        }
        if self.max_length.is_some() {
            out.push("max_length");
        }
        if self.pattern.is_some() {
            out.push("pattern");
        }
        if self.pattern_message.is_some() {
            out.push("pattern_message");
        }
        out
    }

    /// Apply the patch to `component`, or refuse it whole. A refusal
    /// leaves the component untouched and names the problem for the
    /// phase author: a property the type does not read, a bound pair
    /// that crosses, a pattern the station cannot compile, an option
    /// without a value. The merged state is what gets checked, so
    /// `ui.set("v", min=10)` on a `max: 5` component is refused even
    /// though the call alone looks fine.
    pub fn apply(&self, component: &mut UiComponent) -> Result<(), String> {
        let mentioned = self.mentioned();
        if mentioned.is_empty() {
            return Err(format!(
                "ui.set('{}') changes nothing: name at least one property",
                component.key
            ));
        }
        let allowed = settable_properties(component.component_type);
        if let Some(name) = mentioned.iter().find(|n| !allowed.contains(n)) {
            return Err(format!(
                "'{}' is a {} and has no '{}' property that a phase can set (settable: {})",
                component.key,
                component.component_type.as_str(),
                name,
                allowed.join(", ")
            ));
        }

        let mut next = component.clone();
        if let Some(v) = &self.label {
            next.label = v.clone();
        }
        if let Some(v) = &self.description {
            next.description = v.clone();
        }
        if let Some(v) = &self.placeholder {
            next.placeholder = v.clone();
        }
        if let Some(v) = &self.prefix {
            next.prefix = v.clone();
        }
        if let Some(v) = &self.suffix {
            next.suffix = v.clone();
        }
        if let Some(v) = &self.options {
            next.options = v.clone();
        }
        if let Some(v) = &self.min {
            next.min = *v;
        }
        if let Some(v) = &self.max {
            next.max = *v;
        }
        if let Some(v) = &self.step {
            next.step = *v;
        }
        if let Some(v) = &self.min_length {
            next.min_length = *v;
        }
        if let Some(v) = &self.max_length {
            next.max_length = *v;
        }
        if let Some(v) = &self.pattern {
            next.pattern = v.clone();
        }
        if let Some(v) = &self.pattern_message {
            next.pattern_message = v.clone();
        }

        check_merged(&next, &mentioned)?;
        *component = next;
        Ok(())
    }
}

/// Cross-field rules on the component as it would be after the patch,
/// for the fields the patch touches. A YAML component the loader
/// accepted with a pattern that does not compile or a duplicate option
/// value is judged at submit, not here: `ui.set("x", label=...)` on it
/// must not be refused for a rule the call did not touch.
fn check_merged(c: &UiComponent, mentioned: &[&str]) -> Result<(), String> {
    let touches = |names: &[&str]| names.iter().any(|n| mentioned.contains(n));
    if let (true, Some(options)) = (touches(&["options"]), &c.options) {
        let mut seen: Vec<&str> = Vec::with_capacity(options.len());
        for (i, o) in options.iter().enumerate() {
            if o.value.trim().is_empty() {
                return Err(format!(
                    "'{}': option {} has an empty value; every option needs a non-empty 'value'",
                    c.key,
                    i + 1
                ));
            }
            // The validator trims the operator's answer (`trim` defaults
            // to true) before comparing it with the option values, and
            // the YAML loader trims those. A padded run-time value could
            // never be chosen; the worker trims, this guards the wire.
            if o.value.trim() != o.value {
                return Err(format!(
                    "'{}': option {} value has surrounding whitespace",
                    c.key,
                    i + 1
                ));
            }
            if seen.contains(&o.value.as_str()) {
                return Err(format!(
                    "'{}': option value '{}' appears twice; values must be unique",
                    c.key, o.value
                ));
            }
            seen.push(&o.value);
            for (what, text) in [("value", &o.value), ("label", &o.label)] {
                if text.chars().count() > MAX_OPTION_TEXT_CHARS {
                    return Err(format!(
                        "'{}': option {} {what} is longer than {MAX_OPTION_TEXT_CHARS} characters",
                        c.key,
                        i + 1
                    ));
                }
            }
        }
        if options.iter().any(|o| o.image.is_some()) && options.len() > MAX_OPTIONS_WITH_IMAGES {
            return Err(format!(
                "'{}': {} options with images (max {MAX_OPTIONS_WITH_IMAGES} when options have images)",
                c.key,
                options.len()
            ));
        }
    }
    if !touches(&["min", "max", "step"]) {
        return check_merged_text(c, mentioned);
    }
    for (name, v) in [("min", c.min), ("max", c.max), ("step", c.step)] {
        if let Some(v) = v {
            if !v.is_finite() {
                return Err(format!("'{}': {name} must be a finite number", c.key));
            }
        }
    }
    if let (Some(min), Some(max)) = (c.min, c.max) {
        if min > max {
            return Err(format!(
                "'{}': min ({min}) is greater than max ({max})",
                c.key
            ));
        }
    }
    if let Some(step) = c.step {
        if step <= 0.0 {
            return Err(format!("'{}': step must be greater than 0", c.key));
        }
    }
    check_merged_text(c, mentioned)
}

/// The text rules, when the patch touches them.
fn check_merged_text(c: &UiComponent, mentioned: &[&str]) -> Result<(), String> {
    let touches = |names: &[&str]| names.iter().any(|n| mentioned.contains(n));
    if let (true, Some(min), Some(max)) = (
        touches(&["min_length", "max_length"]),
        c.min_length,
        c.max_length,
    ) {
        if min > max {
            return Err(format!(
                "'{}': min_length ({min}) is greater than max_length ({max})",
                c.key
            ));
        }
    }
    if let (true, Some(pattern)) = (touches(&["pattern"]), &c.pattern) {
        if let Err(e) = compile_field_pattern(pattern) {
            return Err(format!("'{}': pattern does not compile: {e}", c.key));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::validate_response;
    use std::collections::HashMap;

    fn option(v: &str) -> UiOption {
        UiOption {
            label: v.to_string(),
            value: v.to_string(),
            image: None,
        }
    }

    fn select(key: &str) -> UiComponent {
        UiComponent {
            key: key.into(),
            required: true,
            ..UiComponent::new(ComponentType::Select)
        }
    }

    fn patch(json: &str) -> UiPropertyPatch {
        serde_json::from_str(json).unwrap()
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The drift gate: a field added to the reader table must be placed
    /// in one of the two lists here, deliberately.
    #[test]
    fn every_reader_field_has_a_decision() {
        for (field, _) in COMPONENT_FIELD_READERS {
            let settable = SETTABLE_TYPE_SPECIFIC_FIELDS.contains(field);
            let closed = CLOSED_TYPE_SPECIFIC_FIELDS.contains(field);
            assert!(
                settable ^ closed,
                "field '{field}' of COMPONENT_FIELD_READERS must be in exactly one of \
                 SETTABLE_TYPE_SPECIFIC_FIELDS / CLOSED_TYPE_SPECIFIC_FIELDS"
            );
        }
        let table: Vec<&str> = COMPONENT_FIELD_READERS.iter().map(|(f, _)| *f).collect();
        for field in SETTABLE_TYPE_SPECIFIC_FIELDS
            .iter()
            .chain(CLOSED_TYPE_SPECIFIC_FIELDS)
        {
            assert!(
                table.contains(field),
                "'{field}' is listed here but not in COMPONENT_FIELD_READERS"
            );
        }
    }

    /// The patch struct must name exactly the settable properties: a
    /// field added to the allow-list without a patch field (or the
    /// reverse) would parse as unknown or never be applied.
    #[test]
    fn patch_fields_are_exactly_the_settable_properties() {
        let full = patch(
            r#"{"label":"l","description":"d","placeholder":"p","prefix":"a","suffix":"b",
                "options":[],"min":0,"max":1,"step":1,"min_length":0,"max_length":1,
                "pattern":"x","pattern_message":"m"}"#,
        );
        let mut mentioned = full.mentioned();
        mentioned.sort_unstable();
        let mut expected: Vec<&str> = UNIVERSAL_SETTABLE_PROPERTIES
            .iter()
            .chain(SETTABLE_TYPE_SPECIFIC_FIELDS)
            .copied()
            .collect();
        expected.sort_unstable();
        assert_eq!(mentioned, expected);
    }

    #[test]
    fn settable_properties_follow_the_reader_table() {
        assert_eq!(
            settable_properties(ComponentType::Select),
            vec!["label", "description", "placeholder", "options"]
        );
        assert_eq!(
            settable_properties(ComponentType::NumberInput),
            vec![
                "label",
                "description",
                "placeholder",
                "prefix",
                "suffix",
                "min",
                "max",
                "step"
            ]
        );
        // Display components keep their universal properties and lose
        // the layout ones.
        assert_eq!(
            settable_properties(ComponentType::Text),
            vec!["label", "description"]
        );
    }

    #[test]
    fn unknown_property_fails_to_parse() {
        let err = serde_json::from_str::<UiPropertyPatch>(r#"{"colour":"red"}"#).unwrap_err();
        assert!(err.to_string().contains("unknown field `colour`"), "{err}");
    }

    #[test]
    fn null_clears_and_absence_leaves_alone() {
        let p = patch(r#"{"max":null}"#);
        assert_eq!(p.max, Some(None));
        assert_eq!(p.min, None);
        let mut c = UiComponent {
            key: "v".into(),
            min: Some(1.0),
            max: Some(5.0),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        p.apply(&mut c).unwrap();
        assert_eq!(c.min, Some(1.0));
        assert_eq!(c.max, None);
        // Round-trips as null so consumers see the clearing too.
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"max":null}"#);
    }

    #[test]
    fn property_the_type_does_not_read_is_refused_untouched() {
        let mut c = UiComponent {
            key: "sn".into(),
            placeholder: Some("Scan".into()),
            ..UiComponent::new(ComponentType::TextInput)
        };
        let err = patch(r#"{"placeholder":"x","options":[{"value":"a","label":"A"}]}"#)
            .apply(&mut c)
            .unwrap_err();
        assert!(
            err.contains("'sn' is a text_input and has no 'options' property"),
            "{err}"
        );
        assert!(
            err.contains("settable: label, description, placeholder"),
            "{err}"
        );
        assert_eq!(
            c.placeholder.as_deref(),
            Some("Scan"),
            "refusal must be atomic"
        );
    }

    #[test]
    fn empty_patch_is_refused() {
        let err = patch("{}").apply(&mut select("mo")).unwrap_err();
        assert!(err.contains("changes nothing"), "{err}");
    }

    /// The submit validator trims the answer before comparing it with the
    /// list, so a padded value (a CHAR column) could never be chosen.
    #[test]
    fn padded_option_values_are_refused() {
        let mut c = select("mo");
        let err = patch(r#"{"options":[{"value":"MO-1043   ","label":"MO-1043"}]}"#)
            .apply(&mut c)
            .unwrap_err();
        assert!(err.contains("surrounding whitespace"), "{err}");
        assert!(c.options.is_none());
    }

    #[test]
    fn options_need_unique_non_empty_values() {
        let mut c = select("mo");
        let err = patch(r#"{"options":[{"value":"","label":"A"}]}"#)
            .apply(&mut c)
            .unwrap_err();
        assert!(err.contains("option 1 has an empty value"), "{err}");
        let err = patch(r#"{"options":[{"value":"A","label":"A"},{"value":"A","label":"B"}]}"#)
            .apply(&mut c)
            .unwrap_err();
        assert!(err.contains("appears twice"), "{err}");
        assert!(c.options.is_none());
    }

    /// The YAML loader's caps hold for a run-time list too.
    #[test]
    fn run_time_options_keep_the_yaml_caps() {
        let mut c = select("mo");
        let long = "x".repeat(MAX_OPTION_TEXT_CHARS + 1);
        let err = patch(&format!(
            r#"{{"options":[{{"value":"a","label":"{long}"}}]}}"#
        ))
        .apply(&mut c)
        .unwrap_err();
        assert!(err.contains("label is longer than 200"), "{err}");
        let grid: Vec<String> = (0..=MAX_OPTIONS_WITH_IMAGES)
            .map(|i| format!(r#"{{"value":"v{i}","label":"V{i}","image":"img{i}.png"}}"#))
            .collect();
        let err = patch(&format!(r#"{{"options":[{}]}}"#, grid.join(",")))
            .apply(&mut c)
            .unwrap_err();
        assert!(err.contains("13 options with images"), "{err}");
        assert!(c.options.is_none(), "refusal is atomic");
    }

    #[test]
    fn set_properties_action_is_the_enum_wire_name() {
        assert_eq!(
            serde_json::to_string(&crate::UiUpdateAction::SetProperties).unwrap(),
            format!("\"{SET_PROPERTIES_ACTION}\"")
        );
    }

    #[test]
    fn bounds_are_checked_on_the_merged_component() {
        let mut c = UiComponent {
            key: "v".into(),
            max: Some(5.0),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let err = patch(r#"{"min":10}"#).apply(&mut c).unwrap_err();
        assert!(err.contains("min (10) is greater than max (5)"), "{err}");
        assert_eq!(c.min, None);
        // Raising both together is fine.
        patch(r#"{"min":10,"max":20}"#).apply(&mut c).unwrap();
        assert_eq!((c.min, c.max), (Some(10.0), Some(20.0)));
        let err = patch(r#"{"step":0}"#).apply(&mut c).unwrap_err();
        assert!(err.contains("step must be greater than 0"), "{err}");
    }

    /// A rule the call does not touch is not re-judged: a YAML pattern the
    /// loader accepted but the station cannot compile is the submit's
    /// business, not `ui.set("sn", label=...)`'s.
    #[test]
    fn untouched_fields_are_not_rejudged() {
        let mut c = UiComponent {
            key: "sn".into(),
            pattern: Some("[unclosed".into()),
            options: Some(vec![option("A"), option("A")]),
            ..UiComponent::new(ComponentType::TextInput)
        };
        patch(r#"{"label":"Serial"}"#).apply(&mut c).unwrap();
        assert_eq!(c.label.as_deref(), Some("Serial"));
        // Touching the pattern judges the pattern.
        let err = patch(r#"{"pattern":"[still"}"#).apply(&mut c).unwrap_err();
        assert!(err.contains("pattern does not compile"), "{err}");
    }

    #[test]
    fn pattern_must_compile() {
        let mut c = UiComponent {
            key: "sn".into(),
            ..UiComponent::new(ComponentType::TextInput)
        };
        let err = patch(r#"{"pattern":"[unclosed"}"#)
            .apply(&mut c)
            .unwrap_err();
        assert!(err.contains("pattern does not compile"), "{err}");
        patch(r#"{"pattern":"^MO-[0-9]+$","pattern_message":"Scan the MO label"}"#)
            .apply(&mut c)
            .unwrap();
        assert_eq!(c.pattern.as_deref(), Some("^MO-[0-9]+$"));
    }

    // -- The point of the whole module: the submit check follows the --
    // -- patched component, not the YAML one.                        --

    #[test]
    fn run_time_option_is_accepted_and_outsider_rejected() {
        let mut c = select("mo");
        c.options = Some(vec![option("YAML-1")]);
        let p = UiPropertyPatch {
            options: Some(Some(vec![option("MO-1042"), option("MO-1043")])),
            ..Default::default()
        };
        p.apply(&mut c).unwrap();
        assert!(
            validate_response(std::slice::from_ref(&c), &values(&[("mo", "MO-1043")])).is_empty()
        );
        let errors = validate_response(std::slice::from_ref(&c), &values(&[("mo", "YAML-1")]));
        assert_eq!(errors["mo"], "Pick from the list");
    }

    #[test]
    fn empty_run_time_list_still_requires_a_choice() {
        let mut c = select("mo");
        patch(r#"{"options":[]}"#).apply(&mut c).unwrap();
        let errors = validate_response(std::slice::from_ref(&c), &values(&[("mo", "")]));
        assert_eq!(errors["mo"], "Required");
        // An answer sent before the list arrived names an order nothing
        // offered: refused, for an empty list and for no list at all.
        let errors = validate_response(std::slice::from_ref(&c), &values(&[("mo", "MO-1")]));
        assert_eq!(errors["mo"], "Pick from the list");
        c.options = None;
        let errors = validate_response(std::slice::from_ref(&c), &values(&[("mo", "MO-1")]));
        assert_eq!(errors["mo"], "Pick from the list");
        // A non-required choice with no options submits empty: decided
        // 2026-10-08 (block only when required).
        c.required = false;
        assert!(validate_response(std::slice::from_ref(&c), &values(&[("mo", "")])).is_empty());
    }

    #[test]
    fn updated_bounds_and_pattern_are_the_ones_enforced() {
        let mut n = UiComponent {
            key: "v".into(),
            min: Some(0.0),
            max: Some(100.0),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        patch(r#"{"min":10,"max":20}"#).apply(&mut n).unwrap();
        let errors = validate_response(std::slice::from_ref(&n), &values(&[("v", "50")]));
        assert_eq!(errors["v"], "Must be between 10 and 20");

        let mut t = UiComponent {
            key: "sn".into(),
            ..UiComponent::new(ComponentType::TextInput)
        };
        patch(r#"{"pattern":"^MO-[0-9]+$","pattern_message":"Scan the MO label"}"#)
            .apply(&mut t)
            .unwrap();
        let errors = validate_response(std::slice::from_ref(&t), &values(&[("sn", "SN-1")]));
        assert_eq!(errors["sn"], "Scan the MO label");
    }

    #[test]
    fn payload_round_trips() {
        let json = r#"{"id":"mo","properties":{"options":[{"label":"A","value":"a"}]}}"#;
        let payload: UiSetPropertiesPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.id, "mo");
        assert_eq!(serde_json::to_string(&payload).unwrap(), json);
    }
}
