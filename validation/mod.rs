//! Procedure validation and diagnostic reporting.
//!
//! Validates YAML procedures for schema compliance, dependency cycles,
//! file existence, and provides detailed error diagnostics with
//! line/column information.

mod location_map;
mod plugs;
mod python;
mod slots;
mod structure;
mod syntax;
mod types;

use std::path::Path;

pub use location_map::YamlLocationMap;
pub use slots::validate_slot_names;
pub use syntax::validate_yaml_syntax;
pub use types::{
    DiagnosticSeverity, RelatedDiagnosticInfo, ValidationDiagnostic, ValidationResult,
};

use crate::procedure::schema::{ProcedureDefinition, ProcedureYaml};
use location_map::YamlLocationMap as LocationMap;

fn create_empty_procedure() -> ProcedureDefinition {
    ProcedureDefinition {
        id: None,
        name: "Invalid YAML".to_string(),
        version: "0.0.0".to_string(),
        description: String::new(),
        execution: None,
        unit: None,
        plugs: vec![],
        setup: vec![],
        main: vec![],
        teardown: vec![],
    }
}

pub async fn load_and_validate(
    app_handle: &tauri::AppHandle,
    yaml_content: &str,
    project_dir: &Path,
) -> (ProcedureDefinition, ValidationResult) {
    log::info!("[VALIDATION] Starting validation");

    let yaml_config: ProcedureYaml = match serde_yaml::from_str(yaml_content) {
        Ok(config) => {
            log::info!("[VALIDATION] Schema parsing succeeded");
            config
        }
        Err(e) => {
            let (line, col) = e.location()
                .map(|loc| (loc.line(), loc.column()))
                .unwrap_or((1, 1));

            log::error!("[VALIDATION] Schema parsing failed at {}:{} - {}", line, col, e);

            let error_result = ValidationResult {
                is_valid: false,
                diagnostics: vec![ValidationDiagnostic::error(
                    "schema-parse-error",
                    format!("YAML schema error: {}", e),
                    line,
                    col,
                    1,
                )],
                phase_plugs: std::collections::HashMap::new(),
            };
            return (create_empty_procedure(), error_result);
        }
    };

    let procedure = ProcedureDefinition::from(yaml_config);
    let validation = validate_structure_and_deps(app_handle, &procedure, yaml_content, project_dir).await;

    log::info!("[VALIDATION] Complete: valid={}, diagnostics={}", validation.is_valid, validation.diagnostics.len());
    for diag in &validation.diagnostics {
        log::info!("[VALIDATION]   [{:?}] {}:{} - {}", diag.severity, diag.start_line, diag.start_column, diag.message);
    }

    (procedure, validation)
}

async fn validate_structure_and_deps(
    app_handle: &tauri::AppHandle,
    procedure: &ProcedureDefinition,
    yaml_content: &str,
    project_dir: &Path,
) -> ValidationResult {
    let location_map = LocationMap::new(yaml_content);
    let mut diagnostics = Vec::new();

    diagnostics.extend(structure::validate_structure(procedure, &location_map));
    diagnostics.extend(python::validate_python_modules(
        procedure,
        project_dir,
        &location_map,
    ));

    let phase_plugs = plugs::extract_phase_plugs(app_handle, procedure, project_dir).await;

    let is_valid = !diagnostics
        .iter()
        .any(|d| matches!(d.severity, DiagnosticSeverity::Error));

    ValidationResult {
        is_valid,
        diagnostics,
        phase_plugs,
    }
}
