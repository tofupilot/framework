//! Slot name validation.

use super::types::ValidationDiagnostic;

pub fn validate_slot_names(slots: &[String]) -> Vec<ValidationDiagnostic> {
    let mut diagnostics = Vec::new();

    for (index, slot_name) in slots.iter().enumerate() {
        if slot_name.trim().is_empty() {
            diagnostics.push(ValidationDiagnostic::error(
                "invalid-slot-name",
                format!("Slot #{} cannot be empty (reserved for internal use)", index + 1),
                1,
                1,
                10,
            ));
            continue;
        }

        if slot_name.trim().eq_ignore_ascii_case("SHARED") {
            diagnostics.push(ValidationDiagnostic::error(
                "invalid-slot-name",
                format!("Slot name '{}' is reserved for internal use", slot_name),
                1,
                1,
                slot_name.len(),
            ));
            continue;
        }

        if slot_name != slot_name.trim() || slot_name.contains(char::is_whitespace) {
            diagnostics.push(ValidationDiagnostic::error(
                "invalid-slot-name",
                format!("Slot name '{}' cannot contain whitespace", slot_name),
                1,
                1,
                slot_name.len(),
            ));
            continue;
        }

        let invalid_chars = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
        if slot_name.chars().any(|c| invalid_chars.contains(&c)) {
            diagnostics.push(ValidationDiagnostic::error(
                "invalid-slot-name",
                format!(
                    "Slot name '{}' contains invalid characters. Use only letters, numbers, hyphens, and underscores",
                    slot_name
                ),
                1,
                1,
                slot_name.len(),
            ));
            continue;
        }

        if slot_name.len() > 50 {
            diagnostics.push(ValidationDiagnostic::error(
                "invalid-slot-name",
                format!("Slot name '{}' is too long (max 50 characters)", slot_name),
                1,
                1,
                slot_name.len(),
            ));
            continue;
        }
    }

    diagnostics
}
