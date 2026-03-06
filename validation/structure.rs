//! Structure validation: duplicates, dependencies, circular references.

use std::collections::{HashMap, HashSet};
use crate::procedure::schema::ProcedureDefinition;
use super::types::{ValidationDiagnostic, RelatedDiagnosticInfo};
use super::location_map::YamlLocationMap;

#[cfg(test)]
#[path = "structure_tests.rs"]
mod tests;

fn check_duplicates<F>(
    diagnostics: &mut Vec<ValidationDiagnostic>,
    items: impl Iterator<Item = String>,
    get_location: F,
    error_code: &str,
    item_type: &str,
) where
    F: Fn(&str) -> Option<(usize, usize, usize)>,
{
    let mut seen: HashMap<String, (usize, usize, usize)> = HashMap::new();

    for name in items {
        if let Some(existing_loc) = seen.get(&name) {
            if let Some((line, col, len)) = get_location(&name) {
                diagnostics.push(
                    ValidationDiagnostic::error(
                        error_code,
                        format!("Duplicate {} '{}'", item_type, name),
                        line,
                        col,
                        len,
                    )
                    .with_related(vec![RelatedDiagnosticInfo {
                        message: "Also defined here".to_string(),
                        start_line: existing_loc.0,
                        start_column: existing_loc.1,
                        end_line: existing_loc.0,
                        end_column: existing_loc.1 + existing_loc.2,
                    }]),
                );
            }
        } else if let Some(loc) = get_location(&name) {
            seen.insert(name, loc);
        }
    }
}

pub(super) fn validate_structure(
    procedure: &ProcedureDefinition,
    location_map: &YamlLocationMap,
) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();

    let all_phases: Vec<_> = procedure.setup.iter()
        .chain(procedure.main.iter())
        .chain(procedure.teardown.iter())
        .collect();

    // Check duplicate phase keys
    check_duplicates(
        &mut diagnostics,
        all_phases.iter().map(|p| p.key.clone()),
        |key| {
            all_phases
                .iter()
                .find(|p| &p.key == key)
                .and_then(|p| location_map.get_phase_location(&p.name))
        },
        "duplicate-phase-key",
        "phase key",
    );

    // Check duplicate plug keys
    check_duplicates(
        &mut diagnostics,
        procedure.plugs.iter().map(|p| p.key.clone()),
        |key| location_map.get_plug_location(key),
        "duplicate-plug-key",
        "plug key",
    );

    // Check duplicate measurement keys within each phase
    for phase in &all_phases {
        let mut seen_meas_keys: HashMap<String, (usize, usize, usize)> = HashMap::new();

        for meas in &phase.measurements {
            if let Some(existing_loc) = seen_meas_keys.get(&meas.key) {
                // Use phase location as fallback for measurement location
                if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                    diagnostics.push(
                        ValidationDiagnostic::error(
                            "duplicate-measurement-key",
                            format!("Phase '{}' has duplicate measurement key: '{}'", phase.name, meas.key),
                            line,
                            col,
                            len,
                        )
                        .with_related(vec![RelatedDiagnosticInfo {
                            message: "Also defined here".to_string(),
                            start_line: existing_loc.0,
                            start_column: existing_loc.1,
                            end_line: existing_loc.0,
                            end_column: existing_loc.1 + existing_loc.2,
                        }]),
                    );
                }
            } else if let Some(loc) = location_map.get_phase_location(&phase.name) {
                seen_meas_keys.insert(meas.key.clone(), loc);
            }
        }
    }

    // Validate y_axis entries have at least key or legend
    for phase in &all_phases {
        for meas in &phase.measurements {
            let y_axes = meas.y_axis.as_ref();
            if let Some(axes) = y_axes {
                for (i, axis) in axes.iter().enumerate() {
                    let has_key = axis.key.as_ref().is_some_and(|k| !k.is_empty());
                    let has_legend = axis.legend.as_ref().is_some_and(|l| !l.is_empty());
                    if !has_key && !has_legend {
                        if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                            diagnostics.push(ValidationDiagnostic::error(
                                "missing-y-axis-key-or-legend",
                                format!(
                                    "Phase '{}', measurement '{}': y_axis[{}] must have at least 'key' or 'legend'",
                                    phase.name, meas.name, i
                                ),
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                }
            }
        }
    }

    // Validate component width values (must be percentage like "50%")
    for phase in &all_phases {
        if let Some(ui) = &phase.ui {
            if let Some(components) = &ui.components {
                for comp in components {
                    if let Err(msg) = comp.validate_width() {
                        if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                            diagnostics.push(ValidationDiagnostic::error(
                                "invalid-width",
                                msg,
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                    if let Err(msg) = comp.validate_aspect() {
                        if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                            diagnostics.push(ValidationDiagnostic::error(
                                "invalid-aspect",
                                msg,
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                    if let Err(msg) = comp.validate_fit() {
                        if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                            diagnostics.push(ValidationDiagnostic::error(
                                "invalid-fit",
                                msg,
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                }
            }
        }
    }

    // Validate axis validator operators (only == and != allowed on axis data)
    for phase in &all_phases {
        for meas in &phase.measurements {
            let mut axes_to_check: Vec<(String, usize, &crate::procedure::schema::ValidatorSpec)> = Vec::new();

            // Collect validators from axes
            if let Some(ref x) = meas.x_axis {
                if let Some(ref validators) = x.validators {
                    for (vi, v) in validators.iter().enumerate() {
                        axes_to_check.push(("x_axis".to_string(), vi, v));
                    }
                }
            }
            if let Some(ref y_axes) = meas.y_axis {
                for (yi, y) in y_axes.iter().enumerate() {
                    if let Some(ref validators) = y.validators {
                        for (vi, v) in validators.iter().enumerate() {
                            axes_to_check.push((format!("y_axis[{}]", yi), vi, v));
                        }
                    }
                }
            }

            for (axis_label, vi, validator) in &axes_to_check {
                if let Some(ref op) = validator.operator {
                    if op != "==" && op != "!=" {
                        if let Some((line, col, len)) = location_map.get_phase_location(&phase.name) {
                            diagnostics.push(ValidationDiagnostic::error(
                                "invalid-axis-validator-operator",
                                format!(
                                    "Phase '{}', measurement '{}', {}.validators[{}]: operator '{}' is not valid for axis data (only '==' and '!=' are allowed)",
                                    phase.name, meas.name, axis_label, vi, op
                                ),
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                }
            }
        }
    }

    let phase_keys_set: HashSet<String> = all_phases.iter().map(|p| p.key.clone()).collect();
    for phase in &all_phases {
        for dep in &phase.depends_on {
            if !phase_keys_set.contains(dep) {
                let phase_name = phase.name.clone();
                if let Some((line, col, len)) = location_map.get_phase_location(&phase_name) {
                    diagnostics.push(ValidationDiagnostic::error(
                        "orphaned-dependency",
                        format!(
                            "Phase '{}' depends on non-existent phase key '{}' (use phase keys, not names)",
                            phase_name, dep
                        ),
                        line,
                        col,
                        len,
                    ));
                }
            }
        }
    }

    let phase_defs: Vec<_> = all_phases.iter().map(|p| (*p).clone()).collect();
    if let Some(cycle) = detect_circular_dependencies(&phase_defs) {
        if let Some(first_phase_name) = cycle.first() {
            if let Some((line, col, len)) = location_map.get_phase_location(first_phase_name) {
                diagnostics.push(ValidationDiagnostic::error(
                    "circular-dependency",
                    format!("Circular dependency detected: {}", cycle.join(" → ")),
                    line,
                    col,
                    len,
                ));
            }
        }
    }

    diagnostics
}

fn detect_circular_dependencies(
    phases: &[crate::procedure::schema::PhaseDefinition],
) -> Option<Vec<String>> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for phase in phases {
        graph.insert(phase.key.clone(), phase.depends_on.clone());
    }

    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    for phase in phases {
        let phase_key = phase.key.clone();
        if !visited.contains(&phase_key) {
            if let Some(cycle) = dfs_detect_cycle(
                &phase_key,
                &graph,
                &mut visited,
                &mut rec_stack,
                &mut path,
            ) {
                return Some(cycle);
            }
        }
    }

    None
}

fn dfs_detect_cycle(
    node: &str,
    graph: &HashMap<String, Vec<String>>,
    visited: &mut HashSet<String>,
    rec_stack: &mut HashSet<String>,
    path: &mut Vec<String>,
) -> Option<Vec<String>> {
    visited.insert(node.to_string());
    rec_stack.insert(node.to_string());
    path.push(node.to_string());

    if let Some(neighbors) = graph.get(node) {
        for neighbor in neighbors {
            if !visited.contains(neighbor) {
                if let Some(cycle) = dfs_detect_cycle(neighbor, graph, visited, rec_stack, path) {
                    return Some(cycle);
                }
            } else if rec_stack.contains(neighbor) {
                let cycle_start = path
                    .iter()
                    .position(|n| n == neighbor)
                    .expect("neighbor must be in path if in rec_stack");
                let mut cycle = path[cycle_start..].to_vec();
                cycle.push(neighbor.to_string());
                return Some(cycle);
            }
        }
    }

    rec_stack.remove(node);
    path.pop();
    None
}
