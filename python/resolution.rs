use std::path::Path;
use tauri::AppHandle;

use super::environment::create_virtual_environment;
use super::installation::ensure_python_available;
use super::manifest::{create_pyproject, extract_version_hint, get_python_version_requirement};
use super::utils::{find_venv_path, get_venv_python_path};

pub async fn resolve_python_executable(
    app: Option<&AppHandle>,
    project_path: &Path,
) -> Result<String, String> {
    if let Some(venv_path) = find_venv_path(project_path) {
        let venv_python = get_venv_python_path(&venv_path);
        if venv_python.exists() {
            log::info!("Using venv Python: {}", venv_python.display());
            return Ok(venv_python.to_string_lossy().to_string());
        }
    }

    let app_handle = app.ok_or_else(|| {
        "No app handle provided for Python environment management.\n\n\
        Python resolution requires UV to install and manage Python versions.\n\
        This is an internal error - please report this issue."
            .to_string()
    })?;

    create_pyproject(project_path).ok();

    let python_version_req = get_python_version_requirement(project_path);
    let version_hint = python_version_req
        .as_ref()
        .map(|v| extract_version_hint(v))
        .unwrap_or_else(|| "3.11".to_string());

    log::info!(
        "No venv found. Will install Python {} via UV and create venv",
        version_hint
    );

    if let Some(ref version) = python_version_req {
        log::info!("pyproject.toml requires Python: {}", version);
    }

    ensure_python_available(app_handle, &version_hint)
        .await
        .map_err(|e| {
            format!(
                "Failed to ensure Python {} is available via UV: {}\n\n\
                UV could not install Python automatically.\n\
                This may be due to network issues or UV installation problems.",
                version_hint, e
            )
        })?;

    log::info!(
        "Python {} is available via UV, creating virtual environment",
        version_hint
    );

    create_virtual_environment(app_handle, project_path, Some(&version_hint))
        .await
        .map_err(|e| {
            format!(
                "Failed to create virtual environment with Python {}: {}\n\n\
                UV failed to create a virtual environment.\n\
                Please check that you have write permissions in the project directory.",
                version_hint, e
            )
        })?;

    let venv_path = find_venv_path(project_path).ok_or_else(|| {
        "Virtual environment was created but could not be found.\n\n\
        This is an internal error - the venv should exist at .venv or venv in the project directory."
            .to_string()
    })?;

    let venv_python = get_venv_python_path(&venv_path);
    if !venv_python.exists() {
        return Err(format!(
            "Virtual environment Python not found at: {}\n\n\
            The venv was created but the Python executable is missing.\n\
            This may indicate a problem with UV's venv creation.",
            venv_python.display()
        ));
    }

    log::info!(
        "Successfully created venv with Python at: {}",
        venv_python.display()
    );
    Ok(venv_python.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn resolve_python_for_project(
    app: AppHandle,
    project_path: String,
) -> Result<String, String> {
    resolve_python_executable(Some(&app), Path::new(&project_path)).await
}
