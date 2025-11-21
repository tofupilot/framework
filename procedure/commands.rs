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
