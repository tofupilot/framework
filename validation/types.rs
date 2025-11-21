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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_severity_serialization() {
        let severities = vec![
            DiagnosticSeverity::Error,
            DiagnosticSeverity::Warning,
            DiagnosticSeverity::Info,
            DiagnosticSeverity::Hint,
        ];

        for severity in severities {
            let json = serde_json::to_string(&severity).unwrap();
            let deserialized: DiagnosticSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(severity, deserialized);
        }
    }

    #[test]
    fn test_severity_serializes_as_string() {
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Error).unwrap(),
            r#""Error""#
        );
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Warning).unwrap(),
            r#""Warning""#
        );
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Info).unwrap(),
            r#""Info""#
        );
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Hint).unwrap(),
            r#""Hint""#
        );
    }

    #[test]
    fn test_diagnostic_error_constructor() {
        let diag = ValidationDiagnostic::error(
            "test-code",
            "Test message".to_string(),
            10,
            5,
            3,
        );

        assert!(matches!(diag.severity, DiagnosticSeverity::Error));
        assert_eq!(diag.code, "test-code");
        assert_eq!(diag.message, "Test message");
        assert_eq!(diag.start_line, 10);
        assert_eq!(diag.start_column, 5);
        assert_eq!(diag.end_line, 10);
        assert_eq!(diag.end_column, 8);
        assert_eq!(diag.source, "tofupilot");
        assert!(diag.related_info.is_empty());
    }

    #[test]
    fn test_diagnostic_warning_constructor() {
        let diag = ValidationDiagnostic::warning(
            "warn-code",
            "Warning message".to_string(),
            5,
            2,
            10,
        );

        assert!(matches!(diag.severity, DiagnosticSeverity::Warning));
        assert_eq!(diag.code, "warn-code");
        assert_eq!(diag.end_column, 12);
    }

    #[test]
    fn test_diagnostic_with_related_info() {
        let related = vec![
            RelatedDiagnosticInfo {
                message: "See here".to_string(),
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 5,
            }
        ];

        let diag = ValidationDiagnostic::error(
            "test",
            "Test".to_string(),
            1,
            1,
            1,
        ).with_related(related);

        assert_eq!(diag.related_info.len(), 1);
        assert_eq!(diag.related_info[0].message, "See here");
    }

    #[test]
    fn test_validation_result_serialization() {
        let result = ValidationResult {
            is_valid: false,
            diagnostics: vec![
                ValidationDiagnostic::error("test", "Error".to_string(), 1, 1, 5),
            ],
            phase_plugs: HashMap::new(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ValidationResult = serde_json::from_str(&json).unwrap();

        assert_eq!(result.is_valid, deserialized.is_valid);
        assert_eq!(result.diagnostics.len(), deserialized.diagnostics.len());
        assert_eq!(
            result.diagnostics[0].severity,
            deserialized.diagnostics[0].severity
        );
    }

    #[test]
    fn test_diagnostic_full_serialization_format() {
        let diag = ValidationDiagnostic::error(
            "test-error",
            "Test error message".to_string(),
            5,
            10,
            15,
        );

        let json = serde_json::to_value(&diag).unwrap();

        assert_eq!(json["severity"], "Error");
        assert_eq!(json["code"], "test-error");
        assert_eq!(json["message"], "Test error message");
        assert_eq!(json["start_line"], 5);
        assert_eq!(json["start_column"], 10);
        assert_eq!(json["end_line"], 5);
        assert_eq!(json["end_column"], 25);
        assert_eq!(json["source"], "tofupilot");
    }
}
