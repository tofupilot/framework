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

fn add_python_warning(
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
        diagnostics.push(ValidationDiagnostic::warning(code, message, line, col, len));
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
                    // Dot-syntax module specs (no slash, no leading '.'/'~')
                    // may resolve through the venv site-packages at runtime
                    // (uv-workspace monorepo layout — tp_worker.py falls back
                    // to importlib.import_module when the file is absent).
                    // Surface as Warning instead of Error so the editor
                    // doesn't block runs that succeed via import.
                    let module_str = python_spec.get_module();
                    let is_file_path = module_str.contains('/') || module_str.contains('\\');
                    let is_relative = module_str.starts_with('.') || module_str.starts_with('~');

                    if !is_file_path && !is_relative {
                        add_python_warning(
                            &mut diagnostics,
                            location_map,
                            &phase_name,
                            "module-not-on-disk",
                            format!(
                                "Python module '{}' not found on disk; will resolve via importlib at runtime if installed in the venv",
                                module_str
                            ),
                        );
                    } else {
                        add_python_error(
                            &mut diagnostics,
                            location_map,
                            &phase_name,
                            "module-not-found",
                            format!("Python module not found (expected at {})", file_path.display()),
                        );
                    }
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
