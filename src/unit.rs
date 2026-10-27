//! Execution-related types for unit information and validation.

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UnitInfo {
    pub serial_number: Option<String>,
    pub part_number: Option<String>,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    /// Operator attributed to the run (member email or free-text
    /// name) — a RUN property, not a
    /// unit field. Like `metadata` below, it is engine-side carriage
    /// only (identify form → Python `run.operated_by` → job results),
    /// deliberately NOT on the station-protocol wire `UnitInfo`; the
    /// CLI reads it here and forwards it to `runs.create` as
    /// `operated_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operated_by: Option<String>,
    /// Sub-units as label -> serial_number mapping
    pub sub_units: Option<HashMap<String, String>>,
    pub status: String,
    /// Operator-entered unit metadata from the identify form
    /// (`unit.metadata.<key>` fields in the procedure YAML). Engine-side
    /// only — deliberately not on the station-protocol wire `UnitInfo`;
    /// the CLI merges it into the upload's `unit_metadata`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
}

/// Server ceiling on `operated_by` (`runs.create` rejects longer, and the
/// `run_operated_by_name_len` CHECK caps the column at the same width).
/// Enforced here independently of the procedure's own `max_length` so a
/// procedure that declares a larger bound — or none at all, the common case —
/// still cannot produce a value the upload will refuse after the test has run.
///
/// Counted in CHARACTERS, unlike the `min_length` / `max_length` checks below
/// which have always compared bytes: this bound mirrors a server limit, and
/// counting bytes here would falsely reject a short accented operator name.
pub const OPERATED_BY_MAX_CHARS: usize = 254;

/// Server ceiling on the unit identity fields (serial, part, revision,
/// batch, sub-unit serials): `runs.create` rejects longer values and
/// anything outside `UNIT_FIELD_CHARSET_PATTERN`. Enforced here
/// independently of the procedure's own `max_length` / `pattern`, for
/// the same reason as `OPERATED_BY_MAX_CHARS`: a value the upload will
/// refuse must be refused before the test runs. Counted in characters.
pub const UNIT_FIELD_MAX_CHARS: usize = 60;

/// The server's own rules on an identity field, applied whatever the
/// procedure declares: at most `UNIT_FIELD_MAX_CHARS` characters, all of
/// them in `UNIT_FIELD_CHARSET_PATTERN`. `runs.create` refuses anything
/// else, so the station must too, before the test runs.
pub(crate) fn validate_identity_wire_limits(field_name: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    let chars = trimmed.chars().count();
    if chars > UNIT_FIELD_MAX_CHARS {
        return Err(format!(
            "{field_name}: expected at most {UNIT_FIELD_MAX_CHARS} characters, got {chars}"
        ));
    }
    let pattern = station_protocol::pattern_messages::UNIT_FIELD_CHARSET_PATTERN;
    if !crate::procedure::schema::is_identity_charset(trimmed) {
        return Err(
            match crate::pattern_messages::derive_pattern_error(pattern, trimmed) {
                Some(derived) => format!("{field_name}: {derived}"),
                None => format!("{field_name}: expected A-Z a-z 0-9 _ . : + -"),
            },
        );
    }
    Ok(())
}

/// The server's rules on the values that will be uploaded: every unit
/// identity field (serial, part, revision, batch, sub-unit serials) of an
/// already-composed `UnitInfo`, prefix and suffix included, declared in
/// the procedure or not. Run after `apply_affixes` on every identify
/// path. Metadata and `operated_by` are free text and stay out.
pub fn validate_identity_wire_values(info: &UnitInfo) -> Result<(), String> {
    for (field, value) in [
        ("serial_number", &info.serial_number),
        ("part_number", &info.part_number),
        ("revision_number", &info.revision_number),
        ("batch_number", &info.batch_number),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            validate_identity_wire_limits(field, value)?;
        }
    }
    if let Some(sub_units) = &info.sub_units {
        for (label, serial) in sub_units {
            validate_identity_wire_limits(&format!("{label} serial number"), serial)?;
        }
    }
    Ok(())
}

/// Validate a single unit field against its configuration
pub(crate) fn validate_unit_field(
    field_name: &str,
    value: &Option<String>,
    config: &crate::procedure::UnitFieldConfig,
) -> Result<(), String> {
    // Serial number and part number are always required
    let is_required = field_name == "serial_number" || field_name == "part_number";

    if is_required {
        let val = value
            .as_ref()
            .ok_or_else(|| format!("{}: required", field_name))?;

        if val.trim().is_empty() {
            return Err(format!("{}: empty", field_name));
        }
    }

    if let Some(val) = value {
        let trimmed = val.trim();

        // Ensure at least 1 character after trim if value is provided
        if trimmed.is_empty() {
            return Err(format!("{}: empty", field_name));
        }

        // Check min_length on trimmed value
        // Lengths count characters, not bytes, to agree with the wire
        // gate in station-protocol and the message wording.
        let chars = trimmed.chars().count();
        if let Some(min) = config.min_length {
            if chars < min {
                return Err(format!(
                    "{}: expected at least {} characters, got {}",
                    field_name, min, chars
                ));
            }
        }

        // Check max_length on trimmed value
        if let Some(max) = config.max_length {
            if chars > max {
                return Err(format!(
                    "{}: expected at most {} characters, got {}",
                    field_name, max, chars
                ));
            }
        }

        // Check pattern on trimmed value. Never quote the raw regex to
        // the operator: the authored pattern_message when there is one,
        // a derived character-level message when the pattern explains
        // itself, the bare verdict otherwise. This error ships on the
        // wire, so the kiosk and the TUI recite the same words.
        if let Some(pattern) = &config.pattern {
            let regex = crate::pattern_messages::compile_field_pattern(pattern)
                .map_err(|e| format!("{}.pattern: invalid regex: {}", field_name, e))?;

            if !regex.is_match(trimmed) {
                return Err(match &config.pattern_message {
                    Some(msg) => format!("{}: {}", field_name, msg),
                    None => match crate::pattern_messages::derive_pattern_error(pattern, trimmed) {
                        Some(derived) => format!("{}: {}", field_name, derived),
                        None => format!("{}: does not match the required format", field_name),
                    },
                });
            }
        }
    }

    Ok(())
}

/// Validate a single sub-unit serial number against its constraints
fn validate_sub_unit_field(
    label: &str,
    value: &str,
    config: &Option<crate::procedure::UnitFieldConfig>,
) -> Result<(), String> {
    let trimmed = value.trim();

    // Sub-units are always required
    if trimmed.is_empty() {
        return Err(format!("{} serial number: required", label));
    }

    // Apply constraints if config exists
    if let Some(field_config) = config {
        let chars = trimmed.chars().count();
        if let Some(min) = field_config.min_length {
            if chars < min {
                return Err(format!(
                    "{}: expected at least {} characters, got {}",
                    label, min, chars
                ));
            }
        }

        if let Some(max) = field_config.max_length {
            if chars > max {
                return Err(format!(
                    "{}: expected at most {} characters, got {}",
                    label, max, chars
                ));
            }
        }

        // Check pattern — same message policy as validate_unit_field:
        // authored pattern_message, then derived, never the raw regex.
        if let Some(pattern) = &field_config.pattern {
            let regex = crate::pattern_messages::compile_field_pattern(pattern)
                .map_err(|e| format!("{} pattern: invalid regex: {}", label, e))?;

            if !regex.is_match(trimmed) {
                return Err(match &field_config.pattern_message {
                    Some(msg) => format!("{}: {}", label, msg),
                    None => match crate::pattern_messages::derive_pattern_error(pattern, trimmed) {
                        Some(derived) => format!("{}: {}", label, derived),
                        None => format!("{}: does not match the required format", label),
                    },
                });
            }
        }
    }

    Ok(())
}

/// Validate sub-units against configuration
fn validate_sub_units(
    sub_units: &Option<HashMap<String, String>>,
    sub_units_config: &crate::procedure::SubUnitsConfig,
) -> Result<(), String> {
    let expected_count = sub_units_config.0.len();

    // Check that we have sub-units
    let sub_units_map = sub_units
        .as_ref()
        .ok_or_else(|| format!("Sub-units: expected {}, got 0", expected_count))?;

    // Check count matches
    if sub_units_map.len() != expected_count {
        return Err(format!(
            "Sub-units: expected {}, got {}",
            expected_count,
            sub_units_map.len()
        ));
    }

    // Validate each configured sub-unit (using key for lookup, label for error messages)
    for item in &sub_units_config.0 {
        let key = item.get_key();
        let serial = sub_units_map
            .get(&key)
            .ok_or_else(|| format!("Sub-unit {} ({}): missing", item.label, key))?;

        validate_sub_unit_field(&item.label, serial, &item.serial_number)?;
    }

    Ok(())
}

/// Validate all unit fields against configuration
pub fn validate_unit_info(
    unit_info: &UnitInfo,
    unit_config: &Option<crate::procedure::UnitConfig>,
    // Procedure-root `operated_by:` (run attribution) — validated here
    // because its value rides the same identify response.
    operated_by_config: Option<&crate::procedure::UnitFieldConfig>,
) -> Result<(), String> {
    if let Some(operated_config) = operated_by_config {
        validate_unit_field("operated_by", &unit_info.operated_by, operated_config)?;
    }
    // Independent of the config above: the ceiling holds even when the
    // procedure declares no `operated_by:` block at all.
    if let Some(value) = unit_info.operated_by.as_deref() {
        let chars = value.trim().chars().count();
        if chars > OPERATED_BY_MAX_CHARS {
            return Err(format!(
                "operated_by must be at most {OPERATED_BY_MAX_CHARS} characters (got {chars})"
            ));
        }
    }

    let config = match unit_config {
        Some(c) => c,
        None => return Ok(()), // No config = no validation
    };

    // Validate built-in fields. Pass the snake_case key so the
    // "required" guard in `validate_unit_field` (which checks against
    // "serial_number" / "part_number") fires for missing required
    // values. Earlier callers passed display labels, which silently
    // skipped the required-field branch and let runs upload with an
    // empty serial / part.
    if let Some(sn_config) = &config.serial_number {
        validate_unit_field("serial_number", &unit_info.serial_number, sn_config)?;
    }

    if let Some(pn_config) = &config.part_number {
        validate_unit_field("part_number", &unit_info.part_number, pn_config)?;
    }

    if let Some(rev_config) = &config.revision_number {
        validate_unit_field("revision_number", &unit_info.revision_number, rev_config)?;
    }

    if let Some(batch_config) = &config.batch_number {
        validate_unit_field("batch_number", &unit_info.batch_number, batch_config)?;
    }

    // Validate sub-units if configured
    if let Some(sub_units_config) = &config.sub_units {
        validate_sub_units(&unit_info.sub_units, sub_units_config)?;
    }

    // Validate operator-entered metadata values against their field
    // configs. Metadata is never required — only present values are
    // checked (min/max length, pattern).
    if let Some(md_config) = &config.metadata {
        if let Some(md_values) = &unit_info.metadata {
            for (key, field_config) in md_config {
                if let Some(value) = md_values.get(key) {
                    validate_unit_field(key, &Some(value.clone()), field_config)?;
                }
            }
        }
    }

    Ok(())
}

pub struct PendingUnitInput {
    pub sender: tokio::sync::oneshot::Sender<UnitInfo>,
    pub unit_config: Option<crate::procedure::UnitConfig>,
}

#[cfg(test)]
mod tests {
    /// TP-1086: the identify-unit field this task was filed against.
    /// A pattern locks the format of the whole serial — before this it
    /// was a substring search, so a scan carrying extra label text
    /// around a valid serial was accepted and stored.
    #[test]
    fn unit_field_pattern_must_match_the_whole_serial() {
        use super::*;
        let field = crate::procedure::UnitFieldConfig {
            pattern: Some("SN-[0-9]+".to_string()),
            ..Default::default()
        };
        let entry = |s: &str| Some(s.to_string());
        assert!(validate_unit_field("Serial number", &entry("SN-0042"), &field).is_ok());
        let err =
            validate_unit_field("Serial number", &entry("SCRAP SN-0042 XX"), &field).unwrap_err();
        assert!(err.starts_with("Serial number"), "{err}");
    }

    #[test]
    fn unit_field_lengths_count_characters_not_bytes() {
        // The wire gate in station-protocol counts characters. If this
        // backstop counted bytes, a value the gate accepted would still
        // kill the run with `identify_unit_failed`.
        use super::*;
        let field = crate::procedure::UnitFieldConfig {
            min_length: Some(10),
            max_length: Some(10),
            ..Default::default()
        };
        let entry = |s: &str| Some(s.to_string());
        // 10 characters, 12 bytes.
        assert!(validate_unit_field("Operator", &entry("Zoé Müller"), &field).is_ok());
        let err = validate_unit_field("Operator", &entry("Zoé Müllers"), &field).unwrap_err();
        assert!(err.contains("got 11"), "{err}");
        assert!(validate_sub_unit_field("Board", "Zoé Müller", &Some(field.clone())).is_ok());
    }

    fn minimal_unit(serial: &str, part: &str) -> UnitInfo {
        UnitInfo {
            serial_number: Some(serial.to_string()),
            part_number: Some(part.to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: None,
            sub_units: None,
            status: "tested".to_string(),
            metadata: None,
        }
    }

    /// `runs.create` caps every identity field at 60 characters and the
    /// unit charset; the station must refuse the same values before the
    /// test runs, whatever the procedure declares.
    #[test]
    fn identity_fields_over_the_server_ceiling_are_rejected() {
        let long = "x".repeat(UNIT_FIELD_MAX_CHARS + 1);
        let err = validate_identity_wire_values(&minimal_unit(&long, "PN-1")).unwrap_err();
        assert!(
            err.contains("serial_number") && err.contains("at most"),
            "got: {err}"
        );
        let err = validate_identity_wire_values(&minimal_unit("SN-1", &long)).unwrap_err();
        assert!(
            err.contains("part_number") && err.contains("at most"),
            "got: {err}"
        );
        let ok = "x".repeat(UNIT_FIELD_MAX_CHARS);
        assert!(validate_identity_wire_values(&minimal_unit(&ok, "PN-1")).is_ok());
    }

    #[test]
    fn identity_fields_outside_the_unit_charset_are_rejected() {
        for bad in ["SN 123", "SN/123", "SN#1", "SN-é"] {
            let err = validate_identity_wire_values(&minimal_unit(bad, "PN-1")).unwrap_err();
            assert!(err.contains("serial_number"), "{bad}: {err}");
        }
        assert!(validate_identity_wire_values(&minimal_unit("SN_1.2:3+4-5", "PN-1")).is_ok());
        let mut with_batch = minimal_unit("SN-1", "PN-1");
        with_batch.batch_number = Some("lot 7".to_string());
        assert!(validate_identity_wire_values(&with_batch).is_err());
    }

    #[test]
    fn operated_by_over_the_ceiling_is_rejected_without_any_config() {
        // The common case is a procedure that declares no `operated_by:` block
        // at all. The ceiling still has to hold, otherwise the value only
        // fails later at `runs.create`, once the test has run.
        let mut unit_info = UnitInfo {
            serial_number: Some("SN-1".to_string()),
            part_number: Some("PN-1".to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: Some("x".repeat(OPERATED_BY_MAX_CHARS + 1)),
            sub_units: None,
            status: "tested".to_string(),
            metadata: None,
        };
        let err = validate_unit_info(&unit_info, &None, None).unwrap_err();
        assert!(err.contains("operated_by"), "got: {err}");
        assert!(err.contains("at most"), "got: {err}");

        unit_info.operated_by = Some("x".repeat(OPERATED_BY_MAX_CHARS));
        assert!(validate_unit_info(&unit_info, &None, None).is_ok());
    }

    #[test]
    fn operated_by_ceiling_counts_characters_not_bytes() {
        // 200 accented characters are 400 bytes. A byte-based ceiling would
        // reject a perfectly valid operator name the server accepts.
        let unit_info = UnitInfo {
            serial_number: Some("SN-1".to_string()),
            part_number: Some("PN-1".to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: Some("é".repeat(200)),
            sub_units: None,
            status: "tested".to_string(),
            metadata: None,
        };
        assert!(validate_unit_info(&unit_info, &None, None).is_ok());
    }

    use super::*;
    use crate::procedure::{SubUnitItemConfig, SubUnitsConfig, UnitFieldConfig};

    // ========================================================================
    // validate_sub_unit_field tests
    // ========================================================================

    #[test]
    fn test_validate_sub_unit_field_success() {
        let result = validate_sub_unit_field("Battery", "BAT-001", &None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_unit_field_empty_fails() {
        let result = validate_sub_unit_field("Battery", "", &None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Battery serial number: required"));
    }

    #[test]
    fn test_validate_sub_unit_field_whitespace_only_fails() {
        let result = validate_sub_unit_field("Motor", "   ", &None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Motor serial number: required"));
    }

    #[test]
    fn test_validate_sub_unit_field_min_length_pass() {
        let config = Some(UnitFieldConfig {
            min_length: Some(5),
            max_length: None,
            pattern: None,
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "BAT-001", &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_unit_field_min_length_fail() {
        let config = Some(UnitFieldConfig {
            min_length: Some(10),
            max_length: None,
            pattern: None,
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "BAT", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least 10 characters"));
    }

    #[test]
    fn test_validate_sub_unit_field_max_length_pass() {
        let config = Some(UnitFieldConfig {
            min_length: None,
            max_length: Some(20),
            pattern: None,
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "BAT-001", &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_unit_field_max_length_fail() {
        let config = Some(UnitFieldConfig {
            min_length: None,
            max_length: Some(5),
            pattern: None,
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "BAT-001-LONG", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at most 5 characters"));
    }

    #[test]
    fn test_validate_sub_unit_field_pattern_pass() {
        let config = Some(UnitFieldConfig {
            min_length: None,
            max_length: None,
            pattern: Some(r"^BAT-\d{3}$".to_string()),
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "BAT-001", &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_unit_field_pattern_fail() {
        let config = Some(UnitFieldConfig {
            min_length: None,
            max_length: None,
            pattern: Some(r"^BAT-\d{3}$".to_string()),
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "INVALID", &config);
        assert!(result.is_err());
        // A sequence pattern derives its character-level message —
        // never the raw regex (TP-760).
        let err = result.unwrap_err();
        assert!(err.contains("Should start with “BAT-”"), "got: {err}");
        assert!(!err.contains(r"^BAT-\d{3}$"));

        // A branching pattern can't derive: the bare verdict.
        let branching = Some(UnitFieldConfig {
            pattern: Some(r"^(BAT|MOT)-\d{3}$".to_string()),
            ..Default::default()
        });
        let err = validate_sub_unit_field("Battery", "INVALID", &branching).unwrap_err();
        assert!(
            err.contains(": does not match the required format"),
            "got: {err}"
        );
        assert!(!err.contains(r"^(BAT|MOT)-\d{3}$"));
    }

    #[test]
    fn test_validate_sub_unit_field_pattern_fail_uses_pattern_message() {
        let config = Some(UnitFieldConfig {
            min_length: None,
            max_length: None,
            pattern: Some(r"^BAT-\d{3}$".to_string()),
            pattern_message: Some("Scan the label under the cell, like BAT-042".to_string()),
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let err = validate_sub_unit_field("Battery", "INVALID", &config).unwrap_err();
        assert!(err.contains("Scan the label under the cell, like BAT-042"));
        assert!(!err.contains(r"^BAT-\d{3}$"));
    }

    #[test]
    fn test_validate_sub_unit_field_trims_whitespace() {
        let config = Some(UnitFieldConfig {
            min_length: Some(3),
            max_length: Some(10),
            pattern: None,
            pattern_message: None,
            default_value: None,
            placeholder: None,
            description: None,
            prefix: None,
            suffix: None,
            locked: false,
        });
        let result = validate_sub_unit_field("Battery", "  BAT-001  ", &config);
        assert!(result.is_ok());
    }

    // ========================================================================
    // validate_sub_units tests
    // ========================================================================

    fn create_config_with_items(items: Vec<SubUnitItemConfig>) -> SubUnitsConfig {
        SubUnitsConfig(items)
    }

    fn create_sub_units_map(pairs: Vec<(&str, &str)>) -> Option<HashMap<String, String>> {
        Some(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn test_validate_sub_units_success() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Controller".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
            ("controller", "CTL-003"),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_units_none_when_expected() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let result = validate_sub_units(&None, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Sub-units: expected 2, got 0"));
    }

    #[test]
    fn test_validate_sub_units_wrong_count_too_few() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Controller".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let sub_units = create_sub_units_map(vec![("battery", "BAT-001"), ("motor", "MOT-002")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Sub-units: expected 3, got 2"));
    }

    #[test]
    fn test_validate_sub_units_wrong_count_too_many() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
            ("controller", "CTL-003"),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Sub-units: expected 2, got 3"));
    }

    #[test]
    fn test_validate_sub_units_empty_serial_fails() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let sub_units = create_sub_units_map(vec![("battery", "BAT-001"), ("motor", "")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Motor serial number: required"));
    }

    #[test]
    fn test_validate_sub_units_missing_label() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: None,
            },
        ]);
        let sub_units =
            create_sub_units_map(vec![("battery", "BAT-001"), ("wronglabel", "MOT-002")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Sub-unit Motor (motor): missing"));
    }

    #[test]
    fn test_validate_sub_units_pattern_per_item() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: Some(UnitFieldConfig {
                    min_length: None,
                    max_length: None,
                    pattern: Some(r"^BAT-\d+$".to_string()),
                    pattern_message: None,
                    default_value: None,
                    placeholder: None,
                    description: None,
                    prefix: None,
                    suffix: None,
                    locked: false,
                }),
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: Some(UnitFieldConfig {
                    min_length: None,
                    max_length: None,
                    pattern: Some(r"^MOT-\d+$".to_string()),
                    pattern_message: None,
                    default_value: None,
                    placeholder: None,
                    description: None,
                    prefix: None,
                    suffix: None,
                    locked: false,
                }),
            },
        ]);
        let sub_units = create_sub_units_map(vec![("battery", "BAT-001"), ("motor", "MOT-002")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_units_pattern_fail_shows_label() {
        let config = create_config_with_items(vec![SubUnitItemConfig {
            label: "Battery Pack".to_string(),
            key: None,
            serial_number: Some(UnitFieldConfig {
                min_length: None,
                max_length: None,
                pattern: Some(r"^BAT-\d+$".to_string()),
                pattern_message: None,
                default_value: None,
                placeholder: None,
                description: None,
                prefix: None,
                suffix: None,
                locked: false,
            }),
        }]);
        let sub_units = create_sub_units_map(vec![("battery_pack", "INVALID")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Battery Pack"));
        // Derived from the sequence pattern — never the raw regex.
        assert!(err.contains("Should start with “BAT-”"), "got: {err}");
        assert!(!err.contains(r"^BAT-\d+$"));
    }

    #[test]
    fn test_validate_sub_units_min_length_per_item() {
        let config = create_config_with_items(vec![SubUnitItemConfig {
            label: "Battery".to_string(),
            key: None,
            serial_number: Some(UnitFieldConfig {
                min_length: Some(10),
                max_length: None,
                pattern: None,
                pattern_message: None,
                default_value: None,
                placeholder: None,
                description: None,
                prefix: None,
                suffix: None,
                locked: false,
            }),
        }]);
        let sub_units = create_sub_units_map(vec![("battery", "BAT")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least 10 characters"));
    }

    // ========================================================================
    // validate_unit_info integration tests for sub-units
    // ========================================================================

    #[test]
    fn test_validate_unit_info_no_sub_units_config() {
        let mut sub_units_map = HashMap::new();
        sub_units_map.insert("Test".to_string(), "SUB-1".to_string());

        let unit_info = UnitInfo {
            serial_number: Some("SN-001".to_string()),
            part_number: Some("PN-001".to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: None,
            sub_units: Some(sub_units_map),
            status: "tested".to_string(),
            metadata: None,
        };
        let config = Some(crate::procedure::UnitConfig {
            auto_identify: false,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            metadata: None,
        });
        let result = validate_unit_info(&unit_info, &config, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unit_info_with_sub_units_config() {
        let mut sub_units_map = HashMap::new();
        sub_units_map.insert("battery".to_string(), "BAT-001".to_string());
        sub_units_map.insert("motor".to_string(), "MOT-002".to_string());

        let unit_info = UnitInfo {
            serial_number: Some("SN-001".to_string()),
            part_number: Some("PN-001".to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: None,
            sub_units: Some(sub_units_map),
            status: "tested".to_string(),
            metadata: None,
        };
        let config = Some(crate::procedure::UnitConfig {
            auto_identify: false,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig {
                    label: "Battery".to_string(),
                    key: None,
                    serial_number: None,
                },
                SubUnitItemConfig {
                    label: "Motor".to_string(),
                    key: None,
                    serial_number: None,
                },
            ])),
            metadata: None,
        });
        let result = validate_unit_info(&unit_info, &config, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unit_info_sub_units_missing_when_required() {
        let unit_info = UnitInfo {
            serial_number: Some("SN-001".to_string()),
            part_number: Some("PN-001".to_string()),
            revision_number: None,
            batch_number: None,
            operated_by: None,
            sub_units: None,
            status: "tested".to_string(),
            metadata: None,
        };
        let config = Some(crate::procedure::UnitConfig {
            auto_identify: false,
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig {
                    label: "Battery".to_string(),
                    key: None,
                    serial_number: None,
                },
                SubUnitItemConfig {
                    label: "Motor".to_string(),
                    key: None,
                    serial_number: None,
                },
            ])),
            metadata: None,
        });
        let result = validate_unit_info(&unit_info, &config, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Sub-units: expected 2"));
    }

    #[test]
    fn test_validate_sub_units_with_custom_key() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Li-Ion Battery Pack".to_string(),
                key: Some("battery".to_string()),
                serial_number: None,
            },
            SubUnitItemConfig {
                label: "3-Phase Motor".to_string(),
                key: Some("motor".to_string()),
                serial_number: None,
            },
        ]);
        let sub_units = create_sub_units_map(vec![("battery", "BAT-001"), ("motor", "MOT-002")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_sub_units_custom_key_missing() {
        let config = create_config_with_items(vec![SubUnitItemConfig {
            label: "Li-Ion Battery Pack".to_string(),
            key: Some("battery".to_string()),
            serial_number: None,
        }]);
        let sub_units = create_sub_units_map(vec![("wrong_key", "BAT-001")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains(": missing"));
        assert!(err.contains("Li-Ion Battery Pack"));
    }
}
