//! Python file editing and analysis commands.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::AppHandle;

use crate::python::resolve_python_internal;
use crate::execution::runtime;

#[derive(Debug, Serialize, Deserialize, specta::Type)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PythonEditorContext {
    Phase { key: String },
    Plug { key: String },
    Function {
        module_path: String,
        function_name: String,
    },
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct PythonFileResult {
    pub content: String,
    pub file_path: String,
}

#[tauri::command]
#[specta::specta]
pub async fn read_python_file(
    procedure_file: String,
    procedure_dir: String,
    context: PythonEditorContext,
) -> Result<PythonFileResult, String> {
    let procedure_file_buf = PathBuf::from(&procedure_file);
    let project_path = PathBuf::from(&procedure_dir);

    let procedure_def = crate::procedure::load_procedure_definition(&procedure_file_buf)
        .map_err(|e| format!("Failed to load procedure: {}", e))?;

    let file_path = match context {
        PythonEditorContext::Phase { key } => {
            let phase = procedure_def
                .get_flat_phases()
                .into_iter()
                .find(|p| p.key == key)
                .ok_or_else(|| format!("Phase with key '{}' not found", key))?;

            let python_spec = phase
                .python
                .ok_or_else(|| format!("Phase '{}' does not have Python configuration", key))?;

            let (file_path, _callable) = python_spec.parse(&project_path)?;
            file_path
        }
        PythonEditorContext::Plug { key } => {
            let plug = procedure_def
                .plugs
                .iter()
                .find(|p| &p.key == &key)
                .ok_or_else(|| format!("Plug with key '{}' not found", key))?;

            let (file_path, _callable) = plug.python.parse(&project_path)?;
            file_path
        }
        PythonEditorContext::Function { module_path, .. } => {
            project_path
                .join(&module_path.replace('.', "/"))
                .with_extension("py")
        }
    };

    let content =
        std::fs::read_to_string(&file_path).map_err(|e| format!("Failed to read Python file: {}", e))?;

    Ok(PythonFileResult {
        content,
        file_path: file_path.to_string_lossy().to_string(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn write_python_file(file_path: String, content: String) -> Result<(), String> {
    use std::fs;

    fs::write(&file_path, content).map_err(|e| format!("Failed to write Python file: {}", e))
}

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct PythonCallable {
    pub name: String,
    pub r#type: String,
}

#[tauri::command]
#[specta::specta]
pub async fn analyze_python_file(
    app_handle: AppHandle,
    procedure_dir: String,
    file_path: String,
) -> Result<Vec<PythonCallable>, String> {
    let file_path_obj = std::path::Path::new(&file_path);
    let procedure_dir_obj = std::path::Path::new(&procedure_dir);

    if !file_path_obj.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }

    let canonical_file = file_path_obj
        .canonicalize()
        .map_err(|e| format!("Failed to resolve file path: {}", e))?;
    let canonical_procedure = procedure_dir_obj
        .canonicalize()
        .map_err(|e| format!("Failed to resolve project directory: {}", e))?;

    if !canonical_file.starts_with(&canonical_procedure) {
        return Err(format!(
            "File must be inside the project directory.\n\nFile: {}\nProject: {}",
            canonical_file.display(),
            canonical_procedure.display()
        ));
    }

    let python_path =
        resolve_python_internal(Some(&app_handle), std::path::Path::new(&procedure_dir))
            .await
            .map_err(|e| format!("Failed to resolve Python for file analysis:\n\n{}", e))?;

    let python_script = r#"
import ast
import sys
import json

def analyze_python_file(file_path):
    with open(file_path, 'r') as f:
        content = f.read()

    try:
        tree = ast.parse(content)
    except SyntaxError as e:
        return []

    callables = []

    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef):
            if not node.name.startswith('_'):
                callables.append({
                    'name': node.name,
                    'type': 'function'
                })
        elif isinstance(node, ast.ClassDef):
            if not node.name.startswith('_'):
                callables.append({
                    'name': node.name,
                    'type': 'class'
                })

    return callables

if __name__ == '__main__':
    file_path = sys.argv[1]
    result = analyze_python_file(file_path)
    print(json.dumps(result))
"#;

    let output = runtime::python::PythonCommandBuilderSync::new(&python_path)
        .hide_window()
        .arg("-c")
        .arg(python_script)
        .arg(&file_path)
        .output()
        .map_err(|e| format!("Failed to execute Python: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Python analysis failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let callables: Vec<PythonCallable> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse Python analysis result: {}", e))?;

    Ok(callables)
}

#[tauri::command]
#[specta::specta]
pub fn to_python_identifier_text(text: String) -> String {
    super::identifier::to_python_identifier(&text)
}
