use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Deserialize, Serialize)]
struct PyProjectFile {
    pub project: Option<PythonManifest>,
}

#[derive(Debug, Deserialize, Serialize, TS)]
#[ts(export)]
pub struct PythonManifest {
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(rename = "requires-python")]
    #[ts(rename = "requires_python")]
    pub requires_python: Option<String>,
    pub dependencies: Option<Vec<String>>,
    #[serde(rename = "optional-dependencies")]
    pub optional_dependencies: Option<std::collections::HashMap<String, Vec<String>>>,
}

pub fn read_pyproject(project_path: &Path) -> Result<PythonManifest, String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found".to_string());
    }

    let content = std::fs::read_to_string(&pyproject_path)
        .map_err(|e| format!("Failed to read pyproject.toml: {}", e))?;

    let file: PyProjectFile =
        toml::from_str(&content).map_err(|e| format!("Failed to parse pyproject.toml: {}", e))?;

    Ok(file.project.unwrap_or_default())
}

impl Default for PythonManifest {
    fn default() -> Self {
        Self {
            name: None,
            version: None,
            requires_python: None,
            dependencies: None,
            optional_dependencies: None,
        }
    }
}

pub fn get_python_version_requirement(project_path: &Path) -> Option<String> {
    read_pyproject(project_path).ok()?.requires_python
}

pub fn get_declared_dependencies(project_path: &Path) -> Result<Vec<String>, String> {
    let manifest = read_pyproject(project_path)?;
    Ok(manifest.dependencies.unwrap_or_default())
}

pub fn extract_version_hint(version_spec: &str) -> String {
    let re = regex::Regex::new(r"(\d+)\.(\d+)")
        .expect("hardcoded regex pattern is valid");
    if let Some(cap) = re.captures(version_spec) {
        format!("{}.{}", &cap[1], &cap[2])
    } else {
        version_spec.to_string()
    }
}

pub fn update_python_version(project_path: &Path, version: &str) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found".to_string());
    }

    let mut manifest = read_pyproject(project_path)?;
    manifest.requires_python = Some(format!(">={}", version));

    let file = PyProjectFile {
        project: Some(manifest),
    };

    let content =
        toml::to_string(&file).map_err(|e| format!("Failed to serialize pyproject.toml: {}", e))?;

    std::fs::write(&pyproject_path, content)
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

    Ok(())
}

pub fn update_dependencies(project_path: &Path, dependencies: Vec<String>) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found".to_string());
    }

    let mut manifest = read_pyproject(project_path)?;
    manifest.dependencies = Some(dependencies);

    let file = PyProjectFile {
        project: Some(manifest),
    };

    let content =
        toml::to_string(&file).map_err(|e| format!("Failed to serialize pyproject.toml: {}", e))?;

    std::fs::write(&pyproject_path, content)
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

    Ok(())
}

fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn update_project_metadata(
    project_path: &Path,
    name: Option<String>,
    version: Option<String>,
) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found".to_string());
    }

    let mut manifest = read_pyproject(project_path)?;

    if let Some(name_value) = name {
        manifest.name = Some(slugify(&name_value));
    }
    if let Some(version_value) = version {
        manifest.version = Some(version_value);
    }

    let file = PyProjectFile {
        project: Some(manifest),
    };

    let content =
        toml::to_string(&file).map_err(|e| format!("Failed to serialize pyproject.toml: {}", e))?;

    std::fs::write(&pyproject_path, content)
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

    Ok(())
}

pub fn create_pyproject(project_path: &Path) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if pyproject_path.exists() {
        return Ok(());
    }

    let manifest = PythonManifest {
        name: Some("procedure".to_string()),
        version: Some("0.1.0".to_string()),
        requires_python: Some(">=3.11".to_string()),
        dependencies: Some(vec![]),
        optional_dependencies: None,
    };

    let file = PyProjectFile {
        project: Some(manifest),
    };

    let content =
        toml::to_string(&file).map_err(|e| format!("Failed to serialize pyproject.toml: {}", e))?;

    std::fs::write(&pyproject_path, content)
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

    Ok(())
}
