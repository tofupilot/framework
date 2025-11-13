use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};
use ts_rs::TS;

/// Centralized context for a procedure that holds both the YAML file path
/// and the derived project directory. This ensures consistency across the application.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureContext {
    /// Full path to the procedure YAML file (e.g., /path/to/project/procedure.yaml)
    pub procedure_file: String,

    /// Directory containing the procedure YAML file (e.g., /path/to/project)
    pub procedure_dir: String,
}

impl ProcedureContext {
    /// Create a new ProcedureContext from a procedure file path
    pub fn from_procedure_file(procedure_file: impl AsRef<Path>) -> Result<Self, String> {
        let procedure_file_buf = procedure_file.as_ref().to_path_buf();

        // Validate it's a YAML file
        if !procedure_file_buf.exists() {
            return Err("YAML file does not exist".to_string());
        }

        let extension = procedure_file_buf.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if extension != "yaml" && extension != "yml" {
            return Err("File must be a YAML file (.yaml or .yml)".to_string());
        }

        // Get procedure directory (parent of YAML file)
        let procedure_dir = procedure_file_buf.parent()
            .ok_or_else(|| "Invalid procedure file path: no parent directory".to_string())?
            .to_path_buf();

        Ok(Self {
            procedure_file: procedure_file_buf.to_string_lossy().to_string(),
            procedure_dir: procedure_dir.to_string_lossy().to_string(),
        })
    }

    /// Get the procedure file path as a PathBuf
    pub fn procedure_file_buf(&self) -> PathBuf {
        PathBuf::from(&self.procedure_file)
    }

    /// Get the procedure directory as a PathBuf
    pub fn procedure_dir_buf(&self) -> PathBuf {
        PathBuf::from(&self.procedure_dir)
    }
}
