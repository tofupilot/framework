//! Venv operations: inspect, sync, delete, auto-resolve Python executable.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use tauri_specta::Event;

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
    #[serde(default, rename = "requires-python")]
    requires_python: Option<String>,
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

pub(crate) fn get_python_version_requirement(project_path: &Path) -> Option<String> {
    read_pyproject(project_path)?.requires_python
}

async fn install_worker_dependencies(app: &AppHandle, project_path: &Path) -> Result<(), String> {
    log::info!("Installing worker dependencies via uv pip install");

    let venv_info = find_python_executable(project_path).and_then(|python_path| {
        let venv_path = python_path.parent()?.parent()?.to_path_buf();
        Some(venv_path)
    }).ok_or("No venv found")?;

    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV binary not found: {}", e))?
        .args(&["pip", "install", "grpcio", "portpicker", "protobuf"])
        .env("VIRTUAL_ENV", venv_info.to_string_lossy().as_ref())
        .current_dir(project_path)
        .output()
        .await
        .map_err(|e| format!("Failed to install worker dependencies: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("UV pip install failed: {}", stderr));
    }

    log::info!("Worker dependencies installed successfully");
    Ok(())
}

#[cfg(test)]
pub(crate) fn create_manifest(project_path: &Path) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if pyproject_path.exists() {
        return Ok(());
    }

    std::fs::write(&pyproject_path, "[project]\nname = \"procedure\"\nversion = \"0.1.0\"\nrequires-python = \">=3.11\"\ndependencies = []\n")
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))
}

#[cfg(not(test))]
fn create_manifest(project_path: &Path) -> Result<(), String> {
    let pyproject_path = project_path.join("pyproject.toml");
    if pyproject_path.exists() {
        return Ok(());
    }

    std::fs::write(&pyproject_path, "[project]\nname = \"procedure\"\nversion = \"0.1.0\"\nrequires-python = \">=3.11\"\ndependencies = []\n")
        .map_err(|e| format!("Failed to write pyproject.toml: {}", e))
}

#[cfg(test)]
pub(crate) fn version_matches(installed: &str, required: &str) -> bool {
    version_matches_impl(installed, required)
}

#[cfg(not(test))]
fn version_matches(installed: &str, required: &str) -> bool {
    version_matches_impl(installed, required)
}

fn version_matches_impl(installed: &str, required: &str) -> bool {
    if installed == required {
        return true;
    }

    if installed.starts_with(&format!("{}.", required)) {
        return true;
    }

    let installed_parts: Vec<&str> = installed.split('.').collect();
    let required_parts: Vec<&str> = required.split('.').collect();

    required_parts.iter().enumerate().all(|(i, req_part)| {
        installed_parts.get(i).map_or(false, |inst_part| inst_part == req_part)
    })
}

async fn list_uv_pythons(app: &AppHandle) -> Result<Vec<String>, String> {
    log::debug!("Listing UV-managed Python installations");

    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV binary not found: {}", e))?
        .args(["python", "list"])
        .output()
        .await
        .map_err(|e| format!("Failed to list UV Pythons: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("UV python list failed: {}", stderr));
    }

    let versions: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }

            line.strip_prefix("cpython-")
                .or(Some(line))
                .and_then(|s| s.split_whitespace().next())
                .map(String::from)
        })
        .collect();

    log::debug!("Found {} UV-managed Python versions: {:?}", versions.len(), versions);
    Ok(versions)
}

async fn install_uv_python(app: &AppHandle, version: &str) -> Result<String, String> {
    log::info!("Installing Python {} via UV", version);

    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV binary not found: {}", e))?
        .args(["python", "install", version])
        .output()
        .await
        .map_err(|e| format!("Failed to install Python {}: {}", version, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "UV python install {} failed:\nStderr: {}\nStdout: {}",
            version, stderr, stdout
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    log::info!("Successfully installed Python {}: {}", version, stdout.trim());
    Ok(stdout.trim().to_string())
}

async fn ensure_python_available(app: &AppHandle, version: &str) -> Result<String, String> {
    log::info!("Ensuring Python {} is available via UV", version);

    let installed_versions = list_uv_pythons(app).await?;

    if let Some(installed) = installed_versions.iter().find(|v| version_matches(v, version)) {
        log::debug!("Python {} already installed (found {})", version, installed);
        return Ok(installed.clone());
    }

    log::info!("Python {} not found, installing via UV", version);
    install_uv_python(app, version).await?;

    let updated_versions = list_uv_pythons(app).await?;
    updated_versions
        .iter()
        .find(|v| version_matches(v, version))
        .cloned()
        .ok_or_else(|| format!("Failed to verify Python {} installation after install", version))
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
        .hide_window()
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

    let (mut rx, _child) = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV sidecar not found: {}", e))?
        .args(&["sync"])
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
    if let Some(python_path) = find_python_executable(project_path) {
        log::info!("Using existing venv Python: {}", python_path.display());

        if let Some(app_handle) = app {
            install_worker_dependencies(app_handle, project_path).await.ok();
        }

        return Ok(python_path.to_string_lossy().to_string());
    }

    let app_handle = app.ok_or_else(|| {
        "Python resolution requires app handle for UV operations.\n\
         This is an internal error - please report this issue."
            .to_string()
    })?;

    create_manifest(project_path).ok();

    let version_requirement = get_python_version_requirement(project_path);
    let version = version_requirement
        .as_ref()
        .map(|spec| {
            spec.chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<String>()
                .split('.')
                .take(2)
                .collect::<Vec<_>>()
                .join(".")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "3.11".to_string());

    log::info!("No venv found. Installing Python {} via UV", version);
    if let Some(ref req) = version_requirement {
        log::info!("pyproject.toml requires-python: {}", req);
    }

    ensure_python_available(app_handle, &version)
        .await
        .map_err(|e| format!("Failed to install Python {}: {}", version, e))?;

    log::info!("Python {} available. Creating venv with `uv sync`", version);

    let venv_name = ".venv";
    let (mut rx, _child) = app_handle
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("UV sidecar not found: {}", e))?
        .args(&["sync"])
        .env("UV_PROJECT_ENVIRONMENT", venv_name)
        .current_dir(project_path)
        .spawn()
        .map_err(|e| format!("Failed to spawn UV sync: {}", e))?;

    while let Some(event) = rx.recv().await {
        if let CommandEvent::Stdout(line) | CommandEvent::Stderr(line) = event {
            log::info!("UV: {}", String::from_utf8_lossy(&line));
        } else if matches!(event, CommandEvent::Terminated(_)) {
            log::info!("UV sync completed");
            break;
        }
    }

    install_worker_dependencies(app_handle, project_path).await?;

    let python_path = find_python_executable(project_path)
        .ok_or_else(|| "Python executable not found after uv sync".to_string())?;

    log::info!("Successfully created venv at: {}", python_path.parent().unwrap().parent().unwrap().display());
    Ok(python_path.to_string_lossy().to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn resolve_python(app: AppHandle, procedure_dir: String) -> Result<String, String> {
    resolve_python_internal(Some(&app), Path::new(&procedure_dir)).await
}
