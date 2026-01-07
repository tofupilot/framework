//! Python version constraint handling for TofuPilot Studio.
//!
//! Computes the intersection between user's `requires-python` and Studio's minimum requirement (>=3.9).

use pep440_rs::{Version, VersionSpecifier, VersionSpecifiers};
use std::path::Path;
use std::str::FromStr;
use toml_edit::DocumentMut;

pub const STUDIO_MIN_VERSION: (u64, u64) = (3, 9);

#[derive(Debug, PartialEq)]
pub enum ConstraintResult {
    /// Use this effective constraint with UV
    Effective(String),
    /// Incompatible - intersection is empty
    Incompatible { user_constraint: String },
}

/// Parse `requires-python` from pyproject.toml
pub fn parse_requires_python(project_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(project_path.join("pyproject.toml")).ok()?;
    let doc: toml::Table = toml::from_str(&content).ok()?;
    doc.get("project")?
        .get("requires-python")?
        .as_str()
        .map(String::from)
}

/// RAII guard that temporarily updates requires-python in pyproject.toml and restores it on drop.
pub struct RequiresPythonGuard {
    pyproject_path: std::path::PathBuf,
    original_content: String,
}

impl RequiresPythonGuard {
    /// Temporarily update requires-python to the effective constraint.
    /// Returns None if no update is needed (constraint already matches or is stricter).
    pub fn new(project_path: &Path, effective_constraint: &str) -> Result<Option<Self>, String> {
        let pyproject_path = project_path.join("pyproject.toml");
        let original_content = std::fs::read_to_string(&pyproject_path)
            .map_err(|e| format!("Failed to read pyproject.toml: {}", e))?;

        let mut doc: DocumentMut = original_content
            .parse()
            .map_err(|e| format!("Failed to parse pyproject.toml: {}", e))?;

        let current = doc
            .get("project")
            .and_then(|p| p.get("requires-python"))
            .and_then(|v| v.as_str())
            .map(String::from);

        // Only update if different from current
        if current.as_deref() == Some(effective_constraint) {
            return Ok(None);
        }

        // Update requires-python
        if let Some(project) = doc.get_mut("project") {
            if let Some(table) = project.as_table_mut() {
                table.insert(
                    "requires-python",
                    toml_edit::value(effective_constraint),
                );
            }
        }

        std::fs::write(&pyproject_path, doc.to_string())
            .map_err(|e| format!("Failed to write pyproject.toml: {}", e))?;

        log::debug!(
            "Temporarily updated requires-python from {:?} to {}",
            current,
            effective_constraint
        );

        Ok(Some(Self {
            pyproject_path,
            original_content,
        }))
    }
}

impl Drop for RequiresPythonGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::write(&self.pyproject_path, &self.original_content) {
            log::error!("Failed to restore pyproject.toml: {}", e);
        } else {
            log::debug!("Restored original pyproject.toml");
        }
    }
}

/// Compute the effective Python version constraint.
///
/// Returns:
/// - `Effective(constraint)` - the constraint to pass to UV
/// - `Incompatible` - user's constraint has no intersection with >=3.9
pub fn compute_effective_constraint(user_constraint: Option<&str>) -> ConstraintResult {
    let studio_min = Version::new([STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1]);
    let studio_specifier =
        VersionSpecifier::from_str(&format!(">={}.{}", STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1))
            .expect("Studio specifier is valid");

    let Some(user_str) = user_constraint else {
        return ConstraintResult::Effective(format!(
            ">={}.{}",
            STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1
        ));
    };

    let user_specifiers = match VersionSpecifiers::from_str(user_str) {
        Ok(specs) => specs,
        Err(_) => {
            log::warn!("Failed to parse requires-python '{}', using Studio default", user_str);
            return ConstraintResult::Effective(format!(
                ">={}.{}",
                STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1
            ));
        }
    };

    if !has_valid_intersection(&user_specifiers, &studio_min) {
        return ConstraintResult::Incompatible {
            user_constraint: user_str.to_string(),
        };
    }

    let effective = compute_intersection_constraint(&user_specifiers, &studio_specifier);
    ConstraintResult::Effective(effective)
}

/// Check if there's any Python version that satisfies both constraints.
fn has_valid_intersection(user_specs: &VersionSpecifiers, studio_min: &Version) -> bool {
    let test_versions: Vec<Version> = (9..=15)
        .flat_map(|minor| {
            (0..=10).map(move |patch| Version::new([3, minor, patch]))
        })
        .collect();

    test_versions.iter().any(|v| {
        user_specs.contains(v) && v >= studio_min
    })
}

/// Compute the effective constraint string that represents the intersection.
fn compute_intersection_constraint(
    user_specs: &VersionSpecifiers,
    studio_spec: &VersionSpecifier,
) -> String {
    let studio_min = Version::new([STUDIO_MIN_VERSION.0, STUDIO_MIN_VERSION.1]);

    let mut lower_bound: Option<Version> = Some(studio_min.clone());
    let mut upper_bound: Option<Version> = None;
    let mut upper_inclusive = false;

    for spec in user_specs.iter() {
        match spec.operator() {
            pep440_rs::Operator::GreaterThan => {
                let v = spec.version().clone();
                if lower_bound.as_ref().map_or(true, |lb| v >= *lb) {
                    lower_bound = Some(next_version(&v));
                }
            }
            pep440_rs::Operator::GreaterThanEqual => {
                let v = spec.version().clone();
                if v > studio_min {
                    if lower_bound.as_ref().map_or(true, |lb| v > *lb) {
                        lower_bound = Some(v);
                    }
                }
            }
            pep440_rs::Operator::LessThan => {
                let v = spec.version().clone();
                if upper_bound.as_ref().map_or(true, |ub| v < *ub) {
                    upper_bound = Some(v);
                    upper_inclusive = false;
                }
            }
            pep440_rs::Operator::LessThanEqual => {
                let v = spec.version().clone();
                if upper_bound.as_ref().map_or(true, |ub| v <= *ub) {
                    upper_bound = Some(v);
                    upper_inclusive = true;
                }
            }
            pep440_rs::Operator::Equal | pep440_rs::Operator::ExactEqual => {
                let v = spec.version().clone();
                if v >= studio_min {
                    return format!("=={}", v);
                }
            }
            pep440_rs::Operator::NotEqual
            | pep440_rs::Operator::TildeEqual
            | pep440_rs::Operator::EqualStar
            | pep440_rs::Operator::NotEqualStar => {
                // These don't affect bounds directly for our intersection calculation
            }
        }
    }

    let mut parts = Vec::new();

    if let Some(lb) = lower_bound {
        parts.push(format!(">={}", lb));
    } else {
        parts.push(format!(">={}", studio_spec.version()));
    }

    if let Some(ub) = upper_bound {
        if upper_inclusive {
            parts.push(format!("<={}", ub));
        } else {
            parts.push(format!("<{}", ub));
        }
    }

    parts.join(",")
}

fn next_version(v: &Version) -> Version {
    let release = v.release();
    if release.len() >= 2 {
        Version::new([release[0], release[1] + 1])
    } else {
        Version::new([release[0] + 1, 0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_constraint_uses_studio_default() {
        let result = compute_effective_constraint(None);
        assert_eq!(result, ConstraintResult::Effective(">=3.9".to_string()));
    }

    #[test]
    fn test_looser_constraint_uses_studio_min() {
        // >=3.8 intersected with >=3.9 = >=3.9
        let result = compute_effective_constraint(Some(">=3.8"));
        assert_eq!(result, ConstraintResult::Effective(">=3.9".to_string()));
    }

    #[test]
    fn test_looser_constraint_37() {
        // >=3.7 intersected with >=3.9 = >=3.9
        let result = compute_effective_constraint(Some(">=3.7"));
        assert_eq!(result, ConstraintResult::Effective(">=3.9".to_string()));
    }

    #[test]
    fn test_same_constraint() {
        // >=3.9 intersected with >=3.9 = >=3.9
        let result = compute_effective_constraint(Some(">=3.9"));
        assert_eq!(result, ConstraintResult::Effective(">=3.9".to_string()));
    }

    #[test]
    fn test_stricter_constraint_preserved() {
        // >=3.10 intersected with >=3.9 = >=3.10
        let result = compute_effective_constraint(Some(">=3.10"));
        assert_eq!(result, ConstraintResult::Effective(">=3.10".to_string()));
    }

    #[test]
    fn test_upper_bound_preserved() {
        // >=3.8,<3.12 intersected with >=3.9 = >=3.9,<3.12
        let result = compute_effective_constraint(Some(">=3.8,<3.12"));
        assert_eq!(result, ConstraintResult::Effective(">=3.9,<3.12".to_string()));
    }

    #[test]
    fn test_tight_range() {
        // >=3.8,<3.10 intersected with >=3.9 = >=3.9,<3.10
        let result = compute_effective_constraint(Some(">=3.8,<3.10"));
        assert_eq!(result, ConstraintResult::Effective(">=3.9,<3.10".to_string()));
    }

    #[test]
    fn test_incompatible_upper_only() {
        // <=3.8 has no intersection with >=3.9
        let result = compute_effective_constraint(Some("<=3.8"));
        assert_eq!(
            result,
            ConstraintResult::Incompatible {
                user_constraint: "<=3.8".to_string()
            }
        );
    }

    #[test]
    fn test_incompatible_less_than() {
        // <3.9 has no intersection with >=3.9
        let result = compute_effective_constraint(Some("<3.9"));
        assert_eq!(
            result,
            ConstraintResult::Incompatible {
                user_constraint: "<3.9".to_string()
            }
        );
    }

    #[test]
    fn test_incompatible_exact() {
        // ==3.8 has no intersection with >=3.9
        let result = compute_effective_constraint(Some("==3.8"));
        assert_eq!(
            result,
            ConstraintResult::Incompatible {
                user_constraint: "==3.8".to_string()
            }
        );
    }

    #[test]
    fn test_incompatible_range() {
        // >=3.8,<3.9 has no intersection with >=3.9
        let result = compute_effective_constraint(Some(">=3.8,<3.9"));
        assert_eq!(
            result,
            ConstraintResult::Incompatible {
                user_constraint: ">=3.8,<3.9".to_string()
            }
        );
    }

    #[test]
    fn test_exact_valid() {
        // ==3.11 is valid (>=3.9)
        let result = compute_effective_constraint(Some("==3.11"));
        assert_eq!(result, ConstraintResult::Effective("==3.11".to_string()));
    }
}
