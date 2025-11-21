//! YAML syntax validation.

use std::collections::HashMap;
use super::types::{ValidationResult, ValidationDiagnostic};

pub fn validate_yaml_syntax(yaml_content: &str) -> ValidationResult {
    let mut diagnostics = Vec::new();

    match serde_yaml::from_str::<serde_yaml::Value>(yaml_content) {
        Ok(_) => {}
        Err(e) => {
            let (line, col) = if let Some(location) = e.location() {
                (location.line(), location.column())
            } else {
                (1, 1)
            };

            let error_msg = e.to_string();

            let lines: Vec<&str> = yaml_content.lines().collect();
            let highlight_len = if line > 0 && line <= lines.len() {
                let target_line = lines[line - 1];
                if col > 0 && col <= target_line.len() {
                    (target_line.len() - col + 1).max(1)
                } else {
                    target_line.trim().len().max(1)
                }
            } else {
                1
            };

            diagnostics.push(ValidationDiagnostic::error(
                "yaml-syntax-error",
                format!("YAML syntax error: {}", error_msg),
                line,
                col,
                highlight_len,
            ));
        }
    }

    ValidationResult {
        is_valid: diagnostics.is_empty(),
        diagnostics,
        phase_plugs: HashMap::new(),
    }
}
