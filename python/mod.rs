//! Python environment resolution and virtual environment management.
//!
//! Handles Python executable discovery, virtual environment creation/validation,
//! and dependency installation for test phases.
//!
//! Resolution order: venv → UV-managed Python (auto-install if needed)

pub mod environment;
pub mod installation;
pub mod manifest;
pub mod resolution;
pub mod types;
pub mod utils;

pub use installation::{ensure_python_available, install_uv_python, list_uv_pythons};
pub use manifest::PythonManifest;
pub use resolution::resolve_python_executable;
pub use types::*;
