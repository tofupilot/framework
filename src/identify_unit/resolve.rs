//! Build a `UnitInfo` from an operator response or from configured
//! defaults.
//!
//! The two entry points are pure: no I/O, no UI. Components emit
//! string values today; if we add numeric fields later, the response
//! type will need to grow past `HashMap<String, String>`.

use std::collections::HashMap;

use crate::procedure::{UnitConfig, UnitFieldConfig};
use crate::unit::{validate_unit_info, UnitInfo};

/// Wrap a present value with the field's configured `prefix` / `suffix`.
/// The stored value is `prefix + input + suffix`, applied AFTER
/// `validate_unit_info` — `pattern` / `min_length` / `max_length`
/// describe the operator's typed input, not the composed value (so a
/// prefix never has to be repeated inside the pattern). The reuse path
/// ("Run again") replays stored values as-is and revalidates nothing.
/// Absent / blank values stay absent: an optional field left empty must
/// not materialize as a bare prefix.
fn with_affixes(cfg: Option<&UnitFieldConfig>, value: Option<String>) -> Option<String> {
    let value = value?;
    let Some(cfg) = cfg else { return Some(value) };
    let prefix = cfg.prefix.as_deref().unwrap_or("");
    let suffix = cfg.suffix.as_deref().unwrap_or("");
    if prefix.is_empty() && suffix.is_empty() {
        return Some(value);
    }
    Some(format!("{prefix}{value}{suffix}"))
}

/// Compose every field of an already-validated `UnitInfo` with its
/// configured affixes. Shared by the operator-response and
/// auto-identify paths so both store identical values.
pub fn apply_affixes(
    cfg: &UnitConfig,
    operated_by_cfg: Option<&UnitFieldConfig>,
    info: UnitInfo,
) -> UnitInfo {
    let sub_units = info.sub_units.map(|map| {
        let cfg_by_key: HashMap<String, &UnitFieldConfig> = cfg
            .sub_units
            .as_ref()
            .map(|sub| {
                sub.0
                    .iter()
                    .filter_map(|item| item.serial_number.as_ref().map(|f| (item.get_key(), f)))
                    .collect()
            })
            .unwrap_or_default();
        map.into_iter()
            .map(|(key, val)| {
                let wrapped = with_affixes(cfg_by_key.get(&key).copied(), Some(val))
                    .expect("value was present");
                (key, wrapped)
            })
            .collect()
    });
    let metadata = info.metadata.map(|map| {
        map.into_iter()
            .map(|(key, val)| {
                let field_cfg = cfg.metadata.as_ref().and_then(|md| md.get(&key));
                let wrapped = with_affixes(field_cfg, Some(val)).expect("value was present");
                (key, wrapped)
            })
            .collect()
    });
    UnitInfo {
        serial_number: with_affixes(cfg.serial_number.as_ref(), info.serial_number),
        part_number: with_affixes(cfg.part_number.as_ref(), info.part_number),
        revision_number: with_affixes(cfg.revision_number.as_ref(), info.revision_number),
        batch_number: with_affixes(cfg.batch_number.as_ref(), info.batch_number),
        operated_by: with_affixes(operated_by_cfg, info.operated_by),
        sub_units,
        status: info.status,
        metadata,
    }
}

/// Parse the operator's response and validate it against the unit
/// config. Sub-unit values are routed via the `sub_unit:<key>` prefix
/// convention emitted on the `IdentifyRequest` wire event.
///
/// Empty / whitespace-only values are dropped — operator-UI sends back
/// every field, but a blank entry is "operator left it empty," not a
/// real value. `validate_unit_info` then rejects missing required
/// fields with a clear error.
pub fn resolve_response(
    cfg: &UnitConfig,
    // Procedure-root `operated_by:` (run attribution). The response's
    // `operated_by` key is only accepted when the procedure declares it.
    operated_by_cfg: Option<&crate::procedure::UnitFieldConfig>,
    values: HashMap<String, String>,
) -> Result<UnitInfo, String> {
    let trim = |s: String| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };

    let mut serial_number = None;
    let mut part_number = None;
    let mut revision_number = None;
    let mut batch_number = None;
    let mut operated_by = None;
    let mut sub_units: HashMap<String, String> = HashMap::new();
    let mut metadata: HashMap<String, String> = HashMap::new();

    for (key, raw) in values {
        if let Some(sub_key) = key.strip_prefix("sub_unit:") {
            if let Some(val) = trim(raw) {
                sub_units.insert(sub_key.to_string(), val);
            }
        } else if let Some(md_key) = key.strip_prefix("metadata:") {
            // Only keys declared in the unit config are accepted —
            // undeclared ones are ignored like any unknown wire key.
            let declared = cfg
                .metadata
                .as_ref()
                .is_some_and(|md| md.contains_key(md_key));
            if declared {
                if let Some(val) = trim(raw) {
                    metadata.insert(md_key.to_string(), val);
                }
            }
        } else {
            match key.as_str() {
                "serial_number" => serial_number = trim(raw),
                "part_number" => part_number = trim(raw),
                "revision_number" => revision_number = trim(raw),
                "batch_number" => batch_number = trim(raw),
                "operated_by" if operated_by_cfg.is_some() => operated_by = trim(raw),
                // Unknown keys ignored to keep the wire forward-compatible.
                _ => {}
            }
        }
    }

    // Locked fields are the procedure's to set, not the operator's: the
    // configured default replaces whatever came back under the key
    // (a client that rendered the field editable, or a forged
    // response). Same value the auto path would store.
    let lock = |cfg: Option<&UnitFieldConfig>, value: Option<String>| {
        cfg.and_then(UnitFieldConfig::locked_value).or(value)
    };
    let serial_number = lock(cfg.serial_number.as_ref(), serial_number);
    let part_number = lock(cfg.part_number.as_ref(), part_number);
    let revision_number = lock(cfg.revision_number.as_ref(), revision_number);
    let batch_number = lock(cfg.batch_number.as_ref(), batch_number);
    let operated_by = lock(operated_by_cfg, operated_by);
    if let Some(sub) = cfg.sub_units.as_ref() {
        for item in &sub.0 {
            if let Some(val) = item
                .serial_number
                .as_ref()
                .and_then(UnitFieldConfig::locked_value)
            {
                sub_units.insert(item.get_key(), val);
            }
        }
    }
    if let Some(md) = cfg.metadata.as_ref() {
        for (key, field) in md {
            if let Some(val) = field.locked_value() {
                metadata.insert(key.clone(), val);
            }
        }
    }

    let unit_info = UnitInfo {
        serial_number,
        part_number,
        revision_number,
        batch_number,
        operated_by,
        sub_units: if sub_units.is_empty() {
            None
        } else {
            Some(sub_units)
        },
        status: "complete".to_string(),
        metadata: if metadata.is_empty() {
            None
        } else {
            Some(metadata)
        },
    };

    // Validate the typed input, then compose the affixes — the
    // constraints describe what the operator enters, the affixes are a
    // storage-side formatting step.
    validate_unit_info(&unit_info, &Some(cfg.clone()), operated_by_cfg)?;
    let composed = apply_affixes(cfg, operated_by_cfg, unit_info);
    crate::unit::validate_identity_wire_values(&composed)?;
    Ok(composed)
}

/// A field's `default_value` as `resolve_response` reads a typed value:
/// trimmed, and nothing when blank, so auto and manual identify store the
/// same identity.
fn default_of(field: &crate::procedure::UnitFieldConfig) -> Option<String> {
    let value = field.default_value.as_deref()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Build a `UnitInfo` from `default_value` fields when
/// `auto_identify: true` or every prompted field is `locked`. Re-runs
/// `validate_auto_identify` as a defense-
/// in-depth check; the procedure loader already enforces this at load
/// time, but a stray code path that constructs a `UnitConfig`
/// programmatically deserves the same guarantee.
pub fn auto_identify_unit_info(
    cfg: &UnitConfig,
    operated_by_cfg: Option<&crate::procedure::UnitFieldConfig>,
) -> Result<UnitInfo, String> {
    cfg.validate_auto_identify()?;
    // The all-locked path reaches here without `auto_identify`; the
    // loader already refused a locked field with no default, this is
    // the same defense-in-depth as the line above.
    cfg.validate_defaults()?;
    if let Some(operated_by) = operated_by_cfg {
        crate::procedure::schema::validate_field_default("operated_by", operated_by)?;
    }

    let sub_units = cfg.sub_units.as_ref().and_then(|sub| {
        let map: HashMap<String, String> = sub
            .0
            .iter()
            .filter_map(|item| {
                item.serial_number
                    .as_ref()
                    .and_then(default_of)
                    .map(|val| (item.get_key(), val))
            })
            .collect();
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    });

    // Metadata fields with a default_value resolve like other fields;
    // default-less fields are simply absent (metadata is never required,
    // so auto_identify imposes no new requirements).
    let metadata = cfg.metadata.as_ref().and_then(|md| {
        let map: HashMap<String, String> = md
            .iter()
            .filter_map(|(key, f)| default_of(f).map(|val| (key.clone(), val)))
            .collect();
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    });

    let unit_info = UnitInfo {
        serial_number: cfg.serial_number.as_ref().and_then(default_of),
        part_number: cfg.part_number.as_ref().and_then(default_of),
        revision_number: cfg.revision_number.as_ref().and_then(default_of),
        batch_number: cfg.batch_number.as_ref().and_then(default_of),
        operated_by: operated_by_cfg.and_then(default_of),
        sub_units,
        status: "complete".to_string(),
        metadata,
    };

    // `default_value` pre-fills the *input* on the manual identify form,
    // so it is validated as typed input and composed afterwards, exactly
    // like a manual submit — auto and manual identify store the same
    // value.
    validate_unit_info(&unit_info, &Some(cfg.clone()), operated_by_cfg)?;
    let composed = apply_affixes(cfg, operated_by_cfg, unit_info);
    crate::unit::validate_identity_wire_values(&composed)?;
    Ok(composed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::{SubUnitItemConfig, SubUnitsConfig, UnitConfig, UnitFieldConfig};

    fn cfg_minimal() -> UnitConfig {
        UnitConfig {
            auto_identify: false,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            metadata: None,
        }
    }

    fn cfg_with_optional_and_sub_units() -> UnitConfig {
        UnitConfig {
            auto_identify: false,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: Some(UnitFieldConfig::default()),
            batch_number: Some(UnitFieldConfig::default()),
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig {
                    label: "Battery".to_string(),
                    key: Some("battery".to_string()),
                    serial_number: None,
                },
                SubUnitItemConfig {
                    label: "Motor".to_string(),
                    key: Some("motor".to_string()),
                    serial_number: None,
                },
            ])),
            metadata: None,
        }
    }

    #[test]
    fn resolve_response_required_fields() {
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-1"));
        assert_eq!(info.part_number.as_deref(), Some("PCB"));
        assert!(info.revision_number.is_none());
        assert!(info.batch_number.is_none());
        assert!(info.sub_units.is_none());
    }

    #[test]
    fn resolve_response_trims_and_drops_blanks() {
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "  SN-2  ".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        values.insert("revision_number".to_string(), "   ".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-2"));
        assert!(info.revision_number.is_none());
    }

    #[test]
    fn resolve_response_routes_sub_unit_prefix() {
        let cfg = cfg_with_optional_and_sub_units();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("sub_unit:battery".to_string(), "BAT-001".to_string());
        values.insert("sub_unit:motor".to_string(), "MOT-002".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        let sub = info.sub_units.expect("sub_units populated");
        assert_eq!(sub.get("battery").map(String::as_str), Some("BAT-001"));
        assert_eq!(sub.get("motor").map(String::as_str), Some("MOT-002"));
    }

    #[test]
    fn resolve_response_routes_operated_by_when_declared() {
        let cfg = cfg_minimal();
        let operated = UnitFieldConfig::default();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("operated_by".to_string(), " jane@acme.com ".to_string());

        let info = resolve_response(&cfg, Some(&operated), values).unwrap();
        assert_eq!(info.operated_by.as_deref(), Some("jane@acme.com"));
    }

    #[test]
    fn resolve_response_ignores_operated_by_when_undeclared() {
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("operated_by".to_string(), "jane@acme.com".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        assert!(info.operated_by.is_none());
    }

    #[test]
    fn auto_identify_resolves_operated_by_default() {
        let mut cfg = cfg_minimal();
        cfg.auto_identify = true;
        cfg.serial_number = Some(UnitFieldConfig {
            default_value: Some("SN-AUTO".to_string()),
            ..Default::default()
        });
        cfg.part_number = Some(UnitFieldConfig {
            default_value: Some("PCB-AUTO".to_string()),
            ..Default::default()
        });
        let operated = UnitFieldConfig {
            default_value: Some("jane@acme.com".to_string()),
            ..Default::default()
        };

        let info = auto_identify_unit_info(&cfg, Some(&operated)).unwrap();
        assert_eq!(info.operated_by.as_deref(), Some("jane@acme.com"));
    }

    #[test]
    fn resolve_response_ignores_unknown_keys() {
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("future_field".to_string(), "ignored".to_string());

        assert!(resolve_response(&cfg, None, values).is_ok());
    }

    #[test]
    fn resolve_response_validation_error_on_missing_required() {
        let cfg = cfg_minimal();
        let values = HashMap::new();
        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(err.to_lowercase().contains("required"));
    }

    #[test]
    fn resolve_response_rejects_whitespace_only_serial() {
        // Operator submits a blank serial; we trim → drop → validation
        // rejects with the snake_case "required" message. Closes the
        // gap left by the prior `validate_unit_info` bug where display
        // labels skipped the required-field guard.
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "   ".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(
            err.contains("serial_number") && err.to_lowercase().contains("required"),
            "expected required-serial error, got: {err}"
        );
    }

    #[test]
    fn auto_identify_builds_from_defaults() {
        let cfg = UnitConfig {
            auto_identify: true,
            serial_number: Some(UnitFieldConfig {
                default_value: Some("SN-AUTO".to_string()),
                ..Default::default()
            }),
            part_number: Some(UnitFieldConfig {
                default_value: Some("PCB-AUTO".to_string()),
                ..Default::default()
            }),
            revision_number: Some(UnitFieldConfig {
                default_value: Some("A".to_string()),
                ..Default::default()
            }),
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![SubUnitItemConfig {
                label: "Battery".to_string(),
                key: Some("battery".to_string()),
                serial_number: Some(UnitFieldConfig {
                    default_value: Some("BAT-AUTO".to_string()),
                    ..Default::default()
                }),
            }])),
            metadata: None,
        };
        let info = auto_identify_unit_info(&cfg, None).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-AUTO"));
        assert_eq!(info.part_number.as_deref(), Some("PCB-AUTO"));
        assert_eq!(info.revision_number.as_deref(), Some("A"));
        let sub = info.sub_units.expect("sub_units populated");
        assert_eq!(sub.get("battery").map(String::as_str), Some("BAT-AUTO"));
    }

    /// The manual form trims what the operator types; the same default
    /// read by `auto_identify` used to keep its spaces, so the two paths
    /// stored different identities for one procedure.
    #[test]
    fn auto_identify_trims_defaults_like_a_typed_value() {
        let field = |v: &str| {
            Some(UnitFieldConfig {
                default_value: Some(v.to_string()),
                ..Default::default()
            })
        };
        let cfg = UnitConfig {
            auto_identify: true,
            serial_number: field("  SN-AUTO "),
            part_number: field("PCB-AUTO\t"),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![SubUnitItemConfig {
                label: "Battery".to_string(),
                key: Some("battery".to_string()),
                serial_number: field(" BAT-AUTO"),
            }])),
            metadata: None,
        };
        let operated = UnitFieldConfig {
            default_value: Some(" alice ".to_string()),
            ..Default::default()
        };
        let info = auto_identify_unit_info(&cfg, Some(&operated)).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-AUTO"));
        assert_eq!(info.part_number.as_deref(), Some("PCB-AUTO"));
        assert_eq!(info.operated_by.as_deref(), Some("alice"));
        let manual = resolve_response(
            &cfg,
            Some(&operated),
            HashMap::from([
                ("serial_number".to_string(), "  SN-AUTO ".to_string()),
                ("part_number".to_string(), "PCB-AUTO\t".to_string()),
                ("sub_unit:battery".to_string(), " BAT-AUTO".to_string()),
                ("operated_by".to_string(), " alice ".to_string()),
            ]),
        )
        .unwrap();
        assert_eq!(info.serial_number, manual.serial_number);
        assert_eq!(info.sub_units, manual.sub_units);
    }

    #[test]
    fn auto_identify_rejects_missing_default() {
        let cfg = UnitConfig {
            auto_identify: true,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig {
                default_value: Some("PCB-AUTO".to_string()),
                ..Default::default()
            }),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            metadata: None,
        };
        let err = auto_identify_unit_info(&cfg, None).unwrap_err();
        assert!(err.contains("serial_number.default_value"));
    }

    #[test]
    fn auto_identify_rejects_sub_unit_missing_default() {
        let cfg = UnitConfig {
            auto_identify: true,
            serial_number: Some(UnitFieldConfig {
                default_value: Some("SN".to_string()),
                ..Default::default()
            }),
            part_number: Some(UnitFieldConfig {
                default_value: Some("PCB".to_string()),
                ..Default::default()
            }),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig {
                    label: "Battery".to_string(),
                    key: Some("battery".to_string()),
                    serial_number: Some(UnitFieldConfig {
                        default_value: Some("BAT-001".to_string()),
                        ..Default::default()
                    }),
                },
                SubUnitItemConfig {
                    label: "Motor".to_string(),
                    key: Some("motor".to_string()),
                    // No default — should be rejected upfront with a
                    // message naming "motor", not the opaque
                    // "expected 2 sub-units got 1" from
                    // validate_unit_info.
                    serial_number: None,
                },
            ])),
            metadata: None,
        };
        let err = auto_identify_unit_info(&cfg, None).unwrap_err();
        assert!(
            err.contains("sub_units.motor.serial_number.default_value"),
            "expected motor-named error, got: {err}"
        );
    }

    fn cfg_with_metadata() -> UnitConfig {
        let mut cfg = cfg_minimal();
        let mut md = std::collections::BTreeMap::new();
        md.insert(
            "modification".to_string(),
            UnitFieldConfig {
                pattern: Some("^MOD-[0-9]+$".to_string()),
                pattern_message: None,
                ..Default::default()
            },
        );
        md.insert("amendment".to_string(), UnitFieldConfig::default());
        cfg.metadata = Some(md);
        cfg
    }

    #[test]
    fn resolve_response_routes_metadata_prefix() {
        let cfg = cfg_with_metadata();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        values.insert("metadata:modification".to_string(), " MOD-42 ".to_string());
        values.insert("metadata:amendment".to_string(), "".to_string()); // blank → absent
        values.insert("metadata:undeclared".to_string(), "x".to_string()); // ignored

        let info = resolve_response(&cfg, None, values).unwrap();
        let md = info.metadata.expect("metadata populated");
        assert_eq!(md.get("modification").map(String::as_str), Some("MOD-42"));
        assert!(!md.contains_key("amendment"));
        assert!(!md.contains_key("undeclared"));
    }

    #[test]
    fn resolve_response_metadata_pattern_violation_errors() {
        let cfg = cfg_with_metadata();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        values.insert("metadata:modification".to_string(), "BAD-1".to_string());

        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(err.contains("modification"), "got: {err}");
    }

    #[test]
    fn resolve_response_all_metadata_blank_is_none() {
        let cfg = cfg_with_metadata();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        values.insert("metadata:modification".to_string(), "  ".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        assert!(info.metadata.is_none());
    }

    /// The server ceiling applies to what is stored: prefix + typed + suffix.
    #[test]
    fn resolve_response_refuses_a_composed_value_over_the_server_ceiling() {
        let mut cfg = cfg_minimal();
        cfg.serial_number = Some(UnitFieldConfig {
            prefix: Some("SN-".to_string()),
            ..Default::default()
        });
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "x".repeat(58));
        values.insert("part_number".to_string(), "PCB".to_string());
        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(
            err.contains("serial_number") && err.contains("at most 60"),
            "{err}"
        );

        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "x".repeat(57));
        values.insert("part_number".to_string(), "PCB".to_string());
        assert!(
            resolve_response(&cfg, None, values).is_ok(),
            "SN- plus 57 is exactly 60"
        );
    }

    /// A value under a field the config does not declare still meets the
    /// server rules: `revision_number` without a `revision_number:` block.
    #[test]
    fn resolve_response_gates_identity_fields_the_config_omits() {
        let cfg = cfg_minimal();
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "PCB 7".to_string());
        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(err.contains("part_number"), "{err}");
    }

    #[test]
    fn resolve_response_wraps_value_with_affixes() {
        let mut cfg = cfg_minimal();
        cfg.serial_number = Some(UnitFieldConfig {
            prefix: Some("SN-".to_string()),
            suffix: Some("-EU".to_string()),
            // Constraints describe the operator's typed input; the
            // affixes are composed after validation.
            pattern: Some("^[0-9]+$".to_string()),
            pattern_message: None,
            min_length: Some(4),
            ..Default::default()
        });
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), " 0042 ".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-0042-EU"));
    }

    #[test]
    fn resolve_response_validates_typed_input_not_composed_value() {
        let mut cfg = cfg_minimal();
        cfg.serial_number = Some(UnitFieldConfig {
            prefix: Some("SN-".to_string()),
            pattern: Some("^[0-9]{4}$".to_string()),
            pattern_message: None,
            ..Default::default()
        });
        // Too short for the typed-input pattern — rejected even though
        // a composed-value reading ("SN-42" vs the pattern) would give
        // a different failure shape.
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "42".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        let err = resolve_response(&cfg, None, values).unwrap_err();
        assert!(err.contains("serial_number"), "got: {err}");

        // The full typed pattern passes without mentioning the prefix.
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "0042".to_string());
        values.insert("part_number".to_string(), "PCB".to_string());
        let info = resolve_response(&cfg, None, values).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-0042"));
    }

    #[test]
    fn resolve_response_blank_optional_field_is_not_wrapped() {
        let mut cfg = cfg_minimal();
        cfg.revision_number = Some(UnitFieldConfig {
            prefix: Some("REV-".to_string()),
            ..Default::default()
        });
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("revision_number".to_string(), "  ".to_string());

        let info = resolve_response(&cfg, None, values).unwrap();
        // No bare "REV-" from an empty input.
        assert!(info.revision_number.is_none());
    }

    #[test]
    fn resolve_response_wraps_sub_unit_and_metadata_and_operated_by() {
        let mut cfg = cfg_minimal();
        cfg.sub_units = Some(SubUnitsConfig(vec![SubUnitItemConfig {
            label: "Battery".to_string(),
            key: Some("battery".to_string()),
            serial_number: Some(UnitFieldConfig {
                prefix: Some("BAT-".to_string()),
                ..Default::default()
            }),
        }]));
        let mut md = std::collections::BTreeMap::new();
        md.insert(
            "modification".to_string(),
            UnitFieldConfig {
                prefix: Some("MOD-".to_string()),
                ..Default::default()
            },
        );
        cfg.metadata = Some(md);
        let operated = UnitFieldConfig {
            suffix: Some("@acme.com".to_string()),
            ..Default::default()
        };

        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN".to_string());
        values.insert("part_number".to_string(), "PN".to_string());
        values.insert("sub_unit:battery".to_string(), "001".to_string());
        values.insert("metadata:modification".to_string(), "42".to_string());
        values.insert("operated_by".to_string(), "jane".to_string());

        let info = resolve_response(&cfg, Some(&operated), values).unwrap();
        assert_eq!(
            info.sub_units.unwrap().get("battery").map(String::as_str),
            Some("BAT-001")
        );
        assert_eq!(
            info.metadata
                .unwrap()
                .get("modification")
                .map(String::as_str),
            Some("MOD-42")
        );
        assert_eq!(info.operated_by.as_deref(), Some("jane@acme.com"));
    }

    #[test]
    fn auto_identify_wraps_defaults_with_affixes() {
        let cfg = UnitConfig {
            auto_identify: true,
            serial_number: Some(UnitFieldConfig {
                default_value: Some("0042".to_string()),
                prefix: Some("SN-".to_string()),
                ..Default::default()
            }),
            part_number: Some(UnitFieldConfig {
                default_value: Some("PCB".to_string()),
                suffix: Some("-V2".to_string()),
                ..Default::default()
            }),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            metadata: None,
        };
        let info = auto_identify_unit_info(&cfg, None).unwrap();
        // Same stored value as a manual submit of the pre-filled default.
        assert_eq!(info.serial_number.as_deref(), Some("SN-0042"));
        assert_eq!(info.part_number.as_deref(), Some("PCB-V2"));
    }

    #[test]
    fn auto_identify_resolves_metadata_defaults() {
        let mut cfg = UnitConfig {
            auto_identify: true,
            serial_number: Some(UnitFieldConfig {
                default_value: Some("SN-AUTO".to_string()),
                ..Default::default()
            }),
            part_number: Some(UnitFieldConfig {
                default_value: Some("PCB-AUTO".to_string()),
                ..Default::default()
            }),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            metadata: None,
        };
        let mut md = std::collections::BTreeMap::new();
        md.insert(
            "modification".to_string(),
            UnitFieldConfig {
                default_value: Some("MOD-42".to_string()),
                ..Default::default()
            },
        );
        // No default — absent, not an error (metadata never required)
        md.insert("amendment".to_string(), UnitFieldConfig::default());
        cfg.metadata = Some(md);

        let info = auto_identify_unit_info(&cfg, None).unwrap();
        let resolved = info.metadata.expect("metadata from defaults");
        assert_eq!(
            resolved.get("modification").map(String::as_str),
            Some("MOD-42")
        );
        assert!(!resolved.contains_key("amendment"));
    }

    #[test]
    fn locked_fields_take_the_configured_value_over_the_response() {
        let mut cfg = cfg_minimal();
        cfg.part_number = Some(UnitFieldConfig {
            default_value: Some("PCB-MAIN-V2".to_string()),
            locked: true,
            ..Default::default()
        });
        cfg.sub_units = Some(SubUnitsConfig(vec![SubUnitItemConfig {
            label: "Radio".to_string(),
            key: Some("radio".to_string()),
            serial_number: Some(UnitFieldConfig {
                default_value: Some("RF-STD".to_string()),
                locked: true,
                ..Default::default()
            }),
        }]));
        let mut md = std::collections::BTreeMap::new();
        md.insert(
            "line".to_string(),
            UnitFieldConfig {
                default_value: Some("L2".to_string()),
                locked: true,
                ..Default::default()
            },
        );
        cfg.metadata = Some(md);
        let operated_by = UnitFieldConfig {
            default_value: Some("station@acme.com".to_string()),
            locked: true,
            ..Default::default()
        };

        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        values.insert("part_number".to_string(), "TAMPERED".to_string());
        values.insert("sub_unit:radio".to_string(), "TAMPERED".to_string());
        values.insert("metadata:line".to_string(), "TAMPERED".to_string());
        values.insert("operated_by".to_string(), "TAMPERED".to_string());
        let info = resolve_response(&cfg, Some(&operated_by), values).unwrap();
        assert_eq!(info.serial_number.as_deref(), Some("SN-1"));
        assert_eq!(info.part_number.as_deref(), Some("PCB-MAIN-V2"));
        assert_eq!(info.sub_units.unwrap()["radio"], "RF-STD");
        assert_eq!(info.metadata.unwrap()["line"], "L2");
        assert_eq!(info.operated_by.as_deref(), Some("station@acme.com"));
    }

    /// A client that omits a locked key altogether (the form does not
    /// submit disabled inputs) still resolves — the lock is the source.
    #[test]
    fn locked_field_missing_from_the_response_resolves_from_config() {
        let mut cfg = cfg_minimal();
        cfg.part_number = Some(UnitFieldConfig {
            default_value: Some("PCB-MAIN-V2".to_string()),
            prefix: Some("PN-".to_string()),
            locked: true,
            ..Default::default()
        });
        let mut values = HashMap::new();
        values.insert("serial_number".to_string(), "SN-1".to_string());
        let info = resolve_response(&cfg, None, values).unwrap();
        // Affixes compose on the locked default like on any value.
        assert_eq!(info.part_number.as_deref(), Some("PN-PCB-MAIN-V2"));
    }
}
