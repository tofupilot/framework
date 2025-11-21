//! Execution-related types for unit information and validation.

use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
#[specta(export = false)]
pub struct UnitInfo {
    pub serial_number: Option<String>,
    pub part_number: Option<String>,
    pub revision_number: Option<String>,
    pub batch_number: Option<String>,
    pub sub_units: Option<Vec<String>>,
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

    Ok(())
}

pub struct PendingUnitInput {
    pub sender: tokio::sync::oneshot::Sender<UnitInfo>,
    pub unit_config: Option<crate::procedure::UnitConfig>,
}
