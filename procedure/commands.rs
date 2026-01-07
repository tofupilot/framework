//! Procedure management commands for loading, file operations, and validation.

use std::path::{Path, PathBuf};
use tauri::AppHandle;
use super::error::CommandError;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct LoadProcedureResponse {
    pub procedure_dir: String,
    pub config: crate::procedure::schema::ProcedureDefinition,
    pub validation: crate::validation::ValidationResult,
}

#[tauri::command]
#[specta::specta]
pub async fn file_exists(path: String) -> Result<bool, CommandError> {
    Ok(Path::new(&path).exists())
}

#[tauri::command]
#[specta::specta]
pub async fn load_procedure(
    app_handle: AppHandle,
    procedure_file: String,
) -> Result<LoadProcedureResponse, CommandError> {
    let path = Path::new(&procedure_file);

    if !path.exists() {
        return Err(CommandError::file_not_found(&procedure_file));
    }

    let procedure_dir = path
        .parent()
        .ok_or_else(|| CommandError::io_error("Cannot determine procedure directory"))?
        .to_string_lossy()
        .to_string();

    let yaml_content = std::fs::read_to_string(path)
        .map_err(|e| CommandError::io_error(format!("Failed to read {}: {}", path.display(), e)))?;

    log::info!("[LOAD_PROCEDURE] Loading: {}", procedure_file);
    let (config, validation) = crate::validation::load_and_validate(
        &app_handle,
        &yaml_content,
        &PathBuf::from(&procedure_dir),
    )
    .await;

    log::info!("[LOAD_PROCEDURE] Returning validation: valid={}, diagnostics={}", validation.is_valid, validation.diagnostics.len());

    Ok(LoadProcedureResponse {
        procedure_dir,
        config,
        validation,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn read_procedure_file(procedure_file: String) -> Result<String, CommandError> {
    let path = Path::new(&procedure_file);
    std::fs::read_to_string(path)
        .map_err(|e| CommandError::io_error(format!("Failed to read {}: {}", path.display(), e)))
}

#[tauri::command]
#[specta::specta]
pub async fn write_procedure_file(procedure_file: String, content: String) -> Result<(), CommandError> {
    let path = Path::new(&procedure_file);
    std::fs::write(path, content)
        .map_err(|e| CommandError::io_error(format!("Failed to write {}: {}", path.display(), e)))
}

#[tauri::command]
#[specta::specta]
pub async fn create_directory(path: String) -> Result<(), CommandError> {
    std::fs::create_dir_all(&path)
        .map_err(|e| CommandError::io_error(format!("Failed to create directory {}: {}", path, e)))
}

#[tauri::command]
#[specta::specta]
pub async fn list_yaml_files(directory: String) -> Result<Vec<String>, CommandError> {
    let dir_path = Path::new(&directory);

    if !dir_path.exists() {
        return Ok(vec![]);
    }

    let mut yaml_files = Vec::new();
    collect_yaml_files(dir_path, dir_path, &mut yaml_files)?;
    yaml_files.sort();

    Ok(yaml_files)
}

fn collect_yaml_files(base: &Path, current: &Path, files: &mut Vec<String>) -> Result<(), CommandError> {
    let entries = std::fs::read_dir(current)
        .map_err(|e| CommandError::io_error(format!("Failed to read directory: {}", e)))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == "pending" {
                    continue;
                }
            }
            collect_yaml_files(base, &path, files)?;
        } else if let Some(ext) = path.extension() {
            if ext == "yaml" || ext == "yml" {
                if let Ok(relative) = path.strip_prefix(base) {
                    files.push(relative.to_string_lossy().to_string());
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn list_subdirectories(directory: String) -> Result<Vec<String>, CommandError> {
    let dir_path = Path::new(&directory);

    if !dir_path.exists() {
        return Ok(vec![]);
    }

    let entries = std::fs::read_dir(dir_path)
        .map_err(|e| CommandError::io_error(format!("Failed to read directory: {}", e)))?;

    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path.to_string_lossy().to_string());
        }
    }
    subdirs.sort();

    Ok(subdirs)
}

#[tauri::command]
#[specta::specta]
pub async fn delete_directory(path: String) -> Result<(), CommandError> {
    let dir_path = Path::new(&path);
    if dir_path.exists() {
        std::fs::remove_dir_all(dir_path)
            .map_err(|e| CommandError::io_error(format!("Failed to delete directory {}: {}", path, e)))?;
    }
    Ok(())
}
