use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use validator::Validate;
use ts_rs::TS;
use crate::PlugScope;
use crate::utils::{
    to_python_identifier,
    deserialize_duration,
    serialize_duration,
    default_true,
};

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_stop() -> FirstFailureAction {
    FirstFailureAction::Stop
}

fn ui_is_empty(ui: &UIConfig) -> bool {
    ui.instructions.is_empty() && ui.components.is_empty()
}

fn default_strategy() -> ExecutionStrategy {
    ExecutionStrategy::PhaseFirst
}

fn is_default_strategy(strategy: &ExecutionStrategy) -> bool {
    matches!(strategy, ExecutionStrategy::PhaseFirst)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStage {
    Setup,
    Main,
    Teardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    All,
    Each,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum FirstFailureAction {
    Stop,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    PhaseFirst,
    SlotFirst,
}

// Internal enum for execution engine
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum StageScope {
    SetupAll,
    SetupEach,
    Main,
    TeardownEach,
    TeardownAll,
}


#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[ts(export)]
pub struct ProcedureYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    pub name: String,

    pub version: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugs: Vec<PlugDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<PhaseDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<PhaseDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<PhaseDefinition>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub struct ProcedureDefinition {
    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[validate(length(min = 1, max = 50))]
    pub version: String,

    #[serde(default)]
    #[validate(length(max = 50000))]
    pub description: String,

    #[serde(default = "default_strategy")]
    pub strategy: ExecutionStrategy,

    #[serde(default)]
    #[validate(range(min = 1, max = 256))]
    pub worker_count: Option<usize>,

    #[serde(default = "default_stop")]
    pub on_first_failure: FirstFailureAction,

    #[serde(default)]
    #[validate(nested)]
    pub slots: Vec<SlotConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitConfig>,

    #[serde(default)]
    #[validate(nested)]
    pub plugs: Vec<PlugDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub setup: Vec<PhaseDefinition>,

    #[serde(default)]
    #[validate(nested)]
    pub main: Vec<PhaseDefinition>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub teardown: Vec<PhaseDefinition>,
}

impl From<ProcedureYaml> for ProcedureDefinition {
    fn from(raw: ProcedureYaml) -> Self {
        let execution = raw.execution.unwrap_or_default();

        ProcedureDefinition {
            name: raw.name,
            version: raw.version,
            description: raw.description,
            strategy: execution.strategy,
            worker_count: execution.worker_count,
            on_first_failure: execution.on_first_failure,
            slots: execution.slots,
            unit: raw.unit,
            plugs: raw.plugs,
            setup: raw.setup,
            main: raw.main,
            teardown: raw.teardown,
        }
    }
}

// Raw deserialization struct for SlotConfig
#[derive(Debug, Deserialize)]
struct SlotConfigRaw {
    #[serde(default)]
    key: Option<String>,
    name: String,
}

impl From<SlotConfigRaw> for SlotConfig {
    fn from(raw: SlotConfigRaw) -> Self {
        let key = if let Some(user_key) = raw.key {
            if !crate::utils::python_identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "Slot '{}' has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    raw.name,
                    user_key
                );
            }
            user_key
        } else {
            to_python_identifier(&raw.name)
        };
        SlotConfig {
            key,
            name: raw.name,
        }
    }
}

#[derive(Debug, Validate, Clone, TS, Serialize)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct SlotConfig {
    #[validate(length(min = 1, max = 50))]
    pub key: String,

    #[validate(length(min = 1, max = 50))]
    pub name: String,
}

impl<'de> Deserialize<'de> for SlotConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = SlotConfigRaw::deserialize(deserializer)?;
        Ok(raw.into())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct UnitFieldConfig {
    /// Default value pre-filled in the input field
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,

    /// Placeholder text shown in the input field
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,

    /// Minimum length for the field value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,

    /// Maximum length for the field value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,

    /// Regex pattern for validation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}


impl Default for UnitFieldConfig {
    fn default() -> Self {
        Self {
            placeholder: None,
            default_value: None,
            min_length: None,
            max_length: None,
            pattern: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, TS)]
#[ts(export)]
pub struct UnitConfig {
    /// Serial number field configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<UnitFieldConfig>,

    /// Part number field configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<UnitFieldConfig>,

    /// Revision number field configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_number: Option<UnitFieldConfig>,

    /// Batch number field configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_number: Option<UnitFieldConfig>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, TS)]
#[ts(export)]
pub struct ExecutionConfig {
    #[serde(default = "default_strategy", skip_serializing_if = "is_default_strategy")]
    pub strategy: ExecutionStrategy,

    #[serde(default, rename = "workers", skip_serializing_if = "Option::is_none")]
    #[validate(range(min = 1, max = 256))]
    pub worker_count: Option<usize>,

    #[serde(default = "default_stop")]
    pub on_first_failure: FirstFailureAction,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub slots: Vec<SlotConfig>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            strategy: ExecutionStrategy::PhaseFirst,
            worker_count: None,
            on_first_failure: FirstFailureAction::Stop,
            slots: vec![],
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub enum ParallelMode {
    #[default]
    Sequential,
    Batch,
    Independent,
}


#[derive(Debug, Serialize, Deserialize, Validate, Clone, TS)]
#[ts(export)]
pub struct RetryConfig {
    #[validate(range(min = 1, max = 10))]
    pub limit: usize,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub delay: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum PhaseNextAction {
    Continue,
    Stop,
    Retry,
    Skip,
    Fail,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, TS)]
#[ts(export)]
pub struct ThenConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<PhaseNextAction>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail: Option<PhaseNextAction>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PhaseNextAction>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<PhaseNextAction>,
}

// Raw deserialization struct for PlugDefinition
#[derive(Debug, Deserialize)]
struct PlugDefinitionRaw {
    #[serde(default)]
    key: Option<String>,
    name: String,
    #[serde(default)]
    scope: Option<Scope>,
    python: PythonSpec,
    #[serde(default)]
    description: String,
}

impl From<PlugDefinitionRaw> for PlugDefinition {
    fn from(raw: PlugDefinitionRaw) -> Self {
        let key = if let Some(user_key) = raw.key {
            if !crate::utils::python_identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "Plug '{}' has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    raw.name,
                    user_key
                );
            }
            user_key
        } else {
            to_python_identifier(&raw.name)
        };
        PlugDefinition {
            key,
            name: raw.name,
            scope: raw.scope,
            python: raw.python,
            description: raw.description,
        }
    }
}

#[derive(Debug, Validate, Clone, TS, Serialize)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct PlugDefinition {
    #[validate(length(min = 1, max = 100))]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,

    pub python: PythonSpec,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl<'de> Deserialize<'de> for PlugDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = PlugDefinitionRaw::deserialize(deserializer)?;
        Ok(raw.into())
    }
}

impl PlugDefinition {
    pub fn to_config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "module": self.python.get_module(),
            "class": self.python.get_callable_name()
        })
    }

    pub fn get_key(&self) -> String {
        self.key.clone()
    }

    pub fn get_display_name(&self) -> String {
        self.name.clone()
    }
}


/// Python specification: "path/to/file:callable_name" or "path.to.file:callable_name"
///
/// Examples:
/// - "phases/test_voltage" → phases/test_voltage.py::test_voltage()
/// - "phases.calibration:setup" → phases/calibration.py::setup()
/// - "plugs.python.ethercat:EtherCATManager" → plugs/python/ethercat.py::EtherCATManager
#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(type = "string")]
#[serde(transparent)]
pub struct PythonSpec(String);

impl PythonSpec {
    /// Parse the spec and resolve to (file_path, callable_name)
    ///
    /// # Arguments
    /// * `project_dir` - Root directory of the project
    ///
    /// # Returns
    /// * `Ok((PathBuf, String))` - Resolved file path and callable name (function or class)
    /// * `Err(String)` - Validation or resolution error
    pub fn parse(&self, project_dir: &Path) -> Result<(PathBuf, String), String> {
        let spec = self.0.trim();

        // Validate not empty
        if spec.is_empty() {
            return Err("Python spec cannot be empty".into());
        }

        // Split on ':' to separate path from callable name
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() > 2 {
            return Err(format!("Invalid spec format '{}': too many ':' separators", spec));
        }

        let path_part = parts[0].trim();
        let name_part = parts.get(1).map(|s| s.trim());

        // Validate path not empty
        if path_part.is_empty() {
            return Err("Path cannot be empty".into());
        }

        // Validate no parent directory traversal
        if path_part.contains("..") {
            return Err(format!("Parent directory traversal not allowed: '{}'", path_part));
        }

        // Validate no absolute paths
        if path_part.starts_with('/') || path_part.starts_with('~') {
            return Err(format!("Absolute paths not allowed: '{}'", path_part));
        }

        // Validate Windows absolute paths
        if path_part.len() >= 2 && path_part.chars().nth(1) == Some(':') {
            return Err(format!("Absolute paths not allowed: '{}'", path_part));
        }

        // Convert "plugs.python.ethercat" → "plugs/python/ethercat.py"
        let file_path = project_dir
            .join(path_part.replace('.', "/"))
            .with_extension("py");

        // Get callable name (or default to last component of path)
        let callable_name = if let Some(name) = name_part {
            if name.is_empty() {
                return Err("Callable name cannot be empty after ':'".into());
            }
            name.to_string()
        } else {
            // Default to last component of path
            path_part
                .rsplit(&['.', '/'][..])
                .next()
                .unwrap_or(path_part)
                .to_string()
        };

        // Validate file exists
        if !file_path.exists() {
            return Err(format!("Python file not found: {}", file_path.display()));
        }

        // Validate callable name is valid Python identifier
        if !is_valid_python_identifier(&callable_name) {
            return Err(format!("Invalid Python identifier: '{}'", callable_name));
        }

        Ok((file_path, callable_name))
    }

    /// Get the callable name (for display purposes, doesn't validate)
    pub fn get_callable_name(&self) -> String {
        let spec = self.0.trim();
        if let Some(name) = spec.split(':').nth(1) {
            name.trim().to_string()
        } else {
            spec.rsplit(&['.', '/'][..])
                .next()
                .unwrap_or(spec)
                .to_string()
        }
    }

    /// Get the module path (for backward compat with execution code)
    pub fn get_module(&self) -> String {
        let spec = self.0.trim();
        spec.split(':').next().unwrap_or(spec).to_string()
    }
}

/// Validate that a string is a valid Python identifier
fn is_valid_python_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    // First char must be letter or underscore
    let first = match name.chars().next() {
        Some(c) => c,
        None => return false, // Already checked above, but be explicit
    };
    if !first.is_alphabetic() && first != '_' {
        return false;
    }

    // Rest must be alphanumeric or underscore
    name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, TS)]
#[ts(export)]
pub struct ExecutableConfig {
    #[validate(length(min = 1, max = 10000))]
    pub command: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub shell: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 4096))]
    pub working_directory: Option<String>,
}

// Raw deserialization struct for PhaseDefinition
#[derive(Debug, Deserialize)]
struct PhaseDefinitionRaw {
    #[serde(default)]
    key: Option<String>,
    name: String,
    #[serde(default)]
    scope: Option<Scope>,
    #[serde(default)]
    python: Option<PythonSpec>,
    #[serde(default)]
    executable: Option<ExecutableConfig>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    measurements: Vec<MeasurementSpec>,
    #[serde(default)]
    ui: UIConfig,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_duration")]
    timeout: Option<u64>,
    #[serde(default)]
    retry: Option<RetryConfig>,
    #[serde(default)]
    then: Option<ThenConfig>,
}

impl From<PhaseDefinitionRaw> for PhaseDefinition {
    fn from(raw: PhaseDefinitionRaw) -> Self {
        let key = if let Some(user_key) = raw.key {
            if !crate::utils::python_identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "Phase '{}' has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    raw.name,
                    user_key
                );
            }
            user_key
        } else {
            to_python_identifier(&raw.name)
        };
        PhaseDefinition {
            key,
            name: raw.name,
            scope: raw.scope,
            python: raw.python,
            executable: raw.executable,
            description: raw.description,
            measurements: raw.measurements,
            ui: raw.ui,
            enabled: raw.enabled,
            result: raw.result,
            depends_on: raw.depends_on,
            timeout: raw.timeout,
            retry: raw.retry,
            then: raw.then,
        }
    }
}

#[derive(Debug, Validate, Clone, TS, Serialize)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct PhaseDefinition {
    #[validate(length(min = 1, max = 100))]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSpec>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableConfig>,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<MeasurementSpec>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "ui_is_empty")]
    pub ui: UIConfig,

    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,

    #[validate(length(max = 50))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,

    #[validate(length(max = 100))]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    #[ts(skip)]
    #[validate(range(min = 1, max = 86400000))]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub timeout: Option<u64>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,
}

impl<'de> Deserialize<'de> for PhaseDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = PhaseDefinitionRaw::deserialize(deserializer)?;
        Ok(raw.into())
    }
}

impl PhaseDefinition {
    pub fn should_skip(&self) -> bool {
        !self.enabled || self.result.as_deref() == Some("skip")
    }

    pub fn to_python_identifier(&self) -> String {
        to_python_identifier(&self.get_display_name())
    }

    pub fn get_key(&self) -> String {
        self.key.clone()
    }

    pub fn get_display_name(&self) -> String {
        self.name.clone()
    }

    pub fn validate_measurement_uniqueness(&self) -> Result<(), String> {
        let mut seen_keys: HashSet<String> = HashSet::new();
        for measurement in &self.measurements {
            let key = &measurement.key;
            if seen_keys.contains(key) {
                return Err(format!(
                    "Phase '{}' has duplicate measurement key: '{}'",
                    self.get_display_name(), key
                ));
            }
            seen_keys.insert(key.clone());
        }
        Ok(())
    }

    pub fn validate_single_runtime(&self) -> Result<(), String> {
        let has_python = self.python.is_some();
        let has_executable = self.executable.is_some();

        if has_python && has_executable {
            return Err(format!(
                "Phase '{}' has both python and executable runtimes defined (only one allowed)",
                self.get_display_name()
            ));
        }

        Ok(())
    }
}

impl ProcedureDefinition {
    /// Ensure key is unique by appending counter if needed
    fn ensure_unique_key(key: String, seen_keys: &mut HashSet<String>) -> String {
        if !seen_keys.contains(&key) {
            seen_keys.insert(key.clone());
            return key;
        }

        let mut counter = 1;
        loop {
            let candidate = format!("{}-{}", key, counter);
            if !seen_keys.contains(&candidate) {
                seen_keys.insert(candidate.clone());
                return candidate;
            }
            counter += 1;
        }
    }

    /// Iterator over all phases with their stage scope (standardized iteration)
    pub fn iter_phases_with_stage(&self) -> impl Iterator<Item = (StageScope, &PhaseDefinition)> {
        self.setup.iter()
            .map(|p| {
                let scope = if p.scope == Some(Scope::All) {
                    StageScope::SetupAll
                } else {
                    StageScope::SetupEach
                };
                (scope, p)
            })
            .chain(self.main.iter().map(|p| (StageScope::Main, p)))
            .chain(self.teardown.iter().map(|p| {
                let scope = if p.scope == Some(Scope::All) {
                    StageScope::TeardownAll
                } else {
                    StageScope::TeardownEach
                };
                (scope, p)
            }))
    }

    /// Get all phases with their stages for execution (calls standardized iterator)
    pub fn get_all_phases_with_stage_scope(&self) -> Vec<(StageScope, &PhaseDefinition)> {
        self.iter_phases_with_stage().collect()
    }

    /// Get all phases as a flat list with stage, scope, and keys populated (for frontend serialization)
    pub fn get_flat_phases(&self) -> Vec<PhaseDefinition> {
        let mut phases = Vec::new();
        let mut seen_keys = HashSet::new();

        for (_, phase) in self.iter_phases_with_stage() {
            let mut p = phase.clone();
            let key = phase.get_key();
            p.key = Self::ensure_unique_key(key, &mut seen_keys);
            if p.scope.is_none() {
                p.scope = Some(Scope::Each);
            }
            phases.push(p);
        }

        phases
    }

    /// Get total phase count across all types
    pub fn total_phase_count(&self) -> usize {
        self.setup.len() + self.main.len() + self.teardown.len()
    }

    /// Get all plugs with their scopes
    pub fn get_all_plugs_with_scope(&self) -> Vec<(PlugScope, &PlugDefinition)> {
        self.plugs
            .iter()
            .map(|plug| {
                let scope = if plug.scope == Some(Scope::All) {
                    PlugScope::All
                } else {
                    PlugScope::Each
                };
                (scope, plug)
            })
            .collect()
    }
}

impl PhaseDefinition {
    pub fn get_timeout_ms(&self) -> Option<u64> {
        self.timeout
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "kebab-case")]
pub enum TextSize {
    #[serde(rename = "xs")]
    Xs,
    #[serde(rename = "sm")]
    Sm,
    #[serde(rename = "base")]
    Base,
    #[serde(rename = "lg")]
    Lg,
    #[serde(rename = "xl")]
    Xl,
    #[serde(rename = "2xl")]
    Xl2,
    #[serde(rename = "3xl")]
    Xl3,
    #[serde(rename = "4xl")]
    Xl4,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum TextColor {
    Default,
    Zinc,
    Red,
    Orange,
    Yellow,
    Green,
    Emerald,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Pink,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum FontFamily {
    Default,
    Monospace,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ValidatorLevel {
    Critical,
    Alert,
    Notice,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(rename_all = "UPPERCASE")]
pub enum ValidatorOutcome {
    Pass,
    Fail,
    Unset,
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
#[serde(untagged)]
pub enum ValidatorExpectedValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    NumberArray(Vec<f64>),
    StringArray(Vec<String>),
    MixedArray(Vec<serde_json::Value>),
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
pub struct ValidatorSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ValidatorOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_value: Option<ValidatorExpectedValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
#[serde(untagged)]
pub enum AggregationValue {
    Number(f64),
    String(String),
    Boolean(bool),
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
pub struct AggregationSpec {
    pub aggregation_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ValidatorOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<AggregationValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
#[serde(untagged)]
pub enum AxisData {
    Numeric(Vec<f64>),
    String(Vec<String>),
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
pub struct AxisSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AxisData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, TS)]
#[ts(export)]
pub struct MultiDimensionalSpec {
    pub x_axis: AxisSpec,
    pub y_axis: Vec<AxisSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// Raw deserialization struct for MeasurementSpec
#[derive(Debug, Deserialize)]
struct MeasurementSpecRaw {
    #[serde(default)]
    key: Option<String>,
    name: String,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    validators: Option<Vec<ValidatorSpec>>,
    #[serde(default)]
    aggregations: Option<Vec<AggregationSpec>>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    multi_dimensional: Option<MultiDimensionalSpec>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    x_axis: Option<AxisSpec>,
    #[serde(default)]
    y_axis: Option<Vec<AxisSpec>>,
}

impl From<MeasurementSpecRaw> for MeasurementSpec {
    fn from(raw: MeasurementSpecRaw) -> Self {
        let key = if let Some(user_key) = raw.key {
            if !crate::utils::python_identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "Measurement '{}' has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    raw.name,
                    user_key
                );
            }
            user_key
        } else {
            to_python_identifier(&raw.name)
        };
        MeasurementSpec {
            key,
            name: raw.name,
            unit: raw.unit,
            validators: raw.validators,
            aggregations: raw.aggregations,
            description: raw.description,
            multi_dimensional: raw.multi_dimensional,
            title: raw.title,
            x_axis: raw.x_axis,
            y_axis: raw.y_axis,
        }
    }
}

#[derive(Debug, Validate, Clone, TS, Serialize)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub struct MeasurementSpec {
    #[validate(length(min = 1, max = 100))]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[validate(length(max = 50))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_dimensional: Option<MultiDimensionalSpec>,

    #[validate(length(max = 200))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_axis: Option<AxisSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<Vec<AxisSpec>>,
}

impl<'de> Deserialize<'de> for MeasurementSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = MeasurementSpecRaw::deserialize(deserializer)?;
        Ok(raw.into())
    }
}

impl MeasurementSpec {
    pub fn get_key(&self) -> String {
        self.key.clone()
    }

    pub fn get_display_name(&self) -> String {
        self.name.clone()
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Default, Clone, TS)]
#[ts(export)]
pub struct UIConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[validate(length(max = 10000))]
    pub instructions: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[validate(nested)]
    pub components: Vec<UIComponent>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, TS)]
#[ts(export)]
#[serde(untagged)]
pub enum ComponentValue {
    Boolean(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Deserialize)]
struct UIComponentRaw {
    // Core Identity
    #[serde(rename = "type")]
    component_type: String,
    #[serde(default)]
    key: Option<String>,

    // Display/UI
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    placeholder: Option<String>,

    // Behavior
    #[serde(default)]
    required: bool,
    #[serde(default)]
    bind: Option<String>,
    #[serde(default)]
    default_value: Option<ComponentValue>,

    // Text Constraints
    #[serde(default)]
    min_length: Option<u32>,
    #[serde(default)]
    max_length: Option<u32>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    suffix: Option<String>,
    #[serde(default = "default_true")]
    trim: bool,

    // Number Constraints
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    decimals: Option<u32>,

    // Select/Choice Options
    #[serde(default)]
    options: Option<Vec<SelectOption>>,
    #[serde(default)]
    multiple: bool,

    // Image/Layout
    #[serde(default)]
    width: Option<String>,
    #[serde(default)]
    height: Option<String>,
    #[serde(default)]
    aspect: Option<String>,
    #[serde(default)]
    fit: Option<String>,

    // Text Styling
    #[serde(default)]
    size: Option<TextSize>,
    #[serde(default)]
    color: Option<TextColor>,
    #[serde(default)]
    font: Option<FontFamily>,
}

impl From<UIComponentRaw> for UIComponent {
    fn from(raw: UIComponentRaw) -> Self {
        let key = if let Some(user_key) = raw.key {
            if !crate::utils::python_identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "UI Component has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    user_key
                );
            }
            user_key
        } else {
            // Generate key from label if available, otherwise from component type
            let base_name = raw.label.as_deref().unwrap_or(&raw.component_type);
            to_python_identifier(base_name)
        };
        UIComponent {
            // Core Identity
            component_type: raw.component_type,
            key,

            // Display/UI
            label: raw.label,
            description: raw.description,
            placeholder: raw.placeholder,

            // Behavior
            required: raw.required,
            bind: raw.bind,
            default_value: raw.default_value,

            // Text Constraints
            min_length: raw.min_length,
            max_length: raw.max_length,
            pattern: raw.pattern,
            prefix: raw.prefix,
            suffix: raw.suffix,
            trim: raw.trim,

            // Number Constraints
            min: raw.min,
            max: raw.max,
            step: raw.step,
            decimals: raw.decimals,

            // Select/Choice Options
            options: raw.options,
            multiple: raw.multiple,

            // Image/Layout
            width: raw.width,
            height: raw.height,
            aspect: raw.aspect,
            fit: raw.fit,

            // Text Styling
            size: raw.size,
            color: raw.color,
            font: raw.font,
        }
    }
}

#[derive(Debug, Serialize, Validate, Clone, TS)]
#[ts(export)]
pub struct UIComponent {
    // Core Identity
    #[serde(rename = "type")]
    #[validate(length(min = 1, max = 50))]
    pub component_type: String,

    #[validate(length(min = 1, max = 100))]
    pub key: String,

    // Display/UI
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub label: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 1000))]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub placeholder: Option<String>,

    // Behavior
    #[serde(default = "default_true")]
    pub required: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 200))]
    pub bind: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<ComponentValue>,

    // Text Constraints
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub pattern: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub suffix: Option<String>,
    #[serde(default = "default_true")]
    pub trim: bool,

    // Number Constraints
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub step: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub decimals: Option<u32>,

    // Select/Choice Options
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiple: bool,

    // Image/Layout
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub width: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub height: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub aspect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 50))]
    pub fit: Option<String>,

    // Text Styling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<TextSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<TextColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<FontFamily>,
}

impl<'de> Deserialize<'de> for UIComponent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = UIComponentRaw::deserialize(deserializer)?;
        Ok(raw.into())
    }
}

#[derive(Debug, Deserialize, Serialize, Validate, Clone, TS)]
#[ts(export)]
pub struct SelectOption {
    #[validate(length(max = 200))]
    pub value: String,
    #[validate(length(max = 200))]
    pub label: String,
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_ts_export() {
        assert!(true);
    }
}
