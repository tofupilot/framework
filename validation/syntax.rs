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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::types::DiagnosticSeverity;

    #[test]
    fn test_valid_yaml() {
        let yaml = r#"
name: Test Procedure
setup: []
main: []
teardown: []
plugs: []
"#;
        let result = validate_yaml_syntax(yaml);
        assert!(result.is_valid);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn test_invalid_yaml_indentation() {
        let yaml = r#"
name: Test
  invalid_indent: value
"#;
        let result = validate_yaml_syntax(yaml);
        assert!(!result.is_valid);
        assert_eq!(result.diagnostics.len(), 1);

        let diag = &result.diagnostics[0];
        assert!(matches!(diag.severity, DiagnosticSeverity::Error));
        assert_eq!(diag.code, "yaml-syntax-error");
        assert!(diag.message.contains("YAML syntax error"));
        assert!(diag.start_line > 0);
        assert!(diag.start_column > 0);
    }

    #[test]
    fn test_invalid_yaml_unclosed_bracket() {
        let yaml = r#"
name: Test
items: [1, 2, 3
"#;
        let result = validate_yaml_syntax(yaml);
        assert!(!result.is_valid);
        assert_eq!(result.diagnostics.len(), 1);
        assert!(matches!(result.diagnostics[0].severity, DiagnosticSeverity::Error));
    }

    #[test]
    fn test_invalid_yaml_colon_in_unquoted_string() {
        let yaml = r#"
name: Test: Invalid
"#;
        let result = validate_yaml_syntax(yaml);
        assert!(!result.is_valid);
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[test]
    fn test_empty_yaml() {
        let yaml = "";
        let result = validate_yaml_syntax(yaml);
        assert!(result.is_valid);
    }

    #[test]
    fn test_diagnostic_has_valid_location() {
        let yaml = r#"
name: Test
  bad_indent: value
"#;
        let result = validate_yaml_syntax(yaml);
        assert!(!result.is_valid);

        let diag = &result.diagnostics[0];
        assert!(diag.start_line >= 1);
        assert!(diag.start_column >= 1);
        assert!(diag.end_line >= diag.start_line);
        assert!(diag.end_column >= diag.start_column);
        assert_eq!(diag.source, "tofupilot");
    }
}
