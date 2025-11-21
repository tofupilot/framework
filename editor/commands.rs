//! Editor commands for YAML watching, validation, and settings management.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::procedure::schema;

static WATCHED_YAML_FILES: OnceLock<Mutex<HashMap<String, AbortHandle>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct YamlFileChangedEvent(pub String);

#[derive(Debug, Clone, Serialize, Event, Type)]
pub struct YamlFileDeletedEvent(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct EditorSettings {
    pub sequential_dependencies: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            sequential_dependencies: true,
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn watch_yaml_file(path: String, app_handle: AppHandle) -> Result<(), String> {
    use std::fs;
    use tokio::time::{interval, Duration};

    let watched = WATCHED_YAML_FILES.get_or_init(|| Mutex::new(HashMap::new()));

    let mut files = watched.lock().await;
    if files.contains_key(&path) {
        return Ok(());
    }

    log::debug!("Setting up YAML file watcher for: {}", path);

    let path_clone = path.clone();

    let initial_modified = fs::metadata(&path)
        .map_err(|e| format!("Failed to get file metadata: {}", e))?
        .modified()
        .map_err(|e| format!("Failed to get modification time: {}", e))?;

    let handle = tokio::spawn(async move {
        let mut last_modified = initial_modified;
        let mut ticker = interval(Duration::from_secs(1));

        loop {
            ticker.tick().await;

            match fs::metadata(&path_clone) {
                Ok(metadata) => {
                    if let Ok(modified) = metadata.modified() {
                        if modified != last_modified {
                            log::trace!(
                                "YAML file changed: {}",
                                path_clone
                            );
                            last_modified = modified;
                            let _ = YamlFileChangedEvent(path_clone.clone()).emit(&app_handle);
                        }
                    }
                }
                Err(e) => {
                    log::info!(
                        "YAML file no longer exists: {} (error: {})",
                        path_clone, e
                    );
                    let _ = YamlFileDeletedEvent(path_clone.clone()).emit(&app_handle);

                    if let Some(watched) = WATCHED_YAML_FILES.get() {
                        let mut files = watched.lock().await;
                        files.remove(&path_clone);
                    }
                    break;
                }
            }
        }
    });

    files.insert(path, handle.abort_handle());
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn unwatch_yaml_file(path: String) -> Result<(), String> {
    log::debug!("Removing YAML file watcher for: {}", path);

    if let Some(watched) = WATCHED_YAML_FILES.get() {
        let mut files = watched.lock().await;
        if let Some(abort_handle) = files.remove(&path) {
            abort_handle.abort();
            log::debug!("Aborted file watcher for: {}", path);
        }
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn load_editor_settings(app: AppHandle) -> Result<EditorSettings, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config directory: {}", e))?;
    let settings_path = config_dir.join("editor_settings.json");

    if !settings_path.exists() {
        return Ok(EditorSettings::default());
    }

    let contents = tokio::fs::read_to_string(&settings_path)
        .await
        .map_err(|e| format!("Failed to read editor settings: {}", e))?;

    serde_json::from_str(&contents).map_err(|e| format!("Failed to parse editor settings: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn save_editor_settings(app: AppHandle, settings: EditorSettings) -> Result<(), String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config directory: {}", e))?;

    tokio::fs::create_dir_all(&config_dir)
        .await
        .map_err(|e| format!("Failed to create config directory: {}", e))?;

    let settings_path = config_dir.join("editor_settings.json");
    let json = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Failed to serialize editor settings: {}", e))?;

    tokio::fs::write(&settings_path, json)
        .await
        .map_err(|e| format!("Failed to write editor settings: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn save_yaml_config(procedure_file: String, config_json: String) -> Result<String, String> {
    use std::fs;
    use validator::Validate;

    // Deserialize from JSON (Runtime types with required keys)
    let runtime_config: schema::ProcedureDefinition =
        serde_json::from_str(&config_json).map_err(|e| format!("Invalid JSON format: {}", e))?;

    log::debug!(
        "Saving YAML config: {} phases (setup: {}, main: {}, teardown: {})",
        runtime_config.setup.len() + runtime_config.main.len() + runtime_config.teardown.len(),
        runtime_config.setup.len(),
        runtime_config.main.len(),
        runtime_config.teardown.len()
    );

    // Validate (includes Python identifier validation)
    runtime_config
        .validate()
        .map_err(|e| format!("Validation failed: {}", e))?;

    // Convert to YAML types (strips auto-generated keys)
    let yaml_config = runtime_config.to_yaml();

    // Debug: Log execution config
    if let Some(ref exec) = yaml_config.execution {
        log::info!("Saving execution config - strategy: {:?}, workers: {}, on_first_failure: {:?}, slots: {}",
            exec.strategy, exec.workers, exec.on_first_failure, exec.slots.len());
    }

    // Serialize to YAML
    let mut yaml_content = serde_yaml::to_string(&yaml_config)
        .map_err(|e| format!("Failed to serialize YAML: {}", e))?;

    // Post-process YAML string
    yaml_content = yaml_content
        .replace("expected_value: 'true'", "expected_value: true")
        .replace("expected_value: 'false'", "expected_value: false")
        .replace("expected_value: \"true\"", "expected_value: true")
        .replace("expected_value: \"false\"", "expected_value: false");

    let sections = ["execution", "unit", "plugs", "setup", "main", "phases", "teardown"];
    for section in sections {
        yaml_content = yaml_content.replace(&format!("\n{}:", section), &format!("\n\n{}:", section));
    }

    // Write to file
    fs::write(&procedure_file, yaml_content).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok("Configuration saved successfully".to_string())
}
