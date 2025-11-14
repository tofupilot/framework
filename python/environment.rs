use std::path::Path;
use std::process::Command as StdCommand;
use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use super::types::{VenvInfo, VenvPackage};
use super::utils::{find_venv_path, get_venv_python_path};

#[tauri::command]
pub async fn get_venv_info(app: AppHandle, directory: String) -> Result<VenvInfo, String> {
    let path = Path::new(&directory);

    let venv_path = match find_venv_path(path) {
        Some(p) => p,
        None => {
            return Ok(VenvInfo {
                exists: false,
                path: None,
                python_path: None,
                python_version: None,
                packages: vec![],
            })
        }
    };

    let venv_python = get_venv_python_path(&venv_path);

    if venv_python.exists() {
        let mut cmd = StdCommand::new(&venv_python);
        cmd.arg("--version")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH");

        crate::utils::configure_no_window(&mut cmd);

        let version_output = cmd
            .output()
            .map_err(|e| format!("Failed to get Python version: {}", e))?;

        let python_version = String::from_utf8_lossy(&version_output.stdout)
            .trim()
            .replace("Python ", "")
            .to_string();

        let packages_output = app
            .shell()
            .sidecar("uv")
            .map_err(|e| format!("Failed to get UV sidecar: {}", e))?
            .args(&["pip", "list", "--format=json"])
            .env("VIRTUAL_ENV", venv_path.to_string_lossy().as_ref())
            .current_dir(&directory)
            .output()
            .await
            .map_err(|e| format!("Failed to list packages: {}", e))?;

        if packages_output.status.success() {
            let packages: Vec<serde_json::Value> = serde_json::from_slice(&packages_output.stdout)
                .map_err(|e| format!("Failed to parse package list: {}", e))?;

            let package_names: Vec<String> = packages
                .iter()
                .take(20)
                .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect();

            Ok(VenvInfo {
                exists: true,
                path: Some(venv_path.to_string_lossy().to_string()),
                python_path: Some(venv_python.to_string_lossy().to_string()),
                python_version: Some(python_version),
                packages: package_names,
            })
        } else {
            Ok(VenvInfo {
                exists: true,
                path: Some(venv_path.to_string_lossy().to_string()),
                python_path: Some(venv_python.to_string_lossy().to_string()),
                python_version: None,
                packages: vec![],
            })
        }
    } else {
        Ok(VenvInfo {
            exists: false,
            path: None,
            python_path: None,
            python_version: None,
            packages: vec![],
        })
    }
}

#[tauri::command]
pub async fn check_venv_packages(
    app: AppHandle,
    venv_path: String,
) -> Result<Vec<VenvPackage>, String> {
    let venv_python = get_venv_python_path(Path::new(&venv_path));

    if !venv_python.exists() {
        return Err(format!("Virtual environment not found: {}", venv_path));
    }

    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("Failed to get UV sidecar: {}", e))?
        .args(&["pip", "list", "--format=json"])
        .env("VIRTUAL_ENV", &venv_path)
        .output()
        .await
        .map_err(|e| format!("Failed to list packages: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Package listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let packages: Vec<VenvPackage> = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse package list: {}", e))?;

    Ok(packages)
}

#[tauri::command]
pub async fn create_virtual_environment(
    app: AppHandle,
    procedure_dir: String,
    python_version: Option<String>,
) -> Result<(), String> {
    let version_to_use = python_version.or_else(|| {
        let procedure_dir_buf = Path::new(&procedure_dir);
        crate::python::manifest::get_python_version_requirement(procedure_dir_buf)
            .map(|v| crate::python::manifest::extract_version_hint(&v))
    });

    let mut command = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("Failed to get UV sidecar: {}", e))?
        .args(&["venv", ".venv"])
        .current_dir(&procedure_dir);

    if let Some(version) = version_to_use {
        log::info!("Creating venv with Python version: {}", version);
        command = command.args(&["--python", &version]);
    }

    let output = command
        .output()
        .await
        .map_err(|e| format!("Failed to create virtual environment: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Virtual environment creation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    log::info!("Virtual environment created successfully with UV");
    Ok(())
}

#[tauri::command]
pub async fn sync_pyproject_dependencies(
    app: AppHandle,
    project_path: String,
) -> Result<String, String> {
    let pyproject_path = Path::new(&project_path).join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found. Please create one first.".to_string());
    }

    let project_path_buf = Path::new(&project_path);

    let venv_name = if let Some(venv_path) = find_venv_path(project_path_buf) {
        let name = venv_path
            .file_name()
            .ok_or_else(|| "Invalid venv path".to_string())?
            .to_string_lossy()
            .to_string();
        log::info!("Using existing virtual environment: {}", name);
        name
    } else {
        log::info!("No virtual environment found, uv sync will create .venv automatically");
        ".venv".to_string()
    };

    log::info!(
        "Syncing dependencies with UV_PROJECT_ENVIRONMENT: {}",
        venv_name
    );

    let output = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("Failed to get UV sidecar: {}", e))?
        .args(&["sync"])
        .env("UV_PROJECT_ENVIRONMENT", &venv_name)
        .current_dir(&project_path)
        .output()
        .await
        .map_err(|e| format!("Failed to sync dependencies: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "UV sync failed with exit code: {}\nStderr: {}\nStdout: {}",
            output.status.code().unwrap_or(-1),
            stderr,
            stdout
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[tauri::command]
pub async fn manual_sync_pyproject_dependencies(
    app: AppHandle,
    project_path: String,
) -> Result<(), String> {
    let pyproject_path = Path::new(&project_path).join("pyproject.toml");
    if !pyproject_path.exists() {
        return Err("pyproject.toml not found. Please create one first.".to_string());
    }

    let project_path_buf = Path::new(&project_path);

    let venv_name = if let Some(venv_path) = find_venv_path(project_path_buf) {
        let name = venv_path
            .file_name()
            .ok_or_else(|| "Invalid venv path".to_string())?
            .to_string_lossy()
            .to_string();
        log::info!("Using existing virtual environment: {}", name);
        name
    } else {
        log::info!("No virtual environment found, uv sync will create .venv automatically");
        ".venv".to_string()
    };

    log::info!(
        "Syncing dependencies with progress. Project path: {}, UV_PROJECT_ENVIRONMENT: {}",
        &project_path,
        venv_name
    );

    let (mut rx, _child) = app
        .shell()
        .sidecar("uv")
        .map_err(|e| format!("Failed to get UV sidecar: {}", e))?
        .args(&["sync"])
        .env("UV_PROJECT_ENVIRONMENT", &venv_name)
        .current_dir(&project_path)
        .spawn()
        .map_err(|e| format!("Failed to spawn UV sync: {}", e))?;

    let exit_code: Option<i32> = None;

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(line) => {
                let line_str = String::from_utf8_lossy(&line);
                log::info!("UV stdout: {}", line_str);
                app.emit("python-install-output", line_str.to_string())
                    .map_err(|e| format!("Failed to emit event: {}", e))?;
            }
            CommandEvent::Stderr(line) => {
                let line_str = String::from_utf8_lossy(&line);
                log::info!("UV stderr: {}", line_str);
                app.emit("python-install-output", line_str.to_string())
                    .map_err(|e| format!("Failed to emit event: {}", e))?;
            }
            CommandEvent::Terminated(_) => {
                log::info!("UV sync completed");
                break;
            }
            _ => {}
        }
    }

    if let Some(code) = exit_code {
        if code != 0 {
            return Err(format!("UV sync failed with exit code: {}", code));
        }
    }

    Ok(())
}
