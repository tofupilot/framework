//! Measurement evaluation and validation.
//!
//! Merges YAML-defined measurements with Python-reported values,
//! evaluates validators (min/max/expected), and determines pass/fail outcomes.

pub mod evaluation;
pub mod issue;
pub mod types;

pub use evaluation::{auto_evaluate_measurements, complete_declared_measurements};
pub use issue::{DeclaredPart, MatchIssue};
pub use types::{Measurement, MeasurementValue};
