//! YAML procedure schema definitions and validation.
//!
//! Defines the structure of test procedures including phases, plugs, measurements,
//! execution configuration, and unit information.

pub mod procedure;

pub use procedure::{ProcedureDefinition, ProcedureYaml, UnitConfig, UnitFieldConfig};
