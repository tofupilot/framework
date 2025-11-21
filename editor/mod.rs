//! Editor functionality for procedure editing, validation, and file watching.
//!
//! Provides commands for:
//! - YAML file watching and auto-reload
//! - Procedure validation
//! - Editor settings management
//! - Configuration saving

pub mod commands;

pub use commands::{
    load_editor_settings, save_editor_settings, save_yaml_config, unwatch_yaml_file,
    watch_yaml_file, EditorSettings,
};
