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

use crate::procedure::schema::ProcedureDefinition;
use location_map::YamlLocationMap as LocationMap;

pub async fn validate_procedure_with_yaml(
    app_handle: &tauri::AppHandle,
    procedure: &ProcedureDefinition,
    yaml_content: &str,
    project_dir: &Path,
) -> ValidationResult {
    let syntax_result = syntax::validate_yaml_syntax(yaml_content);
    if !syntax_result.is_valid {
        return syntax_result;
    }

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
