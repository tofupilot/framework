use crate::schema::procedure::{ProcedureDefinition, ProcedureYaml};
use std::path::Path;
use std::collections::HashSet;
use validator::Validate;

#[must_use = "procedure definition should be checked for validation errors"]
pub fn load_procedure_definition(file_path: &Path) -> Result<ProcedureDefinition, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read file {}: {}", file_path.display(), e))?;

    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    let raw: ProcedureYaml = match extension.to_lowercase().as_str() {
        "yaml" | "yml" => {
            serde_yaml::from_str(&content).map_err(|e| format!("Failed to parse YAML: {}", e))?
        }
        "json" => {
            serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {}", e))?
        }
        _ => {
            serde_yaml::from_str(&content)
                .or_else(|_| serde_json::from_str(&content))
                .map_err(|e| format!("Failed to parse as YAML or JSON: {}", e))?
        }
    };

    let procedure_def = ProcedureDefinition::from(raw);

    // Validate all fields using validator derives
    procedure_def.validate()
        .map_err(|e| format!("Validation error: {}", e))?;

    // Custom validation: duplicate phase names
    let mut seen_slugs: HashSet<String> = HashSet::new();
    for (_, phase) in procedure_def.get_all_phases_with_stage_scope() {
        // Check for duplicate measurement names within phase
        phase.validate_measurement_uniqueness()?;

        // Validate single runtime (business logic)
        phase.validate_single_runtime()?;

        // Check for duplicate phase keys
        let key = phase.get_key();
        if seen_slugs.contains(&key) {
            return Err(format!(
                "Duplicate phase key detected: '{}' (key: '{}' conflicts with another phase)",
                phase.get_display_name(), key
            ));
        }
        seen_slugs.insert(key);
    }

    Ok(procedure_def)
}
