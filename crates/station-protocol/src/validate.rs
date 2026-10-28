//! Operator-response validation against a prompt's component spec.
//!
//! The one validator behind the CLI, station and web submit paths: the
//! CLI's `ui_response` chokepoint runs it before a kiosk / web-console
//! submission resolves the waiting phase (a non-empty result ships back
//! as `StationEvent::UiResponseRejected` and the prompt stays open), and
//! the TUI runs it in-process. Clients render what it returns and
//! validate nothing themselves — one implementation, one wording,
//! nothing to drift.

use std::collections::HashMap;

use crate::{ComponentType, UiComponent};

/// Validate submitted wire-string `values` against `components`.
/// Returns one operator-facing message per failing component key;
/// empty means the submission is acceptable. Keys in `values` that
/// don't belong to an input component (`__bound_measurements__`, …)
/// are ignored — sentinels are the submitter's business.
///
/// The message ladder for a pattern failure never shows the raw regex:
/// the authored `pattern_message`, else the derived character-level
/// message, else the bare verdict.
pub fn validate_response(
    components: &[UiComponent],
    values: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut errors = HashMap::new();
    for comp in components {
        // A locked identify field is not the operator's to fix: the
        // engine judges its configured default and ignores the wire
        // value, so rejecting here would block a submit on a field
        // nobody can edit.
        if !comp.is_input || comp.locked {
            continue;
        }
        if let Some(message) = validate_component(comp, values.get(&comp.key)) {
            errors.insert(comp.key.clone(), message);
        }
    }
    errors
}

/// Validate a single component's wire string (`None` = not submitted).
/// Public so per-field surfaces (the TUI's focus-driven form, Studio's
/// field rows) can validate one field with the exact wording a full
/// submit would produce.
pub fn validate_component(comp: &UiComponent, raw: Option<&String>) -> Option<String> {
    let raw = raw.map(String::as_str).unwrap_or("");
    // Clients trim before submitting when `trim` is set; re-applying it
    // here makes the wire check hold for any submitter.
    let value = if comp.trim { raw.trim() } else { raw };

    if value.is_empty() {
        return match comp.component_type {
            ComponentType::Multiselect | ComponentType::Checklist if comp.required => {
                Some("Pick at least one".to_string())
            }
            _ if comp.required => Some("Required".to_string()),
            _ => None,
        };
    }

    match comp.component_type {
        ComponentType::TextInput | ComponentType::Textarea => validate_text(comp, value),
        ComponentType::NumberInput | ComponentType::Slider => validate_number(comp, value),
        // A choice is judged against the list on the component, and a
        // component without a list offers nothing yet: the phase fills it
        // at run time (`ui.set`), and until then any value is outside it.
        // Letting a value through here would record an order the MES
        // never offered, from an answer sent before the list arrived.
        ComponentType::Radio | ComponentType::Select => {
            let options = comp.options.as_deref().unwrap_or(&[]);
            if !options.iter().any(|o| o.value == value) {
                return Some("Pick from the list".to_string());
            }
            None
        }
        ComponentType::Multiselect | ComponentType::Checklist => {
            // CSV wire shape (the agent path and the web client both
            // join selections with ',').
            let options = comp.options.as_deref().unwrap_or(&[]);
            for part in value.split(',').filter(|p| !p.is_empty()) {
                if !options.iter().any(|o| o.value == part) {
                    return Some("Pick from the list".to_string());
                }
            }
            None
        }
        // Switch arrives as "true"/"false"; a present value is valid.
        // Display components are filtered by `is_input` upstream.
        _ => None,
    }
}

/// The server's charset on a unit identity value, which holds whatever
/// pattern the procedure declares. `None` when the value passes.
pub fn validate_identity_charset(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let pattern = crate::pattern_messages::UNIT_FIELD_CHARSET_PATTERN;
    let re = crate::pattern_messages::compile_field_pattern(pattern).ok()?;
    if re.is_match(value) {
        return None;
    }
    Some(
        crate::pattern_messages::derive_pattern_error(pattern, value)
            .unwrap_or_else(|| "Letters, digits and _ . : + - only".to_string()),
    )
}

/// Operator-facing verdict for a field whose authored pattern does not
/// compile. The operator cannot fix it, so the message points at the
/// procedure rather than at their entry.
pub const INVALID_PATTERN_MESSAGE: &str = "Can't be checked: procedure error";

fn validate_text(comp: &UiComponent, value: &str) -> Option<String> {
    let len = value.chars().count();
    if let Some(min) = comp.min_length {
        let min = min as usize;
        if len < min {
            let s = if min != 1 { "s" } else { "" };
            return Some(format!("Too short: {len}/{min} character{s}"));
        }
    }
    if let Some(max) = comp.max_length {
        let max = max as usize;
        if len > max {
            let s = if max != 1 { "s" } else { "" };
            return Some(format!("Too long: {len}/{max} character{s}"));
        }
    }
    if let Some(ref pattern) = comp.pattern {
        // A pattern the station cannot compile is rejected, not skipped:
        // skipping accepted every entry in a field the author tried to
        // lock down, silently and forever. Same posture as the unit
        // fields (execution-engine `unit.rs`), same "never quote the
        // raw regex" rule.
        let Ok(re) = crate::pattern_messages::compile_field_pattern(pattern) else {
            return Some(INVALID_PATTERN_MESSAGE.to_string());
        };
        if !re.is_match(value) {
            return Some(match comp.pattern_message {
                Some(ref msg) => msg.clone(),
                None => crate::pattern_messages::derive_pattern_error(pattern, value)
                    .unwrap_or_else(|| "Wrong format".to_string()),
            });
        }
    }
    None
}

fn validate_number(comp: &UiComponent, value: &str) -> Option<String> {
    let Ok(n) = value.parse::<f64>() else {
        return Some("Numbers only".to_string());
    };
    if !n.is_finite() {
        return Some("Numbers only".to_string());
    }
    let below = comp.min.is_some_and(|min| n < min);
    let above = comp.max.is_some_and(|max| n > max);
    if below || above {
        return Some(match (comp.min, comp.max) {
            (Some(min), Some(max)) => format!("Must be between {min} and {max}"),
            (Some(min), None) => format!("Must be at least {min}"),
            (None, Some(max)) => format!("Must be at most {max}"),
            (None, None) => unreachable!("bound violated without a bound"),
        });
    }
    if let Some(step) = comp.step {
        if step != 0.0 {
            let rem = n % step;
            if rem.abs() > 1e-6 && (rem - step).abs() > 1e-6 {
                return Some(format!("Must be a multiple of {step}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UiOption;

    fn text(key: &str) -> UiComponent {
        UiComponent {
            key: key.into(),
            required: true,
            ..UiComponent::new(ComponentType::TextInput)
        }
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn missing_required_is_rejected() {
        let errors = validate_response(&[text("sn")], &HashMap::new());
        assert_eq!(errors["sn"], "Required");
    }

    #[test]
    fn optional_empty_passes() {
        let comp = UiComponent {
            required: false,
            ..text("note")
        };
        assert!(validate_response(&[comp], &values(&[("note", "")])).is_empty());
    }

    #[test]
    fn pattern_failure_derives_character_level_message() {
        let comp = UiComponent {
            pattern: Some("^[A-Z0-9-]+$".into()),
            ..text("sn")
        };
        let errors = validate_response(&[comp], &values(&[("sn", "AB 1")]));
        assert_eq!(
            errors["sn"],
            "Remove the space (character 3). Allowed: uppercase letters, digits, -"
        );
    }

    /// TP-1086: a pattern is a format lock, not a search. Before this,
    /// `is_match` was a substring search and a mislabeled scan carrying
    /// a valid serial inside it validated fine.
    #[test]
    fn pattern_must_match_the_whole_entry() {
        let comp = UiComponent {
            pattern: Some("SN-[0-9]+".into()),
            ..text("sn")
        };
        assert!(validate_response(std::slice::from_ref(&comp), &values(&[("sn", "SN-1234")])).is_empty());
        assert!(!validate_response(&[comp], &values(&[("sn", "SCRAP SN-1234 XX")])).is_empty());
    }

    /// The documented "starts with" idiom (`^BAT-.*`) keeps its meaning:
    /// the trailing `.*` already reached the end of the entry.
    #[test]
    fn open_ended_prefix_pattern_still_means_starts_with() {
        let comp = UiComponent {
            pattern: Some("^BAT-.*".into()),
            ..text("batch")
        };
        assert!(validate_response(std::slice::from_ref(&comp), &values(&[("batch", "BAT-2026-01")])).is_empty());
        assert!(!validate_response(&[comp], &values(&[("batch", "X-BAT-2026")])).is_empty());
    }

    /// Anchors are the author's option, never their obligation: the
    /// rule and the wording are identical with and without them.
    #[test]
    fn anchored_and_bare_patterns_are_the_same_rule() {
        let recital = "Remove the space (character 3). Allowed: uppercase letters, digits, -";
        for pattern in ["^[A-Z0-9-]+$", "[A-Z0-9-]+"] {
            let comp = UiComponent {
                pattern: Some(pattern.into()),
                ..text("sn")
            };
            assert!(validate_response(std::slice::from_ref(&comp), &values(&[("sn", "AB-1")])).is_empty());
            let errors = validate_response(&[comp], &values(&[("sn", "AB 1")]));
            assert_eq!(errors["sn"], recital, "pattern {pattern}");
        }
    }

    #[test]
    fn authored_pattern_message_wins() {
        let comp = UiComponent {
            pattern: Some("^[A-Z]+$".into()),
            pattern_message: Some("Scan the label under the battery".into()),
            ..text("sn")
        };
        let errors = validate_response(&[comp], &values(&[("sn", "ab")]));
        assert_eq!(errors["sn"], "Scan the label under the battery");
    }

    #[test]
    fn non_derivable_pattern_falls_back_to_bare_verdict() {
        let comp = UiComponent {
            pattern: Some(r"^(\d{3}|\d{5})$".into()),
            ..text("zip")
        };
        let errors = validate_response(&[comp], &values(&[("zip", "1234")]));
        assert_eq!(errors["zip"], "Wrong format");
    }

    /// A pattern the `regex` crate refuses (a syntax error, or a
    /// lookaround / backreference that JavaScript accepts) must not
    /// fall through to "anything goes": the field is rejected until
    /// the author fixes the procedure.
    #[test]
    fn unparseable_pattern_is_rejected() {
        for pattern in ["[unclosed", r"^(?=.*\d)[A-Z0-9]{6}$", r"^(a)\1$"] {
            let comp = UiComponent {
                pattern: Some(pattern.into()),
                ..text("sn")
            };
            let errors = validate_response(&[comp], &values(&[("sn", "anything")]));
            assert_eq!(errors["sn"], INVALID_PATTERN_MESSAGE, "pattern {pattern}");
        }
    }

    #[test]
    fn trims_before_checking_and_ignores_sentinels() {
        let comp = UiComponent {
            pattern: Some("^[A-Z]+$".into()),
            ..text("sn")
        };
        let vals = values(&[("sn", "  ABC  "), ("__bound_measurements__", "{}")]);
        assert!(validate_response(&[comp], &vals).is_empty());
    }

    #[test]
    fn length_counts_and_names_the_overshoot() {
        let comp = UiComponent {
            max_length: Some(3),
            ..text("code")
        };
        let errors = validate_response(&[comp], &values(&[("code", "ABCDE")]));
        assert_eq!(
            errors["code"],
            "Too long: 5/3 characters"
        );
    }

    #[test]
    fn number_bounds_and_nan() {
        let comp = UiComponent {
            key: "v".into(),
            required: true,
            min: Some(1.0),
            max: Some(10.0),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let errors = validate_response(std::slice::from_ref(&comp), &values(&[("v", "12")]));
        assert_eq!(errors["v"], "Must be between 1 and 10");
        let errors = validate_response(std::slice::from_ref(&comp), &values(&[("v", "abc")]));
        assert_eq!(errors["v"], "Numbers only");
        let errors = validate_response(std::slice::from_ref(&comp), &values(&[("v", "NaN")]));
        assert_eq!(errors["v"], "Numbers only");
    }

    #[test]
    fn select_rejects_unknown_option() {
        let comp = UiComponent {
            key: "mode".into(),
            required: true,
            options: Some(vec![UiOption {
                label: "A".into(),
                value: "A".into(),
                image: None,
            }]),
            ..UiComponent::new(ComponentType::Select)
        };
        let errors = validate_response(&[comp], &values(&[("mode", "B")]));
        assert_eq!(errors["mode"], "Pick from the list");
    }

    #[test]
    fn display_components_are_ignored() {
        let comp = UiComponent {
            key: "title".into(),
            required: false,
            ..UiComponent::new(ComponentType::Text)
        };
        assert!(validate_response(&[comp], &HashMap::new()).is_empty());
    }
}
