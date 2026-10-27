use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
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
        Self::new(ErrorCode::FileNotFound, format!("{}: not found", path))
    }

    pub fn yaml_parse_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::YamlParseError, err.to_string())
    }

    pub fn json_parse_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::JsonParseError, err.to_string())
    }

    pub fn validation_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::ValidationError, err.to_string())
    }

    pub fn io_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::IoError, err.to_string())
    }

    pub fn serialization_error(err: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::SerializationError, err.to_string())
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"code":"serialization-error","message":"Failed to serialize error"}"#.to_string()
        })
    }
}

impl From<CommandError> for String {
    fn from(error: CommandError) -> Self {
        error.to_json_string()
    }
}
