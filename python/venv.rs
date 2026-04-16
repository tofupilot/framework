//! Venv operations: inspect, sync, delete, auto-resolve Python executable.

use super::version_constraint::{compute_effective_constraint, parse_requires_python, ConstraintResult, RequiresPythonGuard, STUDIO_MIN_VERSION};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tauri_specta::Event;
use toml_edit::{DocumentMut, Item, Table};

const STUDIO_DEPENDENCIES: &[&str] = &[];

#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct PythonInstallOutputEvent(pub String);

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct VenvPackage {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct PythonState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venv_path: Option<String>,
    pub declared_dependencies: Vec<String>,
    pub installed_packages: Vec<VenvPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pyproject_content: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    dependencies: Vec<String>,
}

fn read_pyproject(project_path: &Path) -> Option<Manifest> {
    let content = std::fs::read_to_string(project_path.join("pyproject.toml")).ok()?;
    let table: toml::Table = toml::from_str(&content).ok()?;
    let project_section = table.get("project")?;
    toml::from_str(&toml::to_string(project_section).ok()?).ok()
}

#[cfg(test)]
pub(crate) fn get_dependencies(project_path: &Path) -> Vec<String> {
    read_pyproject(project_path)
        .map(|m| m.dependencies)
        .unwrap_or_default()
}

#[cfg(not(test))]
fn get_dependencies(project_path: &Path) -> Vec<String> {
    read_pyproject(project_path)
        .map(|m| m.dependencies)
        .unwrap_or_default()
}

fn build_studio_deps_array() -> toml_edit::Array {
    let mut arr = toml_edit::Array::new();
    for name in STUDIO_DEPENDENCIES {
        arr.push(*name);
    }
    arr
}

#[cfg(test)]
pub(crate) fn ensure_manifest_with_studio_deps(project_path: &Path) -> Result<(), String> {
    ensure_manifest_with_studio_deps_impl(project_path)
}

#[cfg(not(test))]
fn ensure_manifest_with_studio_deps(project_path: &Path) -> Result<(), String> {
    ensure_manifest_with_studio_deps_impl(project_path)
}

fn ensure_manifest_with_studio_deps_impl(project_path: &Path) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");

    let content = if pyproject_path.exists() {
        std::fs::read_to_string(&pyproject_path)
            .map_err(|e| format!("Failed to read pyproject.toml: {}", e))?
    } else {
        "[project]\nname = \"procedure\"\nversion = \"0.1.0\"\nrequires-python = \">=3.9\"\ndependencies = []\n".to_string()
    };

    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| format!("Failed to parse pyproject.toml: {}", e))?;

    let studio_deps = build_studio_deps_array();

    // Always update the studio group to ensure correct dependencies
    if let Some(dg) = doc.get_mut("dependency-groups") {
        if let Some(table) = dg.as_table_mut() {
            table.insert("studio", Item::Value(toml_edit::Value::Array(studio_deps)));
        }
    } else {
        let mut dg_table = Table::new();
        dg_table.insert("studio", Item::Value(toml_edit::Value::Array(studio_deps)));
        doc.insert("dependency-groups", Item::Table(dg_table));
    }

    std::fs::write(&pyproject_path, doc.to_string())
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

    log::info!("Updated [dependency-groups].studio in pyproject.toml");
    Ok(())
}


#[cfg(test)]
pub(crate) fn find_python_executable(project_path: &Path) -> Option<PathBuf> {
    find_python_executable_impl(project_path)
}

#[cfg(not(test))]
fn find_python_executable(project_path: &Path) -> Option<PathBuf> {
    find_python_executable_impl(project_path)
}

fn find_python_executable_impl(project_path: &Path) -> Option<PathBuf> {
    for venv_name in [".venv", "venv"] {
        let venv_path = project_path.join(venv_name);
        if !venv_path.exists() || !venv_path.is_dir() {
            continue;
        }

        let python_path = if cfg!(target_os = "windows") {
            venv_path.join("Scripts").join("python.exe")
        } else {
            venv_path.join("bin").join("python")
        };

        if python_path.exists() {
            log::debug!("Found Python at: {}", python_path.display());
            return Some(python_path);
        }
    }
    log::debug!("No Python executable found in: {}", project_path.display());
    None
}

struct VenvInfo {
    python_path: PathBuf,
    venv_path: PathBuf,
}

async fn inspect_venv(
    app: &AppHandle,
    venv_info: &VenvInfo,
    procedure_dir: &str,
) -> (Option<String>, Vec<VenvPackage>) {
    let version = async {
        let output = crate::execution::runtime::python::PythonCommandBuilderSync::new(
            &venv_info.python_path.to_string_lossy(),
        )
        .arg("--version")
        .output()
        .ok()?;

        Some(
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .strip_prefix("Python ")
                .unwrap_or("")
                .to_string(),
        )
    }
    .await;

    let packages = async {
        let sidecar = app.shell().sidecar("uv").ok()?;

        let output = sidecar
            .args(&["pip", "list", "--format=json"])
            .env("VIRTUAL_ENV", venv_info.venv_path.to_string_lossy().as_ref())
            .current_dir(procedure_dir)
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            log::warn!("UV pip list failed: {}", String::from_utf8_lossy(&output.stderr));
            return None;
        }

        let packages: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).ok()?;

        Some(
            packages
                .iter()
                .filter_map(|p| {
                    Some(VenvPackage {
                        name: p.get("name")?.as_str()?.to_string(),
                        version: p.get("version")?.as_str()?.to_string(),
                    })
                })
                .collect(),
        )
    }
    .await;

    (version, packages.unwrap_or_default())
}

#[tauri::command]
#[specta::specta]
pub async fn get_python_state(
    app: AppHandle,
    procedure_dir: String,
) -> Result<PythonState, String> {
    log::debug!("Getting Python state for: {}", procedure_dir);
    let path = Path::new(&procedure_dir);

    let pyproject_content = std::fs::read_to_string(path.join("pyproject.toml")).ok();
    let declared_dependencies = get_dependencies(path);

    let venv_info = find_python_executable(path).and_then(|python_path| {
        let venv_path = python_path.parent()?.parent()?.to_path_buf();
        Some(VenvInfo {
            python_path,
            venv_path,
        })
    });

    let (python_path, python_version, venv_path, installed_packages) = match venv_info {
        Some(info) => {
            let (version, packages) = inspect_venv(&app, &info, &procedure_dir).await;
            (
                Some(info.python_path.to_string_lossy().to_string()),
                version,
                Some(info.venv_path.to_string_lossy().to_string()),
                packages,
            )
        }
        None => (None, None, None, vec![]),
    };

    Ok(PythonState {
        python_path,
        python_version,
        venv_path,
        declared_dependencies,
        installed_packages,
        pyproject_content,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn sync_python(app: AppHandle, procedure_dir: String) -> Result<(), String> {
    let path = Path::new(&procedure_dir);

    if !path.join("pyproject.toml").exists() {
        return Err("pyproject.toml not found. Please create one first.".to_string());
    }

    let venv_name = find_python_executable(path)
        .and_then(|p| p.parent()?.parent()?.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| ".venv".to_string());

    log::info!("Syncing dependencies for venv: {}", venv_name);

    ensure_manifest_with_studio_deps(path)?;

    let user_constraint = parse_requires_python(path);
    let effective_constraint = match compute_effective_constraint(user_constraint.as_deref()) {
        ConstraintResult::Effective(c) => c,
        ConstraintResult::Incompatible { user_constraint } => {
            return Err(format!(
                "TofuPilot Studio requires Python >={}.{}, but your project specifies '{}' which has no compatible versions.",
                STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1, user_constraint
            ));
        }
    };

    log::info!("Using effective Python constraint: {}", effective_constraint);

    // Temporarily update requires-python for UV resolution (restored on drop)
    let _guard = RequiresPythonGuard::new(path, &effective_constraint)?;

    let (mut rx, _child) = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV sidecar not found: {}", e))?
        .args(&["sync", "--group", "studio", "--python", &effective_constraint])
        .env("UV_PROJECT_ENVIRONMENT", &venv_name)
        .current_dir(path)
        .spawn()
        .map_err(|e| format!("Failed to spawn UV sync: {}", e))?;

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                let line_str = String::from_utf8_lossy(&line);
                log::info!("UV: {}", line_str);
                PythonInstallOutputEvent(line_str.to_string())
                    .emit(&app)
                    .map_err(|e| format!("Failed to emit progress: {}", e))?;
            }
            CommandEvent::Terminated(_) => {
                log::info!("UV sync completed");
                break;
            }
            _ => {}
        }
    }

    // _guard is dropped here, restoring original pyproject.toml

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn delete_venv(procedure_dir: String) -> Result<(), String> {
    let path = Path::new(&procedure_dir);

    for venv_name in [".venv", "venv"] {
        let venv_path = path.join(venv_name);
        if venv_path.exists() {
            std::fs::remove_dir_all(&venv_path)
                .map_err(|e| format!("Failed to delete venv: {}", e))?;
            log::info!("Deleted venv at: {}", venv_path.display());
            return Ok(());
        }
    }

    Ok(())
}

pub async fn resolve_python_internal(
    app: Option<&AppHandle>,
    project_path: &Path,
) -> Result<String, String> {
    ensure_manifest_with_studio_deps(project_path)?;

    let user_constraint = parse_requires_python(project_path);
    let effective_constraint = match compute_effective_constraint(user_constraint.as_deref()) {
        ConstraintResult::Effective(c) => c,
        ConstraintResult::Incompatible { user_constraint } => {
            return Err(format!(
                "TofuPilot Studio requires Python >={}.{}, but your project specifies '{}' which has no compatible versions.",
                STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1, user_constraint
            ));
        }
    };

    log::info!("Running uv sync --group studio --python {}", effective_constraint);

    // Temporarily update requires-python for UV resolution (restored on drop)
    let _guard = RequiresPythonGuard::new(project_path, &effective_constraint)?;

    let (success, stdout, stderr) = if let Some(app_handle) = app {
        // GUI mode: use Tauri sidecar
        let output = app_handle
            .shell()
            .sidecar("uv")
            .map_err(|e| format!("UV binary not found: {}", e))?
            .args(&["sync", "--group", "studio", "--python", &effective_constraint])
            .env("UV_PROJECT_ENVIRONMENT", ".venv")
            .current_dir(project_path)
            .output()
            .await
            .map_err(|e| format!("Failed to run uv sync: {}", e))?;
        (output.status.success(), output.stdout, output.stderr)
    } else {
        // CLI mode: use std::process::Command
        let output = std::process::Command::new("uv")
            .args(["sync", "--group", "studio", "--python", &effective_constraint])
            .env("UV_PROJECT_ENVIRONMENT", ".venv")
            .current_dir(project_path)
            .output()
            .map_err(|e| format!("Failed to run uv sync: {}", e))?;
        (output.status.success(), output.stdout, output.stderr)
    };

    // _guard is dropped here, restoring original pyproject.toml

    if !success {
        let stderr = String::from_utf8_lossy(&stderr);
        let stdout = String::from_utf8_lossy(&stdout);
        return Err(format!(
            "uv sync --group studio failed:\nStderr: {}\nStdout: {}",
            stderr.trim(),
            stdout.trim()
        ));
    }

    log::info!("uv sync completed successfully");

    let python_path = find_python_executable(project_path)
        .ok_or_else(|| "Python executable not found after uv sync".to_string())?;

    log::info!(
        "Python ready at: {}",
        python_path.display()
    );
    Ok(python_path.to_string_lossy().to_string())
}

