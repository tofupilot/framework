//! Single source of truth for assembling the `__bound_measurements__`
//! sentinel from `bind:` UI components. The engine reads this sentinel
//! out of a UI response's `values` map (`worker::extract_bound_measurements`)
//! and turns it into phase measurements / unit fields — a bare
//! `values[key]` is ignored. Every operator-UI surface (TUI, agent
//! protocol, and the React/web client's `buildBoundMeasurementsPayload`)
//! must produce the identical payload so a procedure records the same
//! measurement regardless of which UI ran it.
//!
//! The web reference is `packages/operator-ui/src/state/wire-encoding.ts`.

use std::collections::HashMap;

use super::types::{ComponentType, UiComponent};

/// Build the `__bound_measurements__` JSON payload from components that
/// carry a `bind` directive. `resolve` returns the operator's answer for
/// a component as a string (already trimmed/joined by the caller), or
/// `None` to skip it. Returns `None` when nothing binds.
///
/// Routing mirrors the web client:
/// - `measurements.X` / `measurement.X` → typed scalar under `X`
/// - `unit.X` → string under `__unit__.X`
/// - `unit.sub_units.X` → string under `__unit__.sub_units.X`
/// - `run.operated_by` → string under `__run__.operated_by` (run
///   attribution; the only run-scoped bind target today)
///
/// Only measurements are typed: number/slider → JSON number (empty, not a
/// full number, or non-finite skipped), switch → bool, everything else →
/// the string as-is. Unit and run fields stay strings whatever the
/// component type, since `worker::extract_unit_info_from_json` reads them
/// with `as_str()`. Empty bind names (`bind: measurements.`) are skipped
/// to match the web regex `^measurements?\.(.+)$`, which requires a name.
/// Both builders run
/// `packages/operator-ui/fixtures/operator-protocol/bound-payload.json`.
pub fn build_bound_measurements_payload<F>(components: &[UiComponent], resolve: F) -> Option<String>
where
    F: Fn(&UiComponent) -> Option<String>,
{
    let mut measurements = serde_json::Map::new();
    let mut unit_fields = serde_json::Map::new();
    let mut sub_units = serde_json::Map::new();
    let mut run_fields = serde_json::Map::new();

    for comp in components {
        let Some(bind) = comp.bind.as_deref() else {
            continue;
        };
        let Some(raw) = resolve(comp) else {
            continue;
        };

        if let Some(name) = bind
            .strip_prefix("measurements.")
            .or_else(|| bind.strip_prefix("measurement."))
        {
            if name.is_empty() {
                continue;
            }
            let typed = match comp.component_type {
                ComponentType::NumberInput | ComponentType::Slider => {
                    if raw.is_empty() {
                        continue;
                    }
                    // `inf` / `NaN` parse but have no JSON form.
                    match raw.parse::<f64>() {
                        Ok(n) if n.is_finite() => serde_json::json!(n),
                        _ => continue,
                    }
                }
                ComponentType::Switch => serde_json::json!(raw == "true"),
                _ => serde_json::Value::String(raw),
            };
            measurements.insert(name.to_string(), typed);
        } else if let Some(field) = bind.strip_prefix("unit.") {
            // Unit fields are always strings on the wire (serial/part/etc.).
            if let Some(sub) = field.strip_prefix("sub_units.") {
                if !sub.is_empty() {
                    sub_units.insert(sub.to_string(), serde_json::Value::String(raw));
                }
            } else if !field.is_empty() {
                unit_fields.insert(field.to_string(), serde_json::Value::String(raw));
            }
        } else if let Some(field) = bind.strip_prefix("run.") {
            // Run properties are strings too (operated_by email).
            if !field.is_empty() {
                run_fields.insert(field.to_string(), serde_json::Value::String(raw));
            }
        }
    }

    if measurements.is_empty()
        && unit_fields.is_empty()
        && sub_units.is_empty()
        && run_fields.is_empty()
    {
        return None;
    }

    let mut out = measurements;
    if !unit_fields.is_empty() || !sub_units.is_empty() {
        let mut unit_obj = unit_fields;
        if !sub_units.is_empty() {
            unit_obj.insert(
                "sub_units".to_string(),
                serde_json::Value::Object(sub_units),
            );
        }
        // Ship `__unit__` as a JSON string, byte-identical to the web
        // client (`wire-encoding.ts`: `out.__unit__ = JSON.stringify(unitObj)`).
        // The engine's `extract_bound_measurements` accepts either a nested
        // object or a string, but emitting the same form on every surface
        // keeps the wire payload identical and avoids a future consumer
        // that string-matches `__unit__` diverging by launch method.
        if let Ok(unit_str) = serde_json::to_string(&serde_json::Value::Object(unit_obj)) {
            out.insert("__unit__".to_string(), serde_json::Value::String(unit_str));
        }
    }
    if !run_fields.is_empty() {
        // Same JSON-string form as `__unit__` — see the comment above.
        if let Ok(run_str) = serde_json::to_string(&serde_json::Value::Object(run_fields)) {
            out.insert("__run__".to_string(), serde_json::Value::String(run_str));
        }
    }
    serde_json::to_string(&serde_json::Value::Object(out)).ok()
}

/// Compose `prefix + input + suffix` into the bound string values of a
/// parsed `__bound_measurements__` payload, in place. This is the
/// identify-unit contract (`identify_unit/resolve.rs`) applied to phase
/// prompts: every surface validates and submits the operator's TYPED
/// input, and the engine composes the affixes into the recorded value —
/// so a text input with `prefix: "PCB-"` records "PCB-1234" no matter
/// which UI ran the phase, and `pattern`/`min_length`/`max_length`
/// never have to describe the affixes.
///
/// Only `text_input` components compose: their affixes render as locked
/// adornments around the typed string. Number/slider affixes are
/// display-only units ("kg", "V") whose bound values are JSON numbers,
/// and no other type renders affixes at all.
/// Blank / whitespace-only strings stay as-submitted: a deliberately
/// cleared input still records its empty string (see the builder's
/// empty-string note above) but must not record a bare prefix.
pub fn compose_bound_affixes(
    components: &[UiComponent],
    bound: &mut HashMap<String, serde_json::Value>,
) {
    // bind target → (prefix, suffix). On a duplicate bind the later
    // component wins, matching the builder's insert order (its value
    // wins there too).
    let mut affixes: HashMap<&str, (&str, &str)> = HashMap::new();
    for comp in components {
        if comp.component_type != ComponentType::TextInput {
            continue;
        }
        let prefix = comp.prefix.as_deref().unwrap_or("");
        let suffix = comp.suffix.as_deref().unwrap_or("");
        if prefix.is_empty() && suffix.is_empty() {
            continue;
        }
        if let Some(bind) = comp.bind.as_deref() {
            affixes.insert(bind, (prefix, suffix));
        }
    }
    if affixes.is_empty() {
        return;
    }

    let wrap = |bind: &str, value: &mut serde_json::Value| {
        let Some((prefix, suffix)) = affixes.get(bind) else {
            return;
        };
        let Some(s) = value.as_str() else { return };
        if s.trim().is_empty() {
            return;
        }
        *value = serde_json::Value::String(format!("{prefix}{s}{suffix}"));
    };

    for (key, value) in bound.iter_mut() {
        match key.as_str() {
            "__unit__" => rewrite_nested(value, |obj| {
                for field in [
                    "serial_number",
                    "part_number",
                    "revision_number",
                    "batch_number",
                ] {
                    if let Some(v) = obj.get_mut(field) {
                        wrap(&format!("unit.{field}"), v);
                    }
                }
                if let Some(serde_json::Value::Object(sub)) = obj.get_mut("sub_units") {
                    for (sub_key, v) in sub.iter_mut() {
                        wrap(&format!("unit.sub_units.{sub_key}"), v);
                    }
                }
            }),
            "__run__" => rewrite_nested(value, |obj| {
                if let Some(v) = obj.get_mut("operated_by") {
                    wrap("run.operated_by", v);
                }
            }),
            name => {
                // The builder accepts both bind spellings; try the
                // canonical one first so a (pathological) procedure
                // binding both never composes twice.
                let long = format!("measurements.{name}");
                if affixes.contains_key(long.as_str()) {
                    wrap(&long, value);
                } else {
                    wrap(&format!("measurement.{name}"), value);
                }
            }
        }
    }
}

/// Apply `f` to a nested bound object that ships either as a JSON
/// object or as a JSON-encoded string (`__unit__` / `__run__` — see the
/// wire-form note in `build_bound_measurements_payload`), preserving
/// whichever representation it arrived in.
fn rewrite_nested(
    value: &mut serde_json::Value,
    f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) {
    match value {
        serde_json::Value::Object(obj) => f(obj),
        serde_json::Value::String(s) => {
            if let Ok(serde_json::Value::Object(mut obj)) =
                serde_json::from_str::<serde_json::Value>(s)
            {
                f(&mut obj);
                if let Ok(encoded) = serde_json::to_string(&serde_json::Value::Object(obj)) {
                    *s = encoded;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{UiComponent, UiOption};

    fn radio(key: &str, bind: &str, image: bool) -> UiComponent {
        UiComponent {
            key: key.into(),
            bind: Some(bind.into()),
            options: Some(vec![UiOption {
                label: "A".into(),
                value: "A".into(),
                image: if image { Some("a.png".into()) } else { None },
            }]),
            ..UiComponent::new(ComponentType::Radio)
        }
    }

    #[test]
    fn packs_measurement_bind() {
        let comps = vec![radio("m", "measurements.m", false)];
        let json = build_bound_measurements_payload(&comps, |_| Some("A".into())).expect("present");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["m"], "A");
    }

    #[test]
    fn coerces_number_and_switch() {
        let num = UiComponent {
            key: "n".into(),
            bind: Some("measurements.n".into()),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let sw = UiComponent {
            key: "s".into(),
            bind: Some("measurements.s".into()),
            ..UiComponent::new(ComponentType::Switch)
        };
        let json = build_bound_measurements_payload(&[num, sw], |c| {
            Some(if c.key == "n" {
                "42".into()
            } else {
                "true".into()
            })
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["n"], 42.0);
        assert_eq!(v["s"], true);
    }

    #[test]
    fn routes_unit_and_sub_units() {
        let serial = UiComponent {
            key: "sn".into(),
            bind: Some("unit.serial_number".into()),
            ..UiComponent::new(ComponentType::TextInput)
        };
        let battery = UiComponent {
            key: "bat".into(),
            bind: Some("unit.sub_units.Battery".into()),
            ..UiComponent::new(ComponentType::TextInput)
        };
        let json = build_bound_measurements_payload(&[serial, battery], |c| {
            Some(if c.key == "sn" {
                "SN-1".into()
            } else {
                "BAT-9".into()
            })
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // `__unit__` ships as a JSON string (matches the web client).
        let unit_str = v["__unit__"].as_str().expect("__unit__ is a JSON string");
        let unit: serde_json::Value = serde_json::from_str(unit_str).unwrap();
        assert_eq!(unit["serial_number"], "SN-1");
        assert_eq!(unit["sub_units"]["Battery"], "BAT-9");
    }

    #[test]
    fn empty_name_skipped() {
        let comps = vec![radio("m", "measurements.", false)];
        assert!(build_bound_measurements_payload(&comps, |_| Some("A".into())).is_none());
    }

    #[test]
    fn no_bind_returns_none() {
        let comp = UiComponent {
            key: "x".into(),
            bind: None,
            ..UiComponent::new(ComponentType::TextInput)
        };
        assert!(build_bound_measurements_payload(&[comp], |_| Some("v".into())).is_none());
    }

    #[test]
    fn empty_number_skipped() {
        let num = UiComponent {
            key: "n".into(),
            bind: Some("measurements.n".into()),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        assert!(build_bound_measurements_payload(&[num], |_| Some(String::new())).is_none());
    }

    /// `f64::from_str` accepts `inf`, `infinity` and `NaN`, which have no
    /// JSON form: they are skipped like any unparseable answer, and a
    /// finite sibling still ships.
    #[test]
    fn non_finite_number_skipped() {
        for raw in ["inf", "-inf", "infinity", "NaN"] {
            let num = UiComponent {
                key: "n".into(),
                bind: Some("measurements.n".into()),
                ..UiComponent::new(ComponentType::Slider)
            };
            assert!(
                build_bound_measurements_payload(&[num], |_| Some(raw.into())).is_none(),
                "{raw}"
            );
        }
        let bad = UiComponent {
            key: "bad".into(),
            bind: Some("measurements.bad".into()),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let good = UiComponent {
            key: "good".into(),
            bind: Some("measurements.good".into()),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let json = build_bound_measurements_payload(&[bad, good], |c| {
            Some(if c.key == "bad" {
                "NaN".into()
            } else {
                "1.5".into()
            })
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, serde_json::json!({ "good": 1.5 }));
    }

    fn affixed_text(
        key: &str,
        bind: &str,
        prefix: Option<&str>,
        suffix: Option<&str>,
    ) -> UiComponent {
        UiComponent {
            key: key.into(),
            bind: Some(bind.into()),
            prefix: prefix.map(Into::into),
            suffix: suffix.map(Into::into),
            ..UiComponent::new(ComponentType::TextInput)
        }
    }

    fn bound_map(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn compose_wraps_bound_measurement_string() {
        let comps = vec![affixed_text(
            "pn",
            "measurements.part_number",
            Some("PCB-"),
            Some("-REV"),
        )];
        let mut bound = bound_map(&[("part_number", serde_json::json!("1234"))]);
        compose_bound_affixes(&comps, &mut bound);
        assert_eq!(bound["part_number"], "PCB-1234-REV");
    }

    #[test]
    fn compose_supports_singular_bind_spelling() {
        let comps = vec![affixed_text("fw", "measurement.fw", Some("v"), None)];
        let mut bound = bound_map(&[("fw", serde_json::json!("2.4.1"))]);
        compose_bound_affixes(&comps, &mut bound);
        assert_eq!(bound["fw"], "v2.4.1");
    }

    #[test]
    fn compose_skips_blank_and_non_string_values() {
        let comps = vec![
            affixed_text("a", "measurements.a", Some("P-"), None),
            affixed_text("b", "measurements.b", Some("P-"), None),
        ];
        // A cleared text input ships "" — must not become a bare "P-".
        let mut bound = bound_map(&[("a", serde_json::json!("")), ("b", serde_json::json!(42.0))]);
        compose_bound_affixes(&comps, &mut bound);
        assert_eq!(bound["a"], "");
        assert_eq!(bound["b"], 42.0);
    }

    #[test]
    fn compose_ignores_non_text_components() {
        // Number/slider affixes are display-only units, never composed.
        let num = UiComponent {
            key: "w".into(),
            bind: Some("measurements.w".into()),
            suffix: Some("kg".into()),
            ..UiComponent::new(ComponentType::NumberInput)
        };
        let mut bound = bound_map(&[("w", serde_json::json!("12"))]);
        compose_bound_affixes(&[num], &mut bound);
        assert_eq!(bound["w"], "12");
    }

    #[test]
    fn compose_wraps_unit_and_run_fields_in_string_form() {
        let comps = vec![
            affixed_text("sn", "unit.serial_number", Some("SN-"), None),
            affixed_text("bat", "unit.sub_units.battery", Some("BAT-"), None),
            affixed_text("op", "run.operated_by", None, Some("@acme.com")),
        ];
        // `__unit__` / `__run__` ship as JSON strings on the wire.
        let unit = serde_json::json!({
            "serial_number": "0042",
            "part_number": "PCB",
            "sub_units": { "battery": "001" }
        });
        let run = serde_json::json!({ "operated_by": "jane" });
        let mut bound = bound_map(&[
            ("__unit__", serde_json::json!(unit.to_string())),
            ("__run__", serde_json::json!(run.to_string())),
        ]);
        compose_bound_affixes(&comps, &mut bound);

        let unit_out: serde_json::Value =
            serde_json::from_str(bound["__unit__"].as_str().unwrap()).unwrap();
        assert_eq!(unit_out["serial_number"], "SN-0042");
        // No affix configured for part_number — untouched.
        assert_eq!(unit_out["part_number"], "PCB");
        assert_eq!(unit_out["sub_units"]["battery"], "BAT-001");
        let run_out: serde_json::Value =
            serde_json::from_str(bound["__run__"].as_str().unwrap()).unwrap();
        assert_eq!(run_out["operated_by"], "jane@acme.com");
    }

    #[test]
    fn compose_wraps_unit_fields_in_object_form() {
        // The engine's extractor accepts `__unit__` as a nested object
        // too — compose must handle both wire forms.
        let comps = vec![affixed_text("sn", "unit.serial_number", Some("SN-"), None)];
        let mut bound = bound_map(&[("__unit__", serde_json::json!({ "serial_number": "7" }))]);
        compose_bound_affixes(&comps, &mut bound);
        assert_eq!(bound["__unit__"]["serial_number"], "SN-7");
    }

    /// The cross-surface payload fixture, also run by the web builder
    /// (`packages/operator-ui/src/state/shared-fixtures.test.ts`).
    #[test]
    fn shared_bound_payload_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../packages/operator-ui/fixtures/operator-protocol/bound-payload.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let components: Vec<UiComponent> = serde_json::from_value(case["components"].clone())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let values: HashMap<String, String> =
                serde_json::from_value(case["values"].clone()).unwrap();
            let payload =
                build_bound_measurements_payload(&components, |c| values.get(&c.key).cloned());
            let got = payload
                .map(|json| {
                    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
                    for key in ["__unit__", "__run__"] {
                        if let Some(nested) = v.get_mut(key) {
                            let s = nested.as_str().expect("nested payload is a JSON string");
                            *nested = serde_json::from_str(s).unwrap();
                        }
                    }
                    v
                })
                .unwrap_or(serde_json::Value::Null);
            assert_eq!(got, case["expected"], "{name}");
        }
    }

    #[test]
    fn compose_no_affixes_is_a_noop() {
        let comps = vec![affixed_text("x", "measurements.x", None, None)];
        let mut bound = bound_map(&[("x", serde_json::json!("keep"))]);
        compose_bound_affixes(&comps, &mut bound);
        assert_eq!(bound["x"], "keep");
    }
}
