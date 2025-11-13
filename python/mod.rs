//! Python environment resolution and virtual environment management.
//!
//! Handles Python executable discovery, virtual environment creation/validation,
//! and dependency installation for test phases.
//!
//! Resolution order: venv → uv python find → error

pub mod environment;
pub mod manifest;
pub mod resolution;
pub mod types;
pub mod utils;

pub use manifest::PythonManifest;
pub use resolution::resolve_python_executable;
pub use types::*;
