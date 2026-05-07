use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Type)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    FileNotFound,
    InvalidFileExtension,
    YamlParseError,
    JsonParseError,
    ValidationError,
    IoError,
    SerializationError,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn file_not_found(path: impl std::fmt::Display) -> Self {
        Self::new(
            ErrorCode::FileNotFound,
            format!("File not found: {}", path),
        )
    }

    pub fn yaml_parse_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::YamlParseError, format!("Failed to parse YAML: {}", err))
    }

    pub fn json_parse_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::JsonParseError, format!("Failed to parse JSON: {}", err))
    }

    pub fn validation_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::ValidationError, format!("Validation error: {}", err))
    }

    pub fn io_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::IoError, err.to_string())
    }

    pub fn serialization_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::SerializationError, format!("Serialization error: {}", err))
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(r#"{{"code":"serialization-error","message":"Failed to serialize error"}}"#)
        })
    }
}

impl From<CommandError> for String {
    fn from(error: CommandError) -> Self {
        error.to_json_string()
    }
}
