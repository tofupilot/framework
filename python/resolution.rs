use std::path::Path;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

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

    let python_version_req = crate::python::manifest::get_python_version_requirement(project_path);

    log::debug!(
        "Searching for Python in project directory: {}",
        project_path.display()
    );

    let mut args = vec!["python", "find"];
    let version_arg;
    if let Some(ref version) = python_version_req {
        log::info!("pyproject.toml requires Python: {}", version);
        let version_hint = crate::python::manifest::extract_version_hint(version);
        version_arg = version_hint;
        args.push(&version_arg);
    }

    let (success, stdout, stderr) = if let Some(app_handle) = app {
        // GUI mode: use Tauri sidecar
        let output = app_handle
            .shell()
            .sidecar("uv")
            .map_err(|e| {
                log::error!("Failed to get UV sidecar: {}", e);
                format!(
                    "UV binary not found: {}\n\n\
                    This is likely a bundling issue. Please ensure the UV binary is included in the application resources.",
                    e
                )
            })?
            .args(&args)
            .current_dir(project_path)
            .output()
            .await
            .map_err(|e| {
                log::error!("Failed to execute 'uv python find': {}", e);
                format!("Failed to execute UV to find Python: {}", e)
            })?;
        (output.status.success(), output.stdout, output.stderr)
    } else {
        // CLI mode: use bundled uv binary
        let uv_binary_name = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "uv-aarch64-apple-darwin"
            } else {
                "uv-x86_64-apple-darwin"
            }
        } else if cfg!(target_os = "windows") {
            "uv-x86_64-pc-windows-msvc.exe"
        } else {
            if cfg!(target_arch = "aarch64") {
                "uv-aarch64-unknown-linux-gnu"
            } else {
                "uv-x86_64-unknown-linux-gnu"
            }
        };

        // Find UV binary next to the executable
        let exe_path = std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;
        let exe_dir = exe_path.parent().ok_or("Failed to get executable directory")?;
        let uv_binary = exe_dir.join(uv_binary_name);

        if !uv_binary.exists() {
            return Err(format!("UV binary not found at {:?}. Expected to find it next to the tofupilot executable.", uv_binary));
        }

        let output = tokio::process::Command::new(&uv_binary)
            .args(&args)
            .current_dir(project_path)
            .output()
            .await
            .map_err(|e| {
                log::error!("Failed to execute bundled UV at {:?}: {}", uv_binary, e);
                format!("Failed to execute UV to find Python: {}. UV binary not found at {:?}", e, uv_binary)
            })?;
        (output.status.success(), output.stdout, output.stderr)
    };

    if !success {
        let stderr = String::from_utf8_lossy(&stderr);
        log::error!("UV python find failed: {}", stderr);
        let version_msg = if let Some(ver) = python_version_req {
            format!("matching version requirement '{}' ", ver)
        } else {
            String::new()
        };
        return Err(format!(
            "UV could not find a Python installation {}.\n\n\
            Error: {}\n\n\
            To fix this:\n\
            1. Check your pyproject.toml requires-python field\n\
            2. Install a compatible Python version from python.org\n\
            3. Or install via UV: uv python install 3.11\n\
            4. Ensure Python is in your PATH",
            version_msg,
            stderr.trim()
        ));
    }

    let python_path = String::from_utf8_lossy(&stdout).trim().to_string();

    if python_path.is_empty() {
        log::error!("UV returned empty Python path");
        return Err("No Python installation found by UV.\n\n\
            To fix this:\n\
            1. Check your pyproject.toml requires-python field\n\
            2. Install Python 3.8 or later from python.org\n\
            3. Or install via UV: uv python install 3.11\n\
            4. Restart the application after installation"
            .to_string());
    }

    log::info!("UV found Python: {}", python_path);
    Ok(python_path)
}

#[tauri::command]
pub async fn resolve_python_for_project(
    app: AppHandle,
    project_path: String,
) -> Result<String, String> {
    resolve_python_executable(Some(&app), Path::new(&project_path)).await
}
