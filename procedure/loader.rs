use crate::procedure::schema::{ProcedureDefinition, ProcedureYaml};
use super::error::CommandError;
use std::path::Path;
use validator::Validate;

fn validate_file_path(path: &Path) -> Result<(), CommandError> {
    if !path.exists() {
        return Err(CommandError::file_not_found(path.display()));
    }

    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| CommandError::new(
            super::error::ErrorCode::InvalidFileExtension,
            "File has no extension"
        ))?;

    if extension != "yaml" && extension != "yml" {
        return Err(CommandError::new(
            super::error::ErrorCode::InvalidFileExtension,
            "File must be a YAML file (.yaml or .yml)"
        ));
    }

    Ok(())
}

#[must_use = "procedure definition should be checked for validation errors"]
pub fn load_procedure_definition(file_path: &Path) -> Result<ProcedureDefinition, String> {
    validate_file_path(file_path).map_err(|e| e.message)?;

    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

    let raw: ProcedureYaml = serde_yaml::from_str(&content)
        .map_err(|e| format!("Failed to parse YAML: {}", e))?;

    let procedure_def = ProcedureDefinition::from(raw);

    procedure_def
        .validate()
        .map_err(|e| format!("Validation failed: {}", e))?;

    if let Some(unit) = &procedure_def.unit {
        unit.validate_auto_identify()
            .map_err(|e| format!("Validation failed: {}", e))?;
    }

    for (_, phase) in procedure_def.get_all_phases_with_stage_scope() {
        phase.validate_single_runtime()?;
        if let Some(ui) = &phase.ui {
            if let Some(components) = &ui.components {
                for comp in components {
                    comp.validate_width()?;
                    comp.validate_aspect()?;
                    comp.validate_fit()?;
                    comp.validate_options_count()?;
                }
            }
        }
    }

    Ok(procedure_def)
}
