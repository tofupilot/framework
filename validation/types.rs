//! Validation result types and diagnostic structures.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct ValidationResult {
    pub is_valid: bool,
    pub diagnostics: Vec<ValidationDiagnostic>,
    #[serde(default)]
    pub phase_plugs: HashMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct ValidationDiagnostic {
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[specta(type = u32)]
    pub start_line: usize,
    #[specta(type = u32)]
    pub start_column: usize,
    #[specta(type = u32)]
    pub end_line: usize,
    #[specta(type = u32)]
    pub end_column: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub related_info: Vec<RelatedDiagnosticInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Type)]
pub struct RelatedDiagnosticInfo {
    pub message: String,
    #[specta(type = u32)]
    pub start_line: usize,
    #[specta(type = u32)]
    pub start_column: usize,
    #[specta(type = u32)]
    pub end_line: usize,
    #[specta(type = u32)]
    pub end_column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Type)]
#[repr(u8)]
pub enum DiagnosticSeverity {
    Error = 8,
    Warning = 4,
    Info = 2,
    Hint = 1,
}

impl serde::Serialize for DiagnosticSeverity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(*self as u8)
    }
}

impl<'de> serde::Deserialize<'de> for DiagnosticSeverity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        match value {
            8 => Ok(DiagnosticSeverity::Error),
            4 => Ok(DiagnosticSeverity::Warning),
            2 => Ok(DiagnosticSeverity::Info),
            1 => Ok(DiagnosticSeverity::Hint),
            _ => Err(serde::de::Error::custom(format!(
                "Invalid severity value: {}",
                value
            ))),
        }
    }
}

impl ValidationDiagnostic {
    pub fn error(code: &str, message: String, line: usize, col: usize, len: usize) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            code: code.to_string(),
            message,
            start_line: line,
            start_column: col,
            end_line: line,
            end_column: col + len,
            source: "tofupilot".to_string(),
            related_info: vec![],
        }
    }

    pub fn warning(code: &str, message: String, line: usize, col: usize, len: usize) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            code: code.to_string(),
            message,
            start_line: line,
            start_column: col,
            end_line: line,
            end_column: col + len,
            source: "tofupilot".to_string(),
            related_info: vec![],
        }
    }

    pub fn with_related(mut self, related: Vec<RelatedDiagnosticInfo>) -> Self {
        self.related_info = related;
        self
    }
}
