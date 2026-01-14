//! Execution-related types for unit information and validation.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
#[specta(export = false)]
pub struct UnitInfo {
    pub serial_number: Option<String>,
    pub part_number: Option<String>,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    /// Sub-units as label -> serial_number mapping
    pub sub_units: Option<HashMap<String, String>>,
    pub status: String,
}

/// Validate a single unit field against its configuration
fn validate_unit_field(
    field_name: &str,
    value: &Option<String>,
    config: &crate::procedure::UnitFieldConfig,
) -> Result<(), String> {
    // Serial number and part number are always required
    let is_required = field_name == "serial_number" || field_name == "part_number";

    if is_required {
        let val = value
            .as_ref()
            .ok_or_else(|| format!("{} is required", field_name))?;

        if val.trim().is_empty() {
            return Err(format!("{} cannot be empty", field_name));
        }
    }

    if let Some(val) = value {
        let trimmed = val.trim();

        // Ensure at least 1 character after trim if value is provided
        if trimmed.is_empty() {
            return Err(format!(
                "{} cannot be empty or contain only whitespace",
                field_name
            ));
        }

        // Check min_length on trimmed value
        if let Some(min) = config.min_length {
            if trimmed.len() < min {
                return Err(format!(
                    "{} must be at least {} characters (got {})",
                    field_name,
                    min,
                    trimmed.len()
                ));
            }
        }

        // Check max_length on trimmed value
        if let Some(max) = config.max_length {
            if trimmed.len() > max {
                return Err(format!(
                    "{} must be at most {} characters (got {})",
                    field_name,
                    max,
                    trimmed.len()
                ));
            }
        }

        // Check pattern on trimmed value
        if let Some(pattern) = &config.pattern {
            let regex = regex::Regex::new(pattern)
                .map_err(|e| format!("Invalid validation pattern for {}: {}", field_name, e))?;

            if !regex.is_match(trimmed) {
                return Err(format!(
                    "{} does not match required format: {}",
                    field_name, pattern
                ));
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
        return Err(format!("{} serial number is required", label));
    }

    // Apply constraints if config exists
    if let Some(field_config) = config {
        // Check min_length
        if let Some(min) = field_config.min_length {
            if trimmed.len() < min {
                return Err(format!(
                    "{} must be at least {} characters (got {})",
                    label, min, trimmed.len()
                ));
            }
        }

        // Check max_length
        if let Some(max) = field_config.max_length {
            if trimmed.len() > max {
                return Err(format!(
                    "{} must be at most {} characters (got {})",
                    label, max, trimmed.len()
                ));
            }
        }

        // Check pattern
        if let Some(pattern) = &field_config.pattern {
            let regex = regex::Regex::new(pattern)
                .map_err(|e| format!("Invalid validation pattern for {}: {}", label, e))?;

            if !regex.is_match(trimmed) {
                return Err(format!(
                    "{} does not match required format: {}",
                    label, pattern
                ));
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
    let sub_units_map = sub_units.as_ref().ok_or_else(|| {
        format!("Expected {} sub-units but got none", expected_count)
    })?;

    // Check count matches
    if sub_units_map.len() != expected_count {
        return Err(format!(
            "Expected {} sub-units but got {}",
            expected_count,
            sub_units_map.len()
        ));
    }

    // Validate each configured sub-unit (using key for lookup, label for error messages)
    for item in &sub_units_config.0 {
        let key = item.get_key();
        let serial = sub_units_map.get(&key).ok_or_else(|| {
            format!("Missing sub-unit '{}' (key: {})", item.label, key)
        })?;

        validate_sub_unit_field(&item.label, serial, &item.serial_number)?;
    }

    Ok(())
}

/// Validate all unit fields against configuration
pub fn validate_unit_info(
    unit_info: &UnitInfo,
    unit_config: &Option<crate::procedure::UnitConfig>,
) -> Result<(), String> {
    let config = match unit_config {
        Some(c) => c,
        None => return Ok(()), // No config = no validation
    };

    // Validate built-in fields
    if let Some(sn_config) = &config.serial_number {
        validate_unit_field("Serial Number", &unit_info.serial_number, sn_config)?;
    }

    if let Some(pn_config) = &config.part_number {
        validate_unit_field("Part Number", &unit_info.part_number, pn_config)?;
    }

    if let Some(rev_config) = &config.revision_number {
        validate_unit_field("Revision Number", &unit_info.revision_number, rev_config)?;
    }

    if let Some(batch_config) = &config.batch_number {
        validate_unit_field("Batch Number", &unit_info.batch_number, batch_config)?;
    }

    // Validate sub-units if configured
    if let Some(sub_units_config) = &config.sub_units {
        validate_sub_units(&unit_info.sub_units, sub_units_config)?;
    }

    Ok(())
}

pub struct PendingUnitInput {
    pub sender: tokio::sync::oneshot::Sender<UnitInfo>,
    pub unit_config: Option<crate::procedure::UnitConfig>,
}

#[cfg(test)]
mod tests {
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
        assert!(result.unwrap_err().contains("Battery serial number is required"));
    }

    #[test]
    fn test_validate_sub_unit_field_whitespace_only_fails() {
        let result = validate_sub_unit_field("Motor", "   ", &None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Motor serial number is required"));
    }

    #[test]
    fn test_validate_sub_unit_field_min_length_pass() {
        let config = Some(UnitFieldConfig {
            min_length: Some(5),
            max_length: None,
            pattern: None,
            default_value: None,
            placeholder: None,
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
            default_value: None,
            placeholder: None,
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
            default_value: None,
            placeholder: None,
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
            default_value: None,
            placeholder: None,
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
            default_value: None,
            placeholder: None,
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
            default_value: None,
            placeholder: None,
        });
        let result = validate_sub_unit_field("Battery", "INVALID", &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not match required format"));
    }

    #[test]
    fn test_validate_sub_unit_field_trims_whitespace() {
        let config = Some(UnitFieldConfig {
            min_length: Some(3),
            max_length: Some(10),
            pattern: None,
            default_value: None,
            placeholder: None,
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
        Some(pairs.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn test_validate_sub_units_success() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Controller".to_string(), key: None, serial_number: None },
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
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
        ]);
        let result = validate_sub_units(&None, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected 2 sub-units but got none"));
    }

    #[test]
    fn test_validate_sub_units_wrong_count_too_few() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Controller".to_string(), key: None, serial_number: None },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected 3 sub-units but got 2"));
    }

    #[test]
    fn test_validate_sub_units_wrong_count_too_many() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
            ("controller", "CTL-003"),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected 2 sub-units but got 3"));
    }

    #[test]
    fn test_validate_sub_units_empty_serial_fails() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", ""),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Motor serial number is required"));
    }

    #[test]
    fn test_validate_sub_units_missing_label() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
            SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("wronglabel", "MOT-002"),
        ]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing sub-unit 'Motor'"));
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
                    default_value: None,
                    placeholder: None,
                }),
            },
            SubUnitItemConfig {
                label: "Motor".to_string(),
                key: None,
                serial_number: Some(UnitFieldConfig {
                    min_length: None,
                    max_length: None,
                    pattern: Some(r"^MOT-\d+$".to_string()),
                    default_value: None,
                    placeholder: None,
                }),
            },
        ]);
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
        ]);
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
                default_value: None,
                placeholder: None,
            }),
        }]);
        let sub_units = create_sub_units_map(vec![("battery_pack", "INVALID")]);
        let result = validate_sub_units(&sub_units, &config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Battery Pack"));
        assert!(err.contains("does not match required format"));
    }

    #[test]
    fn test_validate_sub_units_min_length_per_item() {
        let config = create_config_with_items(vec![
            SubUnitItemConfig {
                label: "Battery".to_string(),
                key: None,
                serial_number: Some(UnitFieldConfig {
                    min_length: Some(10),
                    max_length: None,
                    pattern: None,
                    default_value: None,
                    placeholder: None,
                }),
            },
        ]);
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
            sub_units: Some(sub_units_map),
            status: "tested".to_string(),
        };
        let config = Some(crate::procedure::UnitConfig {
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: None,
        });
        let result = validate_unit_info(&unit_info, &config);
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
            sub_units: Some(sub_units_map),
            status: "tested".to_string(),
        };
        let config = Some(crate::procedure::UnitConfig {
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
                SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
            ])),
        });
        let result = validate_unit_info(&unit_info, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unit_info_sub_units_missing_when_required() {
        let unit_info = UnitInfo {
            serial_number: Some("SN-001".to_string()),
            part_number: Some("PN-001".to_string()),
            revision_number: None,
            batch_number: None,
            sub_units: None,
            status: "tested".to_string(),
        };
        let config = Some(crate::procedure::UnitConfig {
            serial_number: Some(UnitFieldConfig::default()),
            part_number: Some(UnitFieldConfig::default()),
            revision_number: None,
            batch_number: None,
            sub_units: Some(SubUnitsConfig(vec![
                SubUnitItemConfig { label: "Battery".to_string(), key: None, serial_number: None },
                SubUnitItemConfig { label: "Motor".to_string(), key: None, serial_number: None },
            ])),
        });
        let result = validate_unit_info(&unit_info, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected 2 sub-units"));
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
        let sub_units = create_sub_units_map(vec![
            ("battery", "BAT-001"),
            ("motor", "MOT-002"),
        ]);
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
        assert!(err.contains("Missing sub-unit"));
        assert!(err.contains("Li-Ion Battery Pack"));
    }
}
