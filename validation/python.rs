//! Python module and callable validation.

use std::path::Path;
use crate::procedure::schema::ProcedureDefinition;
use super::types::ValidationDiagnostic;
use super::location_map::YamlLocationMap;

fn add_python_error(
    diagnostics: &mut Vec<ValidationDiagnostic>,
    location_map: &YamlLocationMap,
    phase_name: &str,
    code: &str,
    message: String,
) {
    let location = location_map
        .find_python_field_for_phase(phase_name)
        .or_else(|| location_map.get_phase_location(phase_name));

    if let Some((line, col, len)) = location {
        diagnostics.push(ValidationDiagnostic::error(code, message, line, col, len));
    }
}

pub(super) fn validate_python_modules(
    procedure: &ProcedureDefinition,
    project_dir: &Path,
    location_map: &YamlLocationMap,
) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();

    let all_phases: Vec<_> = procedure.setup.iter()
        .chain(procedure.main.iter())
        .chain(procedure.teardown.iter())
        .collect();

    for phase in all_phases {
        let Some(ref python_spec) = phase.python else { continue };
        let phase_name = phase.name.clone();

        match python_spec.parse(project_dir) {
            Ok((file_path, callable_name)) => {
                if !file_path.exists() {
                    add_python_error(
                        &mut diagnostics,
                        location_map,
                        &phase_name,
                        "module-not-found",
                        format!("Python module not found (expected at {})", file_path.display()),
                    );
                    continue;
                }

                let Ok(file_content) = std::fs::read_to_string(&file_path) else { continue };

                let patterns = [
                    format!("def {}(", callable_name),
                    format!("class {}(", callable_name),
                    format!("class {}:", callable_name),
                ];

                if !patterns.iter().any(|p| file_content.contains(p)) {
                    add_python_error(
                        &mut diagnostics,
                        location_map,
                        &phase_name,
                        "callable-not-found",
                        format!("Function or class '{}' not found in module", callable_name),
                    );
                }
            }
            Err(err) => {
                add_python_error(
                    &mut diagnostics,
                    location_map,
                    &phase_name,
                    "invalid-python-spec",
                    format!("Invalid Python spec: {}", err),
                );
            }
        }
    }

    diagnostics
}
