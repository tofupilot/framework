//! Plug inference from Python function parameters.

use std::collections::HashMap;
use std::path::Path;
use crate::procedure::schema::ProcedureDefinition;

pub(super) async fn extract_phase_plugs(
    app_handle: &tauri::AppHandle,
    procedure: &ProcedureDefinition,
    project_dir: &Path,
) -> HashMap<String, Vec<String>> {
    let mut phase_plugs: HashMap<String, Vec<String>> = HashMap::new();

    // Get all available plug keys
    let available_plugs: Vec<String> = procedure
        .get_all_plugs_with_scope()
        .iter()
        .map(|(_, plug)| plug.key.clone())
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
        match crate::python::resolve_python_internal(Some(app_handle), project_dir).await {
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
    if let Ok(output) = crate::execution::runtime::python::PythonCommandBuilderSync::new(&python_path)
        .arg("-c")
        .arg(python_script)
        .arg(serde_json::to_string(&modules_to_check).unwrap_or_default())
        .arg(project_dir.to_string_lossy().to_string())
        .arg(serde_json::to_string(&available_plugs).unwrap_or_default())
        .output()
    {
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

