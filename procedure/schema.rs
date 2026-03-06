use crate::execution::events::PlugScope;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use validator::Validate;

pub const DEFAULT_WORKERS: usize = 8;

fn validate_python_identifier(key: &str) -> Result<(), validator::ValidationError> {
    if !crate::python::identifier::is_valid_python_identifier(key) {
        return Err(validator::ValidationError::new("invalid_python_identifier"));
    }
    Ok(())
}

fn validate_sub_units_config(config: &SubUnitsConfig) -> Result<(), validator::ValidationError> {
    // Validate items is not empty
    if config.0.is_empty() {
        let mut err = validator::ValidationError::new("empty_sub_units_items");
        err.message = Some("sub_units must have at least one item".into());
        return Err(err);
    }

    // Validate no duplicate keys (case-insensitive)
    let mut seen_keys: HashSet<String> = HashSet::new();
    for item in &config.0 {
        let key = item.get_key().to_lowercase();
        if seen_keys.contains(&key) {
            let mut err = validator::ValidationError::new("duplicate_sub_unit_key");
            err.message = Some(format!(
                "Duplicate sub-unit key '{}' (from label '{}')",
                key, item.label
            ).into());
            return Err(err);
        }
        seen_keys.insert(key);
    }

    // Validate keys produce valid Python identifiers
    for item in &config.0 {
        let key = item.get_key();
        if key.is_empty() || !crate::python::identifier::is_valid_python_identifier(&key) {
            let mut err = validator::ValidationError::new("invalid_sub_unit_key");
            err.message = Some(format!(
                "Sub-unit key '{}' (from label '{}') is not a valid Python identifier",
                key, item.label
            ).into());
            return Err(err);
        }
    }

    Ok(())
}

fn is_true(value: &bool) -> bool {
    *value
}

fn default_stop() -> FirstFailureAction {
    FirstFailureAction::Stop
}

fn default_strategy() -> ExecutionStrategy {
    ExecutionStrategy::PhaseFirst
}

fn default_workers() -> usize {
    DEFAULT_WORKERS
}

fn default_true() -> bool {
    true
}

fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();

    let mut total_ms = 0f64;
    let mut current_num = String::new();
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        let ch = chars[i];

        if ch.is_ascii_digit() {
            current_num.push(ch);
            i += 1;
        } else if ch == 'm' && i + 1 < chars.len() && chars[i + 1] == 's' {
            if current_num.is_empty() {
                return Err(format!("Invalid duration format: {}", s));
            }

            let num: f64 = current_num
                .parse()
                .map_err(|_| format!("Invalid number in duration: {}", current_num))?;

            total_ms += num;
            current_num.clear();
            i += 2;
        } else if ch == 's' || ch == 'm' || ch == 'h' {
            if current_num.is_empty() {
                return Err(format!("Invalid duration format: {}", s));
            }

            let num: f64 = current_num
                .parse()
                .map_err(|_| format!("Invalid number in duration: {}", current_num))?;

            total_ms += match ch {
                's' => num * 1000.0,
                'm' => num * 60.0 * 1000.0,
                'h' => num * 3600.0 * 1000.0,
                _ => unreachable!(),
            };

            current_num.clear();
            i += 1;
        } else if !ch.is_whitespace() {
            return Err(format!(
                "Invalid character '{}' in duration '{}'. Valid units are: ms, s, m, h",
                ch, s
            ));
        } else {
            i += 1;
        }
    }

    if !current_num.is_empty() {
        return Err(format!(
            "Duration '{}' is missing a unit. Valid units are: ms (milliseconds), s (seconds), m (minutes), h (hours). Examples: 500ms, 30s, 5m, 1h30m",
            s
        ));
    }

    if total_ms == 0.0 {
        return Err("Duration cannot be zero".to_string());
    }

    Ok(total_ms.round() as u64)
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms);
    }

    let total_seconds = ms / 1000;
    let remaining_ms = ms % 1000;

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if secs > 0 {
        parts.push(format!("{}s", secs));
    }
    if remaining_ms > 0 {
        parts.push(format!("{}ms", remaining_ms));
    }

    parts.join("")
}

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DurationValue {
        String(String),
        Number(u64),
    }

    let input: Option<DurationValue> = Option::deserialize(deserializer)?;
    match input {
        None => Ok(None),
        Some(DurationValue::String(s)) => parse_duration(&s).map(Some).map_err(Error::custom),
        Some(DurationValue::Number(ms)) => Ok(Some(ms)),
    }
}

fn serialize_duration<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) => serializer.serialize_u64(*ms),
    }
}

fn serialize_duration_as_string<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) => serializer.serialize_str(&format_duration(*ms)),
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStage {
    Setup,
    Main,
    Teardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    All,
    Each,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum FirstFailureAction {
    Stop,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    PhaseFirst,
    SlotFirst,
}

// Internal enum for execution engine
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, specta::Type)]
pub enum StageScope {
    SetupAll,
    SetupEach,
    Main,
    TeardownEach,
    TeardownAll,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcedureYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub name: String,

    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub version: String,

    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "serde_trim::string_trim")]
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<UnitConfig>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugs: Vec<PlugDefinitionYaml>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<PhaseDefinitionYaml>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<PhaseDefinitionYaml>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<PhaseDefinitionYaml>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Validate, specta::Type)]
pub struct ProcedureDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    #[validate(length(min = 1, max = 50))]
    pub version: String,

    #[serde(default)]
    #[validate(length(max = 50000))]
    pub description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub execution: Option<ExecutionConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
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
        ProcedureDefinition {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            description: raw.description,
            execution: raw.execution,
            unit: raw.unit,
            plugs: raw.plugs.into_iter().map(|p| p.into()).collect(),
            setup: raw.setup.into_iter().map(|p| p.into()).collect(),
            main: raw.main.into_iter().map(|p| p.into()).collect(),
            teardown: raw.teardown.into_iter().map(|p| p.into()).collect(),
        }
    }
}

impl ProcedureDefinition {
    pub fn to_yaml(&self) -> ProcedureYaml {
        ProcedureYaml {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            execution: self.execution.clone(),
            unit: self.unit.clone(),
            plugs: self.plugs.iter().map(|p| p.to_yaml()).collect(),
            setup: self.setup.iter().map(|p| p.to_yaml()).collect(),
            main: self.main.iter().map(|p| p.to_yaml()).collect(),
            teardown: self.teardown.iter().map(|p| p.to_yaml()).collect(),
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
            if !crate::python::identifier::is_valid_python_identifier(&user_key) {
                log::warn!(
                    "Slot '{}' has invalid key '{}' (not a valid Python identifier). Consider using auto-generated key instead.",
                    raw.name,
                    user_key
                );
            }
            user_key
        } else {
            crate::python::identifier::to_python_identifier(&raw.name)
        };
        SlotConfig {
            key,
            name: raw.name,
        }
    }
}

#[derive(Debug, Validate, Clone, Serialize, specta::Type)]
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

#[derive(Debug, Serialize, Deserialize, Clone, specta::Type)]
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

/// Configuration for a specific sub-unit with custom label and constraints
#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct SubUnitItemConfig {
    /// Display label for this sub-unit (e.g., "Battery", "Motor")
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub label: String,

    /// Internal key for this sub-unit (e.g., "battery", "motor")
    /// Used for binding, reports, and internal storage
    /// If not provided, auto-generated from label (lowercase, sanitized)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,

    /// Optional serial number constraints for this specific sub-unit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<UnitFieldConfig>,
}

impl serde::Serialize for SubUnitItemConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SubUnitItemConfig", 3)?;
        state.serialize_field("label", &self.label)?;
        // Always serialize the computed key
        state.serialize_field("key", &self.get_key())?;
        state.serialize_field("serial_number", &self.serial_number)?;
        state.end()
    }
}

impl SubUnitItemConfig {
    /// Get the key for this sub-unit, auto-generating from label if not provided
    pub fn get_key(&self) -> String {
        self.key.clone().unwrap_or_else(|| {
            // Auto-generate key from label: lowercase and sanitize
            crate::python::identifier::to_python_identifier(&self.label.to_lowercase())
        })
    }
}

/// Configuration for sub-units collection (transparent wrapper for direct array serialization)
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(transparent)]
pub struct SubUnitsConfig(pub Vec<SubUnitItemConfig>);

impl validator::Validate for SubUnitsConfig {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        validate_sub_units_config(self).map_err(|e| {
            let mut errors = validator::ValidationErrors::new();
            errors.add("sub_units", e);
            errors
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, Validate, specta::Type)]
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

    /// Sub-units configuration (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub sub_units: Option<SubUnitsConfig>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, specta::Type)]
pub struct ExecutionConfig {
    #[serde(default = "default_strategy")]
    pub strategy: ExecutionStrategy,

    #[serde(default = "default_workers")]
    #[validate(range(min = 1, max = 256))]
    pub workers: usize,

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
            workers: 8,
            on_first_failure: FirstFailureAction::Stop,
            slots: vec![],
        }
    }
}

impl ExecutionConfig {
    /// Validate consistency between global execution config and phase-level then configs
    /// Returns warnings for potentially conflicting configurations
    pub fn validate_consistency(&self, phases: &[&PhaseDefinition]) -> Vec<String> {
        let mut warnings = Vec::new();

        for phase in phases {
            if let Some(then_config) = &phase.then {
                // Warning: on_first_failure: stop + then.fail: continue (conflict)
                if matches!(self.on_first_failure, FirstFailureAction::Stop) {
                    if let Some(PhaseNextAction::Continue) = then_config.fail {
                        warnings.push(format!(
                            "Phase '{}': on_first_failure is 'stop' but then.fail is 'continue'. \
                             Explicit then.fail will override global setting.",
                            phase.name
                        ));
                    }
                }

            }
        }

        warnings
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
pub enum ParallelMode {
    #[default]
    Sequential,
    Batch,
    Independent,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, specta::Type)]
pub struct RetryConfig {
    #[validate(range(min = 1, max = 10))]
    pub limit: usize,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration"
    )]
    pub delay: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RetryConfigYaml {
    pub limit: usize,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration_as_string"
    )]
    pub delay: Option<u64>,
}

impl From<RetryConfigYaml> for RetryConfig {
    fn from(yaml: RetryConfigYaml) -> Self {
        RetryConfig {
            limit: yaml.limit,
            delay: yaml.delay,
        }
    }
}

impl From<&RetryConfig> for RetryConfigYaml {
    fn from(config: &RetryConfig) -> Self {
        RetryConfigYaml {
            limit: config.limit,
            delay: config.delay,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum PhaseNextAction {
    Continue,
    Stop,
    Retry,
    Skip,
}

#[derive(Debug, Serialize, Deserialize, Validate, Clone, specta::Type)]
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

// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PlugDefinitionYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    pub python: PythonSpec,
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "serde_trim::string_trim")]
    pub description: String,
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub struct PlugDefinition {
    #[validate(length(min = 1, max = 100), custom(function = "validate_python_identifier"))]
    pub key: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,

    pub scope: Scope,

    pub python: PythonSpec,

    #[validate(length(max = 50000))]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl From<PlugDefinitionYaml> for PlugDefinition {
    fn from(yaml: PlugDefinitionYaml) -> Self {
        let key = yaml.key.unwrap_or_else(|| {
            crate::python::identifier::to_python_identifier(&yaml.name)
        });

        PlugDefinition {
            key,
            name: yaml.name,
            scope: yaml.scope.unwrap_or(Scope::Each),
            python: yaml.python,
            description: yaml.description,
        }
    }
}

impl PlugDefinition {
    pub fn to_yaml(&self) -> PlugDefinitionYaml {
        let auto_key = crate::python::identifier::to_python_identifier(&self.name);
        PlugDefinitionYaml {
            key: if self.key != auto_key { Some(self.key.clone()) } else { None },
            name: self.name.clone(),
            scope: Some(self.scope),
            python: self.python.clone(),
            description: self.description.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for PlugDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = PlugDefinitionYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

impl PlugDefinition {
    pub fn to_config_json(&self, project_dir: &Path) -> Result<serde_json::Value, String> {
        let (file_path, callable_name) = self.python.parse(project_dir)?;
        Ok(serde_json::json!({
            "file": file_path.to_string_lossy(),
            "class": callable_name
        }))
    }

    pub fn scope_is_all(&self) -> bool {
        matches!(self.scope, Scope::All)
    }
}

/// Python specification: "path/to/file:callable_name" or "path.to.file:callable_name"
///
/// Examples:
/// - "phases/test_voltage" → phases/test_voltage.py::test_voltage()
/// - "phases.calibration:setup" → phases/calibration.py::setup()
/// - "plugs.python.ethercat:EtherCATManager" → plugs/python/ethercat.py::EtherCATManager
#[derive(Debug, Serialize, Deserialize, Clone, specta::Type)]
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

        // Handle Windows absolute paths (e.g., "C:/path/to/file.py:func")
        // Windows paths have a drive letter followed by colon, so we may have 3 parts
        let (path_part, name_part) = if parts.len() == 3
            && parts[0].len() == 1
            && parts[0].chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        {
            // Windows absolute path: "C:/path/file.py:func" -> ["C", "/path/file.py", "func"]
            let path = format!("{}:{}", parts[0], parts[1]);
            (path, Some(parts[2].trim()))
        } else if parts.len() > 2 {
            return Err(format!(
                "Invalid spec format '{}': too many ':' separators",
                spec
            ));
        } else {
            (parts[0].trim().to_string(), parts.get(1).map(|s| s.trim()))
        };

        // Validate path not empty
        if path_part.is_empty() {
            return Err("Path cannot be empty".into());
        }

        // Check if path uses slash syntax (file path) or dot syntax (module path)
        let is_file_path = path_part.contains('/') || path_part.contains('\\');

        let file_path = if is_file_path {
            // File path syntax: resolve relative to project_dir, allow ..
            let expanded_path = if path_part.starts_with('~') {
                if let Some(home) = dirs::home_dir() {
                    home.join(&path_part[1..].trim_start_matches('/'))
                } else {
                    return Err("Cannot expand ~ - home directory not found".into());
                }
            } else {
                project_dir.join(&path_part)
            };

            // Canonicalize to resolve .. and symlinks
            let resolved = expanded_path.canonicalize().unwrap_or(expanded_path);

            // Ensure .py extension
            if resolved.extension().map(|e| e == "py").unwrap_or(false) {
                resolved
            } else {
                resolved.with_extension("py")
            }
        } else {
            // Module path syntax (dots): no .. allowed, resolve relative to project_dir
            if path_part.contains("..") {
                return Err(format!(
                    "Parent directory traversal not allowed in module paths. Use file path syntax instead: '{}'",
                    path_part
                ));
            }

            // Convert "plugs.python.ethercat" → "plugs/python/ethercat.py"
            project_dir
                .join(path_part.replace('.', "/"))
                .with_extension("py")
        };

        // Get callable name (or default to last component of path)
        let callable_name = if let Some(name) = name_part {
            if name.is_empty() {
                return Err("Callable name cannot be empty after ':'".into());
            }
            name.to_string()
        } else {
            // Default to file stem (filename without .py)
            file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&path_part)
                .to_string()
        };

        // Validate file exists
        if !file_path.exists() {
            return Err(format!("Python file not found: {}", file_path.display()));
        }

        // Validate callable name is valid Python identifier
        if !crate::python::identifier::is_valid_python_identifier(&callable_name) {
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

#[derive(Debug, Serialize, Deserialize, Validate, Clone, specta::Type)]
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
// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PhaseDefinitionYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<ExecutableConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<MeasurementSpecYaml>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UIConfig>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_duration",
        serialize_with = "serialize_duration_as_string"
    )]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfigYaml>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub struct PhaseDefinition {
    #[validate(length(min = 1, max = 100), custom(function = "validate_python_identifier"))]
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

    #[validate(length(max = 1000))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<MeasurementSpec>,

    #[validate(nested)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UIConfig>,

    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,

    #[validate(length(max = 50))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,

    #[validate(length(max = 100))]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

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

impl From<PhaseDefinitionYaml> for PhaseDefinition {
    fn from(yaml: PhaseDefinitionYaml) -> Self {
        let key = yaml.key.unwrap_or_else(|| {
            crate::python::identifier::to_python_identifier(&yaml.name)
        });

        PhaseDefinition {
            key,
            name: yaml.name,
            scope: yaml.scope,
            python: yaml.python,
            executable: yaml.executable,
            description: yaml.description,
            measurements: yaml.measurements.into_iter().map(|m| m.into()).collect(),
            ui: yaml.ui,
            enabled: yaml.enabled,
            result: yaml.result,
            depends_on: yaml.depends_on,
            timeout: yaml.timeout,
            retry: yaml.retry.map(|r| r.into()),
            then: yaml.then,
        }
    }
}

impl PhaseDefinition {
    pub fn to_yaml(&self) -> PhaseDefinitionYaml {
        let auto_key = crate::python::identifier::to_python_identifier(&self.name);
        PhaseDefinitionYaml {
            key: if self.key != auto_key { Some(self.key.clone()) } else { None },
            name: self.name.clone(),
            scope: self.scope,
            python: self.python.clone(),
            executable: self.executable.clone(),
            description: self.description.clone(),
            measurements: self.measurements.iter().map(|m| m.to_yaml()).collect(),
            ui: self.ui.clone(),
            enabled: self.enabled,
            result: self.result.clone(),
            depends_on: self.depends_on.clone(),
            timeout: self.timeout,
            retry: self.retry.as_ref().map(|r| r.into()),
            then: self.then.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for PhaseDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = PhaseDefinitionYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

impl PhaseDefinition {
    pub fn should_skip(&self) -> bool {
        !self.enabled || self.result.as_deref() == Some("skip")
    }

    pub fn to_python_identifier(&self) -> String {
        crate::python::identifier::to_python_identifier(&self.name)
    }

    pub fn get_key(&self) -> String {
        self.key.clone()
    }

    pub fn get_display_name(&self) -> String {
        self.name.clone()
    }


    pub fn validate_single_runtime(&self) -> Result<(), String> {
        let has_python = self.python.is_some();
        let has_executable = self.executable.is_some();

        if has_python && has_executable {
            return Err(format!(
                "Phase '{}' has both python and executable runtimes defined (only one allowed)",
                self.name
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
        self.setup
            .iter()
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
            let key = phase.key.clone();
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
                let scope = if plug.scope == Scope::All {
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

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
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

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum TextColor {
    Default,
    Zinc,
    Red,
    Orange,
    Yellow,
    Green,
    Lime,
    Sky,
    Blue,
    Violet,
    Purple,
    Pink,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum FontFamily {
    Default,
    Monospace,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ValidatorLevel {
    Critical,
    Alert,
    Notice,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[serde(rename_all = "UPPERCASE")]
pub enum ValidatorOutcome {
    Pass,
    Fail,
    Unset,
}

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
#[serde(untagged)]
pub enum ValidatorExpectedValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    NumberArray(Vec<f64>),
    StringArray(Vec<String>),
    #[specta(skip)]
    MixedArray(Vec<serde_json::Value>),
    #[specta(skip)]
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
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

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
#[serde(untagged)]
pub enum AggregationValue {
    Number(f64),
    String(String),
    Boolean(bool),
    #[specta(skip)]
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
pub struct AggregationSpec {
    #[serde(rename = "type")]
    pub aggregation_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ValidatorOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<AggregationValue>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
#[serde(untagged)]
pub enum AxisData {
    Numeric(Vec<f64>),
    String(Vec<String>),
}

#[derive(Debug, Deserialize, Clone, specta::Type)]
pub struct AxisSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AxisData>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub legend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub description: Option<String>,
}

impl serde::Serialize for AxisSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        // Count non-None fields for struct size hint
        let mut count = 0;
        if self.data.is_some() { count += 1; }
        if self.get_key().is_some() { count += 1; }
        if self.get_legend().is_some() { count += 1; }
        if self.unit.is_some() { count += 1; }
        if self.aggregations.is_some() { count += 1; }
        if self.validators.is_some() { count += 1; }
        if self.description.is_some() { count += 1; }

        let mut state = serializer.serialize_struct("AxisSpec", count)?;
        if let Some(ref data) = self.data {
            state.serialize_field("data", data)?;
        }
        if let Some(ref unit) = self.unit {
            state.serialize_field("unit", unit)?;
        }
        // Always serialize resolved legend and key
        if let Some(legend) = self.get_legend() {
            state.serialize_field("legend", &legend)?;
        }
        if let Some(key) = self.get_key() {
            state.serialize_field("key", &key)?;
        }
        if let Some(ref aggs) = self.aggregations {
            state.serialize_field("aggregations", aggs)?;
        }
        if let Some(ref vals) = self.validators {
            state.serialize_field("validators", vals)?;
        }
        if let Some(ref desc) = self.description {
            state.serialize_field("description", desc)?;
        }
        state.end()
    }
}

impl AxisSpec {
    /// Returns the explicit key, or auto-generates one from legend via to_python_identifier
    pub fn get_key(&self) -> Option<String> {
        if let Some(ref key) = self.key {
            if !key.is_empty() {
                return Some(key.clone());
            }
        }
        if let Some(ref legend) = self.legend {
            let generated = crate::python::identifier::to_python_identifier(legend);
            if !generated.is_empty() {
                return Some(generated);
            }
        }
        None
    }

    /// Returns the explicit legend, or auto-generates one from key (title-cased)
    pub fn get_legend(&self) -> Option<String> {
        if let Some(ref legend) = self.legend {
            if !legend.is_empty() {
                return Some(legend.clone());
            }
        }
        if let Some(ref key) = self.key {
            if !key.is_empty() {
                // Title-case: "voltage_output" -> "Voltage Output"
                return Some(
                    key.split('_')
                        .filter(|s| !s.is_empty())
                        .map(|word| {
                            let mut chars = word.chars();
                            match chars.next() {
                                Some(c) => {
                                    c.to_uppercase().to_string() + &chars.collect::<String>()
                                }
                                None => String::new(),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        None
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, specta::Type)]
pub struct MultiDimensionalSpec {
    pub x_axis: AxisSpec,
    pub y_axis: Vec<AxisSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub title: Option<String>,
}

// YAML representation (key optional for clean YAML files)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MeasurementSpecYaml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validators: Option<Vec<ValidatorSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregations: Option<Vec<AggregationSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "serde_trim::option_string_trim")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_axis: Option<AxisSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<Vec<AxisSpec>>,
}

// Runtime representation (key always present)
#[derive(Debug, Validate, Clone, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub struct MeasurementSpec {
    #[validate(length(min = 1, max = 100), custom(function = "validate_python_identifier"))]
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

    #[validate(length(max = 200))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_axis: Option<AxisSpec>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<Vec<AxisSpec>>,
}

impl From<MeasurementSpecYaml> for MeasurementSpec {
    fn from(yaml: MeasurementSpecYaml) -> Self {
        let trimmed = yaml.name.trim();
        let name = if trimmed.is_empty() {
            yaml.name
        } else {
            trimmed.to_string()
        };
        let key = yaml.key.unwrap_or_else(|| {
            crate::python::identifier::to_python_identifier(&name)
        });

        MeasurementSpec {
            key,
            name,
            unit: yaml.unit,
            validators: yaml.validators,
            aggregations: yaml.aggregations,
            description: yaml.description,
            title: yaml.title,
            x_axis: yaml.x_axis,
            y_axis: yaml.y_axis,
        }
    }
}

impl MeasurementSpec {
    pub fn to_yaml(&self) -> MeasurementSpecYaml {
        let auto_key = crate::python::identifier::to_python_identifier(&self.name);
        MeasurementSpecYaml {
            key: if self.key != auto_key { Some(self.key.clone()) } else { None },
            name: self.name.clone(),
            unit: self.unit.clone(),
            validators: self.validators.clone(),
            aggregations: self.aggregations.clone(),
            description: self.description.clone(),
            title: self.title.clone(),
            x_axis: self.x_axis.clone(),
            y_axis: self.y_axis.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for MeasurementSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = MeasurementSpecYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Default, Clone, specta::Type)]
#[specta(rename = "ProcedureUiConfig")]
pub struct UIConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub components: Option<Vec<UIComponent>>,

    /// Override whether this UI requires user input (shows Continue button).
    /// If not set, auto-detected from component types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_input: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[specta(rename = "ProcedureComponentValue", export = false)]
#[serde(untagged)]
pub enum ComponentValue {
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UIComponentYaml {
    // Core Identity
    #[serde(rename = "type")]
    component_type: UIComponentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,

    // Display/UI
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    label: Option<String>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    description: Option<String>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
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
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    prefix: Option<String>,
    #[serde(default, deserialize_with = "serde_trim::option_string_trim")]
    suffix: Option<String>,
    #[serde(default = "default_true")]
    trim: bool,

    // Textarea-specific
    #[serde(default)]
    rows: Option<u32>,

    // Number Constraints
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,

    // Select/Choice Options
    #[serde(default)]
    options: Option<Vec<SelectOption>>,

    // Grid columns (for image_choice / image_checklist)
    #[serde(default)]
    columns: Option<u32>,

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

impl From<UIComponentYaml> for UIComponent {
    fn from(yaml: UIComponentYaml) -> Self {
        let type_str = format!("{:?}", yaml.component_type);
        let base_name = yaml.label.as_deref().unwrap_or(&type_str);
        let key = yaml.key.unwrap_or_else(|| crate::python::identifier::to_python_identifier(base_name));

        UIComponent {
            // Core Identity
            component_type: yaml.component_type,
            key,

            // Display/UI
            label: yaml.label,
            description: yaml.description,
            placeholder: yaml.placeholder,

            // Behavior
            required: yaml.required,
            bind: yaml.bind,
            default_value: yaml.default_value,

            // Text Constraints
            min_length: yaml.min_length,
            max_length: yaml.max_length,
            pattern: yaml.pattern,
            prefix: yaml.prefix,
            suffix: yaml.suffix,
            trim: yaml.trim,
            rows: yaml.rows,

            // Number Constraints
            min: yaml.min,
            max: yaml.max,
            step: yaml.step,
            // Select/Choice Options
            options: yaml.options,

            // Grid columns
            columns: yaml.columns,

            // Image/Layout
            width: yaml.width,
            height: yaml.height,
            aspect: yaml.aspect,
            fit: yaml.fit,

            // Text Styling
            size: yaml.size,
            color: yaml.color,
            font: yaml.font,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum UIComponentType {
    TextInput,
    NumberInput,
    Switch,
    Textarea,
    Radio,
    Select,
    Multiselect,
    Checklist,
    Slider,
    ImageChoice,
    ImageChecklist,
    Text,
    Image,
    Progress,
    ArtificialHorizon,
}

#[derive(Debug, Serialize, Validate, Clone, specta::Type)]
#[specta(rename = "ProcedureUiComponent", export = false)]
pub struct UIComponent {
    // Core Identity
    #[serde(rename = "type")]
    pub component_type: UIComponentType,

    #[validate(length(min = 1, max = 100), custom(function = "validate_python_identifier"))]
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

    // Textarea-specific
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,

    // Number Constraints
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub step: Option<f64>,

    // Select/Choice Options
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub options: Option<Vec<SelectOption>>,

    // Grid columns (for image_choice / image_checklist)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,

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

impl UIComponent {
    pub fn to_yaml(&self) -> UIComponentYaml {
        let type_str = format!("{:?}", self.component_type);
        let base_name = self.label.as_deref().unwrap_or(&type_str);
        let auto_key = crate::python::identifier::to_python_identifier(base_name);

        UIComponentYaml {
            component_type: self.component_type.clone(),
            key: if self.key != auto_key { Some(self.key.clone()) } else { None },
            label: self.label.clone(),
            description: self.description.clone(),
            placeholder: self.placeholder.clone(),
            required: self.required,
            bind: self.bind.clone(),
            default_value: self.default_value.clone(),
            min_length: self.min_length,
            max_length: self.max_length,
            pattern: self.pattern.clone(),
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
            trim: self.trim,
            rows: self.rows,
            min: self.min,
            max: self.max,
            step: self.step,
            options: self.options.clone(),
            columns: self.columns,
            width: self.width.clone(),
            height: self.height.clone(),
            aspect: self.aspect.clone(),
            fit: self.fit.clone(),
            size: self.size.clone(),
            color: self.color.clone(),
            font: self.font.clone(),
        }
    }

    pub fn validate_width(&self) -> Result<(), String> {
        if let Some(ref w) = self.width {
            let trimmed = w.trim();
            if !trimmed.ends_with('%') {
                return Err(format!(
                    "Component '{}': width must be a percentage (e.g. '50%'), got '{}'",
                    self.key, w
                ));
            }
            let num_part = &trimmed[..trimmed.len() - 1];
            match num_part.parse::<f64>() {
                Ok(v) if v > 0.0 && v <= 100.0 => {}
                _ => {
                    return Err(format!(
                        "Component '{}': width must be between 0% and 100%, got '{}'",
                        self.key, w
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn validate_aspect(&self) -> Result<(), String> {
        if let Some(ref a) = self.aspect {
            let trimmed = a.trim();
            if trimmed.is_empty() {
                return Ok(());
            }
            match trimmed {
                "16/9" | "4/3" | "3/4" | "2/3" | "9/16" | "square" | "auto" => {}
                _ => {
                    return Err(format!(
                        "Component '{}': aspect must be one of '16/9', '4/3', '3/4', '2/3', '9/16', 'square', 'auto', got '{}'",
                        self.key, a
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn validate_fit(&self) -> Result<(), String> {
        if let Some(ref f) = self.fit {
            let trimmed = f.trim();
            if trimmed.is_empty() {
                return Ok(());
            }
            match trimmed {
                "contain" | "cover" | "fill" => {}
                _ => {
                    return Err(format!(
                        "Component '{}': fit must be one of 'contain', 'cover', 'fill', got '{}'",
                        self.key, f
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn validate_options_count(&self) -> Result<(), String> {
        if matches!(
            self.component_type,
            UIComponentType::ImageChoice | UIComponentType::ImageChecklist
        ) {
            if let Some(opts) = &self.options {
                if opts.len() > 12 {
                    return Err(format!(
                        "Component '{}' has {} options (max 12 for image_choice/image_checklist)",
                        self.key,
                        opts.len()
                    ));
                }
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for UIComponent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let yaml = UIComponentYaml::deserialize(deserializer)?;
        Ok(yaml.into())
    }
}

#[derive(Debug, Deserialize, Serialize, Validate, Clone, specta::Type)]
pub struct SelectOption {
    #[validate(length(max = 200))]
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub value: String,
    #[validate(length(max = 200))]
    #[serde(deserialize_with = "serde_trim::string_trim")]
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[validate(length(max = 500))]
    pub image: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ts_export() {
        assert!(true);
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), 30_000);
        assert_eq!(parse_duration("1s").unwrap(), 1_000);
        assert_eq!(parse_duration("90s").unwrap(), 90_000);
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), 300_000);
        assert_eq!(parse_duration("1m").unwrap(), 60_000);
        assert_eq!(parse_duration("10m").unwrap(), 600_000);
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), 3_600_000);
        assert_eq!(parse_duration("2h").unwrap(), 7_200_000);
    }

    #[test]
    fn test_parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), 500);
        assert_eq!(parse_duration("1000ms").unwrap(), 1_000);
        assert_eq!(parse_duration("100ms").unwrap(), 100);
    }

    #[test]
    fn test_parse_duration_combined() {
        assert_eq!(parse_duration("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration("2h15m30s").unwrap(), 8_130_000);
        assert_eq!(parse_duration("1m30s").unwrap(), 90_000);
        assert_eq!(parse_duration("1h500ms").unwrap(), 3_600_500);
    }

    #[test]
    fn test_parse_duration_with_whitespace() {
        assert_eq!(parse_duration("  30s  ").unwrap(), 30_000);
        assert_eq!(parse_duration("5m ").unwrap(), 300_000);
    }

    #[test]
    fn test_parse_duration_invalid_no_unit() {
        let result = parse_duration("30");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing a unit"));
    }

    #[test]
    fn test_parse_duration_invalid_zero() {
        let result = parse_duration("0s");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Duration cannot be zero");
    }

    #[test]
    fn test_parse_duration_invalid_character() {
        let result = parse_duration("30x");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid character"));
    }

    #[test]
    fn test_parse_duration_empty() {
        let result = parse_duration("");
        assert!(result.is_err());
    }

    #[test]
    fn test_format_duration_milliseconds() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(999), "999ms");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(30_000), "30s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60_000), "1m");
        assert_eq!(format_duration(300_000), "5m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3_600_000), "1h");
        assert_eq!(format_duration(7_200_000), "2h");
    }

    #[test]
    fn test_format_duration_combined() {
        assert_eq!(format_duration(5_400_000), "1h30m");
        assert_eq!(format_duration(90_000), "1m30s");
        assert_eq!(format_duration(3_661_000), "1h1m1s");
        assert_eq!(format_duration(3_600_500), "1h500ms");
    }

    #[test]
    fn test_duration_roundtrip() {
        let inputs = vec!["30s", "5m", "1h", "1h30m", "500ms", "2h15m30s"];

        for input in inputs {
            let parsed = parse_duration(input).unwrap();
            let formatted = format_duration(parsed);
            let reparsed = parse_duration(&formatted).unwrap();
            assert_eq!(parsed, reparsed, "Roundtrip failed for input: {}", input);
        }
    }

    #[test]
    fn test_validate_python_identifier_valid() {
        assert!(validate_python_identifier("valid_name").is_ok());
        assert!(validate_python_identifier("test123").is_ok());
        assert!(validate_python_identifier("_private").is_ok());
        assert!(validate_python_identifier("CamelCase").is_ok());
    }

    #[test]
    fn test_validate_python_identifier_invalid() {
        assert!(validate_python_identifier("123invalid").is_err());
        assert!(validate_python_identifier("invalid-name").is_err());
        assert!(validate_python_identifier("invalid name").is_err());
        assert!(validate_python_identifier("").is_err());
    }
}
