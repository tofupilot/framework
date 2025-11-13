//! Procedure validation and diagnostic reporting.
//!
//! Validates YAML procedures for schema compliance, dependency cycles,
//! file existence, and provides detailed error diagnostics with
//! line/column information.

use crate::schema::procedure::ProcedureDefinition;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub diagnostics: Vec<ValidationDiagnostic>,
    #[serde(default)]
    pub phase_plugs: HashMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct ValidationDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub related_info: Vec<RelatedDiagnosticInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct RelatedDiagnosticInfo {
    pub message: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, TS)]
#[ts(export)]
#[repr(u8)]
pub enum DiagnosticSeverity {
    Error = 8,
    Warning = 4,
    Info = 2,
    Hint = 1,
}

impl serde::Serialize for DiagnosticSeverity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(*self as u8)
    }
}

impl<'de> serde::Deserialize<'de> for DiagnosticSeverity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        match value {
            8 => Ok(DiagnosticSeverity::Error),
            4 => Ok(DiagnosticSeverity::Warning),
            2 => Ok(DiagnosticSeverity::Info),
            1 => Ok(DiagnosticSeverity::Hint),
            _ => Err(serde::de::Error::custom(format!(
                "Invalid severity value: {}",
                value
            ))),
        }
    }
}

impl ValidationDiagnostic {
    pub fn error(code: &str, message: String, line: usize, col: usize, len: usize) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            code: code.to_string(),
            message,
            start_line: line,
            start_column: col,
            end_line: line,
            end_column: col + len,
            source: "tofupilot".to_string(),
            related_info: vec![],
        }
    }

    pub fn warning(code: &str, message: String, line: usize, col: usize, len: usize) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            code: code.to_string(),
            message,
            start_line: line,
            start_column: col,
            end_line: line,
            end_column: col + len,
            source: "tofupilot".to_string(),
            related_info: vec![],
        }
    }

    pub fn with_related(mut self, related: Vec<RelatedDiagnosticInfo>) -> Self {
        self.related_info = related;
        self
    }
}

pub struct YamlLocationMap {
    locations: HashMap<String, (usize, usize, usize)>,
    content: String,
}

impl YamlLocationMap {
    pub fn new(yaml_content: &str) -> Self {
        let mut locations = HashMap::new();

        for (line_num, line) in yaml_content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();

            if trimmed.starts_with("- name:") {
                if let Some(name_start) = line.find("name:") {
                    let value_start = name_start + 5;
                    if let Some(colon_pos) = line[name_start..].find(':') {
                        let value = line[name_start + colon_pos + 1..].trim();
                        let name = value.trim_matches('"').trim_matches('\'');
                        if !name.is_empty() {
                            let col = value_start + line[value_start..].find(name).unwrap_or(0) + 1;
                            locations
                                .insert(format!("phase:{}", name), (line_num, col, name.len()));
                        }
                    }
                }
            }

            if trimmed.starts_with("- key:") {
                if let Some(key_start) = line.find("key:") {
                    let value_start = key_start + 4;
                    if let Some(colon_pos) = line[key_start..].find(':') {
                        let value = line[key_start + colon_pos + 1..].trim();
                        let key = value.trim_matches('"').trim_matches('\'');
                        if !key.is_empty() {
                            let col = value_start + line[value_start..].find(key).unwrap_or(0) + 1;
                            locations.insert(format!("plug:{}", key), (line_num, col, key.len()));
                        }
                    }
                }
            }

            if trimmed == "plugs:" {
                locations.insert("section:plugs".to_string(), (line_num, indent + 1, 5));
            }

            if trimmed == "main:" {
                locations.insert("section:main".to_string(), (line_num, indent + 1, 4));
            }

            if let Some(colon_pos) = trimmed.find(':') {
                let key = trimmed[..colon_pos].trim();
                if !key.is_empty() && !key.starts_with('-') {
                    let col = indent + 1;
                    locations.insert(
                        format!("field:{}:{}", line_num, key),
                        (line_num, col, key.len()),
                    );
                }
            }

            locations.insert(
                format!("line:{}", line_num),
                (line_num, indent + 1, trimmed.len().max(1)),
            );
        }

        Self {
            locations,
            content: yaml_content.to_string(),
        }
    }

    pub fn get_phase_location(&self, phase_name: &str) -> Option<(usize, usize, usize)> {
        self.locations
            .get(&format!("phase:{}", phase_name))
            .copied()
    }

    pub fn get_plug_location(&self, plug_key: &str) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("plug:{}", plug_key)).copied()
    }

    pub fn get_section_location(&self, section: &str) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("section:{}", section)).copied()
    }

    pub fn get_line_location(&self, line: usize) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("line:{}", line)).copied()
    }

    pub fn find_python_field_for_phase(&self, phase_name: &str) -> Option<(usize, usize, usize)> {
        if let Some((phase_line, _, _)) = self.get_phase_location(phase_name) {
            for (line_num, line) in self.content.lines().enumerate().skip(phase_line) {
                let line_num = line_num + 1;
                let trimmed = line.trim();

                if trimmed.starts_with("- name:") && line_num != phase_line {
                    break;
                }

                if trimmed.starts_with("python:") {
                    let col = line.find("python:").unwrap_or(0) + 1;
                    return Some((line_num, col, 6));
                }
            }
        }
        None
    }
}

pub fn validate_yaml_syntax(yaml_content: &str) -> ValidationResult {
    let mut diagnostics = Vec::new();

    match serde_yaml::from_str::<serde_yaml::Value>(yaml_content) {
        Ok(_) => {}
        Err(e) => {
            let (line, col) = if let Some(location) = e.location() {
                (location.line(), location.column())
            } else {
                (1, 1)
            };

            let error_msg = e.to_string();

            // Calculate highlight length based on the line content
            let lines: Vec<&str> = yaml_content.lines().collect();
            let highlight_len = if line > 0 && line <= lines.len() {
                let target_line = lines[line - 1];
                if col > 0 && col <= target_line.len() {
                    (target_line.len() - col + 1).max(1)
                } else {
                    target_line.trim().len().max(1)
                }
            } else {
                1
            };

            diagnostics.push(ValidationDiagnostic::error(
                "yaml-syntax-error",
                format!("YAML syntax error: {}", error_msg),
                line,
                col,
                highlight_len,
            ));
        }
    }

    ValidationResult {
        is_valid: diagnostics.is_empty(),
        diagnostics,
        phase_plugs: HashMap::new(),
    }
}

pub async fn validate_procedure_with_yaml(
    app_handle: &tauri::AppHandle,
    procedure: &ProcedureDefinition,
    yaml_content: &str,
    project_dir: &Path,
) -> ValidationResult {
    let syntax_result = validate_yaml_syntax(yaml_content);
    if !syntax_result.is_valid {
        return syntax_result;
    }

    let location_map = YamlLocationMap::new(yaml_content);
    let mut diagnostics = Vec::new();

    diagnostics.extend(validate_structure(procedure, &location_map));
    diagnostics.extend(validate_python_modules(
        procedure,
        project_dir,
        &location_map,
    ));

    let phase_plugs = extract_phase_plugs(app_handle, procedure, project_dir).await;

    let is_valid = !diagnostics
        .iter()
        .any(|d| matches!(d.severity, DiagnosticSeverity::Error));

    ValidationResult {
        is_valid,
        diagnostics,
        phase_plugs,
    }
}

fn validate_structure(
    procedure: &ProcedureDefinition,
    location_map: &YamlLocationMap,
) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();

    let mut all_phases = Vec::new();
    all_phases.extend(procedure.setup.iter().map(|p| ("setup", p)));
    all_phases.extend(procedure.main.iter().map(|p| ("main", p)));
    all_phases.extend(procedure.teardown.iter().map(|p| ("teardown", p)));

    let mut phase_names: HashMap<String, (usize, usize, usize)> = HashMap::new();
    for (_, phase) in &all_phases {
        let phase_display_name = phase.get_display_name();
        if let Some(existing_loc) = phase_names.get(&phase_display_name) {
            if let Some((line, col, len)) = location_map.get_phase_location(&phase_display_name) {
                diagnostics.push(
                    ValidationDiagnostic::error(
                        "duplicate-phase-name",
                        format!("Duplicate phase name: '{}'", phase_display_name),
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
        } else {
            if let Some(loc) = location_map.get_phase_location(&phase_display_name) {
                phase_names.insert(phase_display_name, loc);
            }
        }
    }

    let mut plug_keys: HashMap<String, (usize, usize, usize)> = HashMap::new();
    for plug in &procedure.plugs {
        let key = &plug.key;
        if let Some(existing_loc) = plug_keys.get(key) {
            if let Some((line, col, len)) = location_map.get_plug_location(key) {
                diagnostics.push(
                    ValidationDiagnostic::error(
                        "duplicate-plug-key",
                        format!("Duplicate plug key: '{}'", key),
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
        } else {
            if let Some(loc) = location_map.get_plug_location(key) {
                plug_keys.insert(key.clone(), loc);
            }
        }
    }

    let phase_keys_set: HashSet<String> = all_phases
        .iter()
        .map(|(_, p)| p.get_key())
        .collect();
    for (_, phase) in &all_phases {
        for dep in &phase.depends_on {
            if !phase_keys_set.contains(dep) {
                let phase_display_name = phase.get_display_name();
                if let Some((line, col, len)) = location_map.get_phase_location(&phase_display_name)
                {
                    diagnostics.push(ValidationDiagnostic::error(
                        "orphaned-dependency",
                        format!(
                            "Phase '{}' depends on non-existent phase key '{}' (use phase keys, not names)",
                            phase_display_name, dep
                        ),
                        line,
                        col,
                        len,
                    ));
                }
            }
        }
    }

    let all_phase_defs: Vec<_> = all_phases.iter().map(|(_, p)| (*p).clone()).collect();
    if let Some(cycle) = detect_circular_dependencies(&all_phase_defs) {
        let empty = String::new();
        let first_phase_name = cycle.first().unwrap_or(&empty);
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

    diagnostics
}

fn validate_python_modules(
    procedure: &ProcedureDefinition,
    project_dir: &Path,
    location_map: &YamlLocationMap,
) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();

    let mut all_phases = Vec::new();
    all_phases.extend(procedure.setup.iter());
    all_phases.extend(procedure.main.iter());
    all_phases.extend(procedure.teardown.iter());

    for phase in all_phases {
        let phase_display_name = phase.get_display_name();
        if let Some(ref python_spec) = phase.python {
            match python_spec.parse(project_dir) {
                Ok((file_path, callable_name)) => {
                    if !file_path.exists() {
                        if let Some((line, col, len)) =
                            location_map.find_python_field_for_phase(&phase_display_name)
                        {
                            diagnostics.push(ValidationDiagnostic::error(
                                "module-not-found",
                                format!(
                                    "Python module not found (expected at {})",
                                    file_path.display()
                                ),
                                line,
                                col,
                                len,
                            ));
                        } else if let Some((line, col, len)) =
                            location_map.get_phase_location(&phase_display_name)
                        {
                            diagnostics.push(ValidationDiagnostic::error(
                                "module-not-found",
                                format!(
                                    "Python module not found (expected at {})",
                                    file_path.display()
                                ),
                                line,
                                col,
                                len,
                            ));
                        }
                        continue;
                    }

                    let file_content = match std::fs::read_to_string(&file_path) {
                        Ok(content) => content,
                        Err(_) => continue,
                    };

                    let function_pattern = format!("def {}(", callable_name);
                    let class_pattern = format!("class {}(", callable_name);
                    let class_pattern_no_inherit = format!("class {}:", callable_name);

                    if !file_content.contains(&function_pattern)
                        && !file_content.contains(&class_pattern)
                        && !file_content.contains(&class_pattern_no_inherit)
                    {
                        if let Some((line, col, len)) =
                            location_map.find_python_field_for_phase(&phase_display_name)
                        {
                            diagnostics.push(ValidationDiagnostic::error(
                                "callable-not-found",
                                format!(
                                    "Function or class '{}' not found in module",
                                    callable_name
                                ),
                                line,
                                col,
                                len,
                            ));
                        } else if let Some((line, col, len)) =
                            location_map.get_phase_location(&phase_display_name)
                        {
                            diagnostics.push(ValidationDiagnostic::error(
                                "callable-not-found",
                                format!(
                                    "Function or class '{}' not found in module",
                                    callable_name
                                ),
                                line,
                                col,
                                len,
                            ));
                        }
                    }
                }
                Err(err) => {
                    if let Some((line, col, len)) =
                        location_map.find_python_field_for_phase(&phase_display_name)
                    {
                        diagnostics.push(ValidationDiagnostic::error(
                            "invalid-python-spec",
                            format!("Invalid Python spec: {}", err),
                            line,
                            col,
                            len,
                        ));
                    } else if let Some((line, col, len)) =
                        location_map.get_phase_location(&phase_display_name)
                    {
                        diagnostics.push(ValidationDiagnostic::error(
                            "invalid-python-spec",
                            format!("Invalid Python spec: {}", err),
                            line,
                            col,
                            len,
                        ));
                    }
                }
            }
        }
    }

    diagnostics
}

pub fn validate_slot_names(slots: &[String]) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut seen = HashSet::new();

    for slot in slots {
        if !seen.insert(slot.clone()) {
            diagnostics.push(ValidationDiagnostic::error(
                "duplicate-slot-name",
                format!("Duplicate slot name: '{}'", slot),
                1,
                1,
                10,
            ));
        }
    }

    diagnostics
}

fn detect_circular_dependencies(
    phases: &[crate::schema::procedure::PhaseDefinition],
) -> Option<Vec<String>> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for phase in phases {
        graph.insert(phase.get_display_name(), phase.depends_on.clone());
    }

    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    for phase in phases {
        let phase_display_name = phase.get_display_name();
        if !visited.contains(&phase_display_name) {
            if let Some(cycle) = dfs_detect_cycle(
                &phase_display_name,
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
                let cycle_start = path.iter().position(|n| n == neighbor)
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

async fn extract_phase_plugs(
    app_handle: &tauri::AppHandle,
    procedure: &ProcedureDefinition,
    project_dir: &Path,
) -> HashMap<String, Vec<String>> {
    use std::process::Command;

    let mut phase_plugs: HashMap<String, Vec<String>> = HashMap::new();

    // Get all available plug keys
    let available_plugs: Vec<String> = procedure
        .get_all_plugs_with_scope()
        .iter()
        .map(|(_, plug)| plug.get_key())
        .collect();

    if available_plugs.is_empty() {
        return phase_plugs;
    }

    // Collect modules to check (only Python phases)
    let modules_to_check: Vec<serde_json::Value> = procedure
        .get_all_phases_with_stage_scope()
        .iter()
        .filter(|(_, phase)| phase.python.is_some())
        .map(|(_, phase)| {
            let python_spec = phase.python.as_ref().unwrap();
            serde_json::json!({
                "phase_name": phase.name,
                "file": python_spec.get_module(),
                "function": python_spec.get_callable_name()
            })
        })
        .collect();

    if modules_to_check.is_empty() {
        return phase_plugs;
    }

    // Python script to extract function parameters
    let python_script = r#"
import sys
import json
import ast
from pathlib import Path

def extract_plugs(module_info, procedure_dir, available_plugs):
    file_path = module_info['file']
    function_name = module_info['function']
    phase_name = module_info['phase_name']

    try:
        if not file_path.endswith('.py'):
            file_path = file_path.replace('.', '/') + '.py'

        module_file = procedure_dir / file_path

        if not module_file.exists():
            return {"phase": phase_name, "inferred_plugs": []}

        with open(module_file, 'r') as f:
            source = f.read()

        try:
            tree = compile(source, str(module_file), 'exec', ast.PyCF_ONLY_AST)
        except:
            return {"phase": phase_name, "inferred_plugs": []}

        function_params = []
        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef) and node.name == function_name:
                function_params = [arg.arg for arg in node.args.args]
                break

        runtime_params = {'run', 'ui', 'measurements', 'logs', 'unit'}
        inferred_plugs = []

        for param in function_params:
            if param in runtime_params:
                continue

            for plug_key in available_plugs:
                if param.lower() == plug_key.lower():
                    inferred_plugs.append(plug_key)
                    break

        return {"phase": phase_name, "inferred_plugs": inferred_plugs}
    except:
        return {"phase": phase_name, "inferred_plugs": []}

modules = json.loads(sys.argv[1])
procedure_dir = Path(sys.argv[2])
available_plugs = json.loads(sys.argv[3])
results = []
for module_info in modules:
    result = extract_plugs(module_info, procedure_dir, available_plugs)
    if result:
        results.append(result)
print(json.dumps(results))
"#;

    // Resolve Python executable using UV
    let python_path =
        match crate::python::resolve_python_executable(Some(app_handle), project_dir).await {
            Ok(path) => path,
            Err(e) => {
                log::warn!(
                "Failed to resolve Python for plug inference: {}. Skipping plug auto-detection.",
                e
            );
                return phase_plugs;
            }
        };

    // Run Python script
    let mut cmd = Command::new(&python_path);
    cmd.arg("-c")
        .arg(python_script)
        .arg(serde_json::to_string(&modules_to_check).unwrap_or_default())
        .arg(project_dir.to_string_lossy().to_string())
        .arg(serde_json::to_string(&available_plugs).unwrap_or_default());

    crate::utils::configure_no_window(&mut cmd);

    if let Ok(output) = cmd.output() {
        if let Ok(results) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) {
            for result in results {
                if let (Some(phase_name), Some(plugs_array)) = (
                    result.get("phase").and_then(|p| p.as_str()),
                    result.get("inferred_plugs").and_then(|p| p.as_array()),
                ) {
                    let plugs: Vec<String> = plugs_array
                        .iter()
                        .filter_map(|p| p.as_str().map(|s| s.to_string()))
                        .collect();

                    if !plugs.is_empty() {
                        phase_plugs.insert(phase_name.to_string(), plugs);
                    }
                }
            }
        }
    }

    phase_plugs
}
