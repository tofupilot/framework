//! `ui.wait()`: the pure half of a phase waiting for its operator mid-phase.
//!
//! The job loop in `runner.rs` owns the lifecycle (register the prompt,
//! select on the answer and the worker socket, write the answer back).
//! What lives here has no I/O, so the two contracts it carries are
//! unit-tested without a Python worker:
//!
//! * what the phase receives: [`typed_answer`], typed per component
//!   (number → float, switch → bool, multi-choice → list, an affixed text
//!   input → `prefix + input + suffix`). Close to what `bind:` records,
//!   not the same: `bind:` joins a multi-choice into one comma-separated
//!   string and skips an empty number;
//! * what the operator sees when the screen comes back:
//!   [`components_for_rearm`], the live components (YAML plus every
//!   `ui.set`) carrying the latest values (the operator's previous answer,
//!   then any `ui.<key> = …` written since).

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::ui::{ComponentType, ComponentValue, UiComponent};

/// A number or slider on the wire: `None` when empty or not finite.
fn wire_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

/// A switch on the wire.
fn wire_switch(s: &str) -> bool {
    s == "true"
}

/// A multi-choice on the wire: its values, comma-joined.
fn wire_choices(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').filter(|p| !p.is_empty())
}

/// The operator's answer as the phase sees it: one entry per input
/// component of the screen, typed. Wire strings are what every surface
/// submits (`HashMap<String, String>`); sentinels such as
/// `__bound_measurements__` are not components and never appear.
///
/// An input left empty reads `None` for a number, a slider and a single
/// choice, `""` for text, `[]` for a multi-choice and `False` for a switch,
/// so a key is always present and `answer["key"]` never raises.
pub(crate) fn typed_answer(
    components: &[UiComponent],
    raw: &HashMap<String, String>,
) -> Map<String, Value> {
    let mut out = Map::new();
    for comp in components.iter().filter(|c| c.is_input && !c.locked) {
        let submitted = raw.get(&comp.key).map(String::as_str).unwrap_or("");
        let s = if comp.trim {
            submitted.trim()
        } else {
            submitted
        };
        let value = match comp.component_type {
            ComponentType::NumberInput | ComponentType::Slider => {
                wire_number(s).map(Value::from).unwrap_or(Value::Null)
            }
            ComponentType::Switch => Value::Bool(wire_switch(s)),
            ComponentType::Multiselect | ComponentType::Checklist => Value::Array(
                wire_choices(s)
                    .map(|p| Value::String(p.to_string()))
                    .collect(),
            ),
            ComponentType::Radio | ComponentType::Select => {
                if s.is_empty() {
                    Value::Null
                } else {
                    Value::String(s.to_string())
                }
            }
            // Same rule as `compose_bound_affixes`: the affixes render as
            // locked adornments around the typed text, so the value is the
            // whole string; a blank entry stays blank, never a bare prefix.
            ComponentType::TextInput if !s.trim().is_empty() => Value::String(format!(
                "{}{s}{}",
                comp.prefix.as_deref().unwrap_or(""),
                comp.suffix.as_deref().unwrap_or("")
            )),
            ComponentType::TextInput | ComponentType::Textarea => Value::String(s.to_string()),
            ComponentType::Text | ComponentType::Image | ComponentType::Progress => continue,
        };
        out.insert(comp.key.clone(), value);
    }
    out
}

/// The values a re-armed screen opens on, per component key. `None` is
/// an explicit clear (a number the operator emptied, `ui.<key> = None`):
/// the form opens empty, not on the YAML default.
pub(crate) type FormValues = HashMap<String, Option<ComponentValue>>;

/// The operator's answer folded back into form values, so the screen a
/// later `wait()` shows holds what was typed last instead of the YAML
/// defaults. Kept in wire shape (the input text, without affixes): this
/// pre-fills the form, it is not what the phase receives.
pub(crate) fn answer_as_form_values(
    components: &[UiComponent],
    raw: &HashMap<String, String>,
    into: &mut FormValues,
) {
    for comp in components.iter().filter(|c| c.is_input && !c.locked) {
        let Some(s) = raw.get(&comp.key) else {
            continue;
        };
        let value = match comp.component_type {
            ComponentType::NumberInput | ComponentType::Slider => {
                match wire_number(s) {
                    Some(n) => ComponentValue::Number(n),
                    // Cleared by the operator: the next form opens empty,
                    // not on an earlier answer nor on the YAML default.
                    None => {
                        into.insert(comp.key.clone(), None);
                        continue;
                    }
                }
            }
            ComponentType::Switch => ComponentValue::Boolean(wire_switch(s)),
            ComponentType::Multiselect | ComponentType::Checklist => {
                ComponentValue::Array(wire_choices(s).map(str::to_string).collect())
            }
            _ => ComponentValue::String(s.clone()),
        };
        into.insert(comp.key.clone(), Some(value));
    }
}

/// A `ui.<key> = value` payload value as a component value. `None` for a
/// shape no component holds (an object, a mixed list): the screen keeps
/// what it had.
pub(crate) fn component_value_from_json(value: &Value) -> Option<ComponentValue> {
    // The type's own (untagged) deserializer, so a new variant is picked
    // up here too.
    ComponentValue::deserialize(value).ok()
}

/// The screen a re-armed `wait()` shows: the live components with each
/// held value as the component's runtime `value`, the field every surface
/// reads first when it opens a form (web `getComponentDefaultValue`, TUI
/// `ComponentState::init`, the kiosk's text and progress renderers).
pub(crate) fn components_for_rearm(live: &[UiComponent], values: &FormValues) -> Vec<UiComponent> {
    live.iter()
        .map(|comp| {
            let mut comp = comp.clone();
            match values.get(&comp.key) {
                Some(Some(v)) => comp.value = Some(v.clone()),
                // Every surface falls back to `default_value` when `value`
                // is empty, so a clear drops both.
                Some(None) => {
                    comp.value = None;
                    comp.default_value = None;
                }
                None => {}
            }
            comp
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(key: &str, t: ComponentType) -> UiComponent {
        UiComponent {
            key: key.into(),
            ..UiComponent::new(t)
        }
    }

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn answer_is_typed_per_component() {
        let components = vec![
            comp("offset", ComponentType::NumberInput),
            comp("gain", ComponentType::Slider),
            comp("done", ComponentType::Switch),
            comp("checks", ComponentType::Checklist),
            comp("mode", ComponentType::Select),
            comp("note", ComponentType::Textarea),
            comp("reading", ComponentType::Text),
        ];
        let out = typed_answer(
            &components,
            &raw(&[
                ("offset", "0.25"),
                ("gain", "3"),
                ("done", "true"),
                ("checks", "a,b"),
                ("mode", "fast"),
                ("note", "ok"),
                ("__bound_measurements__", "{}"),
            ]),
        );
        assert_eq!(
            Value::Object(out),
            serde_json::json!({
                "offset": 0.25,
                "gain": 3.0,
                "done": true,
                "checks": ["a", "b"],
                "mode": "fast",
                "note": "ok",
            }),
            "display components and sentinels are not answers"
        );
    }

    #[test]
    fn empty_inputs_have_a_value_of_their_type() {
        let components = vec![
            comp("n", ComponentType::NumberInput),
            comp("s", ComponentType::Switch),
            comp("m", ComponentType::Multiselect),
            comp("r", ComponentType::Radio),
            comp("t", ComponentType::TextInput),
        ];
        let out = typed_answer(&components, &HashMap::new());
        assert_eq!(
            Value::Object(out),
            serde_json::json!({"n": null, "s": false, "m": [], "r": null, "t": ""})
        );
    }

    #[test]
    fn affixed_text_reads_whole_and_blank_stays_blank() {
        let sn = UiComponent {
            prefix: Some("PCB-".into()),
            suffix: Some("-A".into()),
            trim: false,
            ..comp("sn", ComponentType::TextInput)
        };
        let out = typed_answer(std::slice::from_ref(&sn), &raw(&[("sn", "1234")]));
        assert_eq!(out["sn"], "PCB-1234-A");
        let out = typed_answer(&[sn], &raw(&[("sn", "  ")]));
        assert_eq!(out["sn"], "  ", "a blank entry never records a bare prefix");
    }

    #[test]
    fn locked_and_trimmed_fields() {
        let locked = UiComponent {
            locked: true,
            ..comp("lot", ComponentType::TextInput)
        };
        let trimmed = UiComponent {
            trim: true,
            ..comp("op", ComponentType::TextInput)
        };
        let out = typed_answer(&[locked, trimmed], &raw(&[("lot", "x"), ("op", "  ana ")]));
        assert!(!out.contains_key("lot"));
        assert_eq!(out["op"], "ana");
    }

    #[test]
    fn rearmed_screen_carries_the_last_values() {
        let components = vec![
            comp("offset", ComponentType::NumberInput),
            comp("reading", ComponentType::Text),
            comp("checks", ComponentType::Checklist),
        ];
        let mut held = HashMap::new();
        answer_as_form_values(
            &components,
            &raw(&[("offset", "0.5"), ("checks", "a")]),
            &mut held,
        );
        // A `ui.reading = …` written after the answer.
        held.insert(
            "reading".into(),
            component_value_from_json(&serde_json::json!("3.210 V")),
        );
        let out = components_for_rearm(&components, &held);
        assert_eq!(out[0].value, Some(ComponentValue::Number(0.5)));
        assert_eq!(out[1].value, Some(ComponentValue::String("3.210 V".into())));
        assert_eq!(out[2].value, Some(ComponentValue::Array(vec!["a".into()])));

        // A later answer that clears the number empties it on the next
        // form, YAML default included.
        let with_default = vec![UiComponent {
            default_value: Some(ComponentValue::Number(1.0)),
            ..comp("offset", ComponentType::NumberInput)
        }];
        answer_as_form_values(&with_default, &raw(&[("offset", "")]), &mut held);
        let out = components_for_rearm(&with_default, &held);
        assert_eq!(out[0].value, None);
        assert_eq!(out[0].default_value, None);
    }

    #[test]
    fn unrepresentable_ui_values_are_ignored() {
        assert_eq!(
            component_value_from_json(&serde_json::json!({"a": 1})),
            None
        );
        assert_eq!(
            component_value_from_json(&serde_json::json!([1, "a"])),
            None
        );
        assert_eq!(component_value_from_json(&Value::Null), None);
    }
}
