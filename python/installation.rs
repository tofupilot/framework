use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

pub async fn list_uv_pythons(app: &AppHandle) -> Result<Vec<String>, String> {
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

    let stdout = String::from_utf8_lossy(&output.stdout);

    let versions: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }

            if line.starts_with("cpython-") {
                line.strip_prefix("cpython-")
                    .and_then(|s| s.split_whitespace().next())
                    .map(|v| v.to_string())
            } else {
                line.split_whitespace().next().map(|v| v.to_string())
            }
        })
        .collect();

    log::debug!("Found {} UV-managed Python versions: {:?}", versions.len(), versions);
    Ok(versions)
}

pub async fn install_uv_python(app: &AppHandle, version: &str) -> Result<String, String> {
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

fn version_matches(installed: &str, required: &str) -> bool {
    if installed == required {
        return true;
    }

    if installed.starts_with(&format!("{}.", required)) {
        return true;
    }

    let installed_parts: Vec<&str> = installed.split('.').collect();
    let required_parts: Vec<&str> = required.split('.').collect();

    for (i, req_part) in required_parts.iter().enumerate() {
        if let Some(inst_part) = installed_parts.get(i) {
            if inst_part != req_part {
                return false;
            }
        } else {
            return false;
        }
    }

    true
}

pub async fn ensure_python_available(
    app: &AppHandle,
    version: &str,
) -> Result<String, String> {
    log::info!("Ensuring Python {} is available via UV", version);

    let installed_versions = list_uv_pythons(app).await?;

    for installed in &installed_versions {
        if version_matches(installed, version) {
            log::debug!("Python {} already installed (found {})", version, installed);
            return Ok(installed.clone());
        }
    }

    log::info!("Python {} not found, installing via UV", version);
    install_uv_python(app, version).await?;

    let updated_versions = list_uv_pythons(app).await?;
    for installed in &updated_versions {
        if version_matches(installed, version) {
            return Ok(installed.clone());
        }
    }

    Err(format!(
        "Failed to verify Python {} installation after install",
        version
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_matches() {
        assert!(version_matches("3.11", "3.11"));
        assert!(version_matches("3.11.5", "3.11"));
        assert!(version_matches("3.11.5", "3.11.5"));
        assert!(!version_matches("3.10.5", "3.11"));
        assert!(!version_matches("3.11", "3.11.5"));
    }
}
