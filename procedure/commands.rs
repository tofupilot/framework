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

    let yaml_config: crate::procedure::schema::ProcedureYaml = serde_yaml::from_str(&yaml_content)
        .map_err(CommandError::yaml_parse_error)?;

    let config: crate::procedure::schema::ProcedureDefinition = yaml_config.into();

    let syntax_validation = crate::validation::validate_yaml_syntax(&yaml_content);
    let validation = if !syntax_validation.is_valid {
        syntax_validation
    } else {
        let procedure_def = crate::procedure::load_procedure_definition(path)
            .map_err(CommandError::validation_error)?;

        crate::validation::validate_procedure_with_yaml(
            &app_handle,
            &procedure_def,
            &yaml_content,
            &PathBuf::from(&procedure_dir),
        )
        .await
    };

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
