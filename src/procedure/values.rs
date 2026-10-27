//! `config:` and `secrets:`: the values a procedure declares, the run context
//! the host knows, and their resolution when a run starts.
//!
//! The procedure (and each sequence file it calls) declares the keys it
//! reads, with a type and whether a value is required. The host (the CLI)
//! finds the values (a `.env`, the shell, the station, the server) and
//! passes them as [`ResolvedValues`], each tagged with where it came from.
//! [`resolve_values`] checks them against the declarations, applies the
//! defaults, and refuses a run whose required values are missing with a
//! [`ConfigMissing`] the host can recognise, before the unit is identified.
//! The result, [`RunValues`], is what the orchestrator expands markers with
//! and hands to the phases.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::markers::{MarkerValues, RUN_FIELDS};
use super::schema::ProcedureDefinition;

/// The type of a declared config value or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    #[default]
    String,
    Number,
    Integer,
    Boolean,
}

impl ValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            ValueType::String => "string",
            ValueType::Number => "number",
            ValueType::Integer => "integer",
            ValueType::Boolean => "boolean",
        }
    }

    /// `a string`, `an integer`: the type in a sentence.
    pub fn with_article(self) -> &'static str {
        match self {
            ValueType::String => "a string",
            ValueType::Number => "a number",
            ValueType::Integer => "an integer",
            ValueType::Boolean => "a boolean",
        }
    }

    /// `value` as this type: a value of the type as is, a whole float for
    /// an integer as the integer, and a string read as the type (values
    /// from a `.env` or the environment are always text). `None` when it
    /// is not one.
    pub fn coerce(self, value: &serde_json::Value) -> Option<serde_json::Value> {
        use serde_json::Value;
        let text = value.as_str().map(str::trim);
        match self {
            ValueType::String => match value {
                Value::String(_) => Some(value.clone()),
                Value::Number(n) => Some(Value::String(n.to_string())),
                Value::Bool(b) => Some(Value::String(b.to_string())),
                _ => None,
            },
            ValueType::Number => match (value, text) {
                (Value::Number(_), _) => Some(value.clone()),
                (_, Some(t)) => t
                    .parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .and_then(|f| {
                        t.parse::<i64>()
                            .map(Value::from)
                            .ok()
                            .or_else(|| serde_json::Number::from_f64(f).map(Value::Number))
                    }),
                _ => None,
            },
            ValueType::Integer => match (value, text) {
                (Value::Number(n), _) if n.is_i64() || n.is_u64() => Some(value.clone()),
                (Value::Number(n), _) => n
                    .as_f64()
                    .filter(|f| f.fract() == 0.0 && f.is_finite())
                    .map(|f| Value::from(f as i64)),
                (_, Some(t)) => t.parse::<i64>().ok().map(Value::from),
                _ => None,
            },
            ValueType::Boolean => match (value, text) {
                (Value::Bool(_), _) => Some(value.clone()),
                (_, Some(t)) => match t.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Some(Value::Bool(true)),
                    "false" | "0" | "no" | "off" => Some(Value::Bool(false)),
                    _ => None,
                },
                _ => None,
            },
        }
    }
}

/// One `config:` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct ConfigSpecYaml {
    /// string (default), number, integer or boolean.
    #[serde(rename = "type", default, skip_serializing_if = "is_string")]
    pub value_type: ValueType,
    /// true: no run starts without a value (the station shows "not
    /// configured"). Default false: a missing value is null.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Value when none is set, of the declared type. Not with required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Free text, shown where the value is set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
}

/// One `secrets:` declaration. A secret has no default: its value never
/// lives in the repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct SecretSpecYaml {
    /// string (default), number, integer or boolean.
    #[serde(rename = "type", default, skip_serializing_if = "is_string")]
    pub value_type: ValueType,
    /// true: no run starts without a value. Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Free text, shown where the secret is set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
}

fn is_string(t: &ValueType) -> bool {
    *t == ValueType::String
}

/// The config and secrets one phase of a sequence file reads: the ones its
/// file declares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueScope {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub config: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub secrets: BTreeSet<String>,
}

/// Config value or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Config,
    Secret,
}

impl ValueKind {
    /// The namespace a marker and a phase read it under.
    pub fn namespace(self) -> &'static str {
        match self {
            ValueKind::Config => "config",
            ValueKind::Secret => "secrets",
        }
    }
}

/// One declared key, with its kind. Displays as `config.X` / `secrets.X`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ValueKey {
    pub kind: ValueKind,
    pub key: String,
}

impl std::fmt::Display for ValueKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.kind.namespace(), self.key)
    }
}

/// A declaration as the resolution reads it, config value or secret.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueDeclaration {
    pub value_type: ValueType,
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
}

impl From<&ConfigSpecYaml> for ValueDeclaration {
    fn from(spec: &ConfigSpecYaml) -> Self {
        Self {
            value_type: spec.value_type,
            required: spec.required,
            default: spec.default.clone(),
            description: spec.description.clone(),
        }
    }
}

impl From<&SecretSpecYaml> for ValueDeclaration {
    fn from(spec: &SecretSpecYaml) -> Self {
        Self {
            value_type: spec.value_type,
            required: spec.required,
            default: None,
            description: spec.description.clone(),
        }
    }
}

/// Every key the procedure and its sequence files declare, merged: what
/// the host resolves. A key is required when any declaration requires it;
/// the procedure's default wins over a sequence file's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeclaredValues {
    pub config: BTreeMap<String, ValueDeclaration>,
    pub secrets: BTreeMap<String, ValueDeclaration>,
}

impl DeclaredValues {
    pub fn of(&self, kind: ValueKind) -> &BTreeMap<String, ValueDeclaration> {
        match kind {
            ValueKind::Config => &self.config,
            ValueKind::Secret => &self.secrets,
        }
    }

    /// Every declared key, config first.
    pub fn keys(&self) -> Vec<ValueKey> {
        let config = self.config.keys().map(|k| ValueKey {
            kind: ValueKind::Config,
            key: k.clone(),
        });
        let secrets = self.secrets.keys().map(|k| ValueKey {
            kind: ValueKind::Secret,
            key: k.clone(),
        });
        config.chain(secrets).collect()
    }

    fn merge(&mut self, kind: ValueKind, key: &str, declaration: ValueDeclaration) {
        let map = match kind {
            ValueKind::Config => &mut self.config,
            ValueKind::Secret => &mut self.secrets,
        };
        match map.get_mut(key) {
            None => {
                map.insert(key.to_string(), declaration);
            }
            Some(existing) => {
                existing.required |= declaration.required;
                if existing.default.is_none() {
                    existing.default = declaration.default;
                }
                if existing.description.is_none() {
                    existing.description = declaration.description;
                }
            }
        }
        if let Some(d) = map.get_mut(key) {
            // A default makes a required value never missing: the merged
            // declaration keeps the stricter reading.
            if d.required {
                d.default = None;
            }
        }
    }
}

impl ProcedureDefinition {
    /// The keys the procedure and every sequence file it calls declare,
    /// merged (see `DeclaredValues`). What the host resolves for a run.
    pub fn declared_values(&self) -> DeclaredValues {
        let mut out = DeclaredValues::default();
        for (key, spec) in &self.config {
            out.merge(ValueKind::Config, key, spec.into());
        }
        for (key, spec) in &self.secrets {
            out.merge(ValueKind::Secret, key, spec.into());
        }
        for instance in &self.sequences {
            for (key, spec) in &instance.config {
                out.merge(ValueKind::Config, key, spec.into());
            }
            for (key, spec) in &instance.secrets {
                out.merge(ValueKind::Secret, key, spec.into());
            }
        }
        out
    }
}

/// The station a run executes on. Every field is `None` where the host does
/// not know it (a source run has no station id).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationContext {
    pub id: Option<String>,
    pub name: Option<String>,
}

/// The procedure being run, as the server knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureContext {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
}

/// The deployment the run's code came from. `environment` is `production`,
/// `preview` or `development` (source and Studio runs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentContext {
    pub id: Option<String>,
    pub environment: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
}

/// What the host knows about a run before it starts: `{run.*}` markers and
/// the `run` object of a phase read it. The slot is added per job by the
/// engine, which knows the slots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunContext {
    pub station: StationContext,
    pub procedure: ProcedureContext,
    pub deployment: DeploymentContext,
}

/// The slot a job runs for. `name` is the key when the slot has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotContext {
    pub key: String,
    pub name: String,
}

impl RunContext {
    /// The context as the `run` object reads it, with the slot of one job
    /// (`null` for a shared job).
    pub fn to_json(&self, slot: Option<&SlotContext>) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "slot".to_string(),
                slot.map(|s| serde_json::json!({"key": s.key, "name": s.name}))
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        value
    }
}

/// Where a value came from. Recorded with the run; the engine adds
/// `Default` for a declaration's `default:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueSource {
    /// The declaration's `default:`.
    Default,
    /// The shell environment of a source run.
    Shell,
    /// The procedure's `.env` file.
    Dotenv,
    /// The station process's environment (deployed runs before server
    /// delivery).
    Process,
    /// A value set on the station.
    Station,
    /// A value set on the procedure (for the deployment's environment).
    Procedure,
    /// A value set for the organization.
    Organization,
}

/// One value the host found, with its source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedValue {
    pub value: serde_json::Value,
    pub source: ValueSource,
}

impl ResolvedValue {
    pub fn new(value: impl Into<serde_json::Value>, source: ValueSource) -> Self {
        Self {
            value: value.into(),
            source,
        }
    }
}

/// What the host passes to [`resolve_values`]: the values it found, by
/// key. Keys nobody declared are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResolvedValues {
    #[serde(default)]
    pub config: BTreeMap<String, ResolvedValue>,
    #[serde(default)]
    pub secrets: BTreeMap<String, ResolvedValue>,
}

/// Required values without one: the run cannot start. Returned before the
/// unit is identified, so a station can show "not configured" and report
/// the keys rather than fail a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigMissing {
    pub keys: Vec<ValueKey>,
}

impl std::fmt::Display for ConfigMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<String> = self.keys.iter().map(ToString::to_string).collect();
        write!(
            f,
            "Not configured: {} required by procedure.yaml {} no value; set {} for this station or procedure",
            keys.join(", "),
            if keys.len() == 1 { "has" } else { "have" },
            if keys.len() == 1 { "it" } else { "them" }
        )
    }
}

impl std::error::Error for ConfigMissing {}

/// A value the host found that does not read as its declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueTypeMismatch {
    pub key: ValueKey,
    pub expected: ValueType,
    pub source: ValueSource,
}

/// Why [`resolve_values`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValuesError {
    /// Required values without one.
    ConfigMissing(ConfigMissing),
    /// Values that do not read as their declared type.
    TypeMismatch(Vec<ValueTypeMismatch>),
}

impl std::fmt::Display for ValuesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValuesError::ConfigMissing(missing) => missing.fmt(f),
            ValuesError::TypeMismatch(errors) => {
                let parts: Vec<String> = errors
                    .iter()
                    .map(|e| {
                        format!(
                            "{} is declared {} but its value (from {}) is not one",
                            e.key,
                            e.expected.as_str(),
                            serde_json::to_value(e.source)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                f.write_str(&parts.join("; "))
            }
        }
    }
}

impl std::error::Error for ValuesError {}

/// The values a run executes with: the run context, and every declared
/// config value and secret that has a value, typed, defaults applied. A declared
/// key without a value (not required, no default) is absent and reads as
/// `null`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunValues {
    pub context: RunContext,
    pub config: BTreeMap<String, ResolvedValue>,
    pub secrets: BTreeMap<String, ResolvedValue>,
}

/// Secret values shorter than this are never masked: masking `on` or `42`
/// would hide every occurrence of a common word or number.
pub const MASK_MIN_LEN: usize = 8;

/// What a masked secret is replaced with.
pub const MASK: &str = "••••";

/// Checks what the host found against the procedure's declarations (see
/// the module doc). Values of undeclared keys are dropped.
pub fn resolve_values(
    definition: &ProcedureDefinition,
    context: RunContext,
    provided: &ResolvedValues,
) -> Result<RunValues, ValuesError> {
    let declared = definition.declared_values();
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    let mut out = RunValues {
        context,
        ..Default::default()
    };
    for (kind, given) in [
        (ValueKind::Config, &provided.config),
        (ValueKind::Secret, &provided.secrets),
    ] {
        for (key, declaration) in declared.of(kind) {
            let value_key = || ValueKey {
                kind,
                key: key.clone(),
            };
            let found = given
                .get(key)
                .filter(|v| !v.value.is_null())
                .cloned()
                .or_else(|| {
                    declaration
                        .default
                        .clone()
                        .map(|d| ResolvedValue::new(d, ValueSource::Default))
                });
            let Some(found) = found else {
                if declaration.required {
                    missing.push(value_key());
                }
                continue;
            };
            match declaration.value_type.coerce(&found.value) {
                Some(value) => {
                    let target = match kind {
                        ValueKind::Config => &mut out.config,
                        ValueKind::Secret => &mut out.secrets,
                    };
                    target.insert(
                        key.clone(),
                        ResolvedValue {
                            value,
                            source: found.source,
                        },
                    );
                }
                None => mismatched.push(ValueTypeMismatch {
                    key: value_key(),
                    expected: declaration.value_type,
                    source: found.source,
                }),
            }
        }
    }
    if !missing.is_empty() {
        return Err(ValuesError::ConfigMissing(ConfigMissing { keys: missing }));
    }
    if !mismatched.is_empty() {
        return Err(ValuesError::TypeMismatch(mismatched));
    }
    Ok(out)
}

impl RunValues {
    /// Every config value by key, values only.
    pub fn config_values(&self) -> BTreeMap<String, serde_json::Value> {
        self.config
            .iter()
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect()
    }

    /// Every secret by key, values only.
    pub fn secret_values(&self) -> BTreeMap<String, serde_json::Value> {
        self.secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect()
    }

    /// The secret values to mask in anything that leaves the station:
    /// each as text, at least `MASK_MIN_LEN` characters, longest first (so
    /// a secret containing another is masked whole).
    pub fn masked_values(&self) -> Vec<String> {
        let mut values: Vec<String> = self
            .secrets
            .values()
            .map(|v| super::markers::as_text(&v.value))
            .filter(|s| s.chars().count() >= MASK_MIN_LEN)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        values.sort_by_key(|s| std::cmp::Reverse(s.len()));
        values
    }

    /// `text` with every exact occurrence of a secret value replaced by
    /// `MASK`.
    pub fn mask<'a>(&self, text: &'a str) -> Cow<'a, str> {
        mask_secrets(text, &self.masked_values())
    }

    /// Expands the run-start markers of `value` for one job's slot.
    pub fn expand(
        &self,
        value: &serde_json::Value,
        slot: Option<&SlotContext>,
    ) -> serde_json::Value {
        self.expand_markers(value, slot, true)
    }

    /// `expand` of a plug's old `config:`, where `{slot}` and
    /// `{slot_name}` stay text.
    pub fn expand_config(
        &self,
        value: &serde_json::Value,
        slot: Option<&SlotContext>,
    ) -> serde_json::Value {
        self.expand_markers(value, slot, false)
    }

    fn expand_markers(
        &self,
        value: &serde_json::Value,
        slot: Option<&SlotContext>,
        slot_aliases: bool,
    ) -> serde_json::Value {
        let run = self.context.to_json(slot);
        let config = self.config_values();
        let secrets = self.secret_values();
        MarkerValues {
            run: &run,
            config: &config,
            secrets: &secrets,
            slot_aliases,
        }
        .expand(value)
    }

    /// `expand` of an `env:` map: every value as text.
    pub fn expand_env(
        &self,
        env: &BTreeMap<String, String>,
        slot: Option<&SlotContext>,
    ) -> Vec<(String, String)> {
        env.iter()
            .map(|(k, v)| {
                let expanded = self.expand(&serde_json::Value::String(v.clone()), slot);
                (k.clone(), super::markers::as_text(&expanded))
            })
            .collect()
    }

    /// One `default_value` with its markers expanded, as text.
    pub fn expand_text(&self, text: &str, slot: Option<&SlotContext>) -> String {
        super::markers::as_text(&self.expand(&serde_json::Value::String(text.to_string()), slot))
    }

    /// The unit block with every `default_value`'s run-start markers
    /// expanded for one slot. What the host passes to `identify`.
    pub fn expand_unit_config(
        &self,
        config: &super::schema::UnitConfig,
        slot: Option<&SlotContext>,
    ) -> super::schema::UnitConfig {
        let mut out = config.clone();
        out.for_each_field_mut(|field| self.expand_field(field, slot));
        out
    }

    /// One field's `default_value` expanded (the root `operated_by:`).
    pub fn expand_field(
        &self,
        field: &mut super::schema::UnitFieldConfig,
        slot: Option<&SlotContext>,
    ) {
        if let Some(v) = field.default_value.as_mut() {
            *v = self.expand_text(v, slot);
        }
    }
}

/// `text` with every exact occurrence of one of `values` replaced by
/// `MASK`. `values` as `RunValues::masked_values` returns them.
pub fn mask_secrets<'a>(text: &'a str, values: &[String]) -> Cow<'a, str> {
    let mut out = Cow::Borrowed(text);
    for value in values {
        if value.is_empty() || !out.contains(value.as_str()) {
            continue;
        }
        out = Cow::Owned(out.replace(value.as_str(), MASK));
    }
    out
}

/// `text`, a Python `repr`, with every one of `values` masked as it
/// appears raw and as `repr` escapes it: a PEM key's newlines read `\n`
/// there, a backslash `\\`, a quote `\'`, and an exact match of the raw
/// value finds none of them.
pub fn mask_python_repr<'a>(text: &'a str, values: &[String]) -> Cow<'a, str> {
    let mut forms: Vec<String> = Vec::with_capacity(values.len() * 3);
    for value in values {
        forms.push(python_escape(value, false));
        forms.push(python_escape(value, true));
        forms.push(value.clone());
    }
    // Longest first, so a secret that contains another is masked whole.
    forms.sort_by_key(|s| std::cmp::Reverse(s.len()));
    forms.dedup();
    mask_secrets(text, &forms)
}

/// `value` as it appears inside a Python `repr` string literal.
fn python_escape(value: &str, escape_quote: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\'' if escape_quote => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out
}

/// The JSON of `value` masked through lists and mappings: every string,
/// every object key, and every number whose text is a secret (an integer
/// PIN recorded as a measurement), which becomes the string `MASK`.
pub fn mask_json(value: &mut serde_json::Value, values: &[String]) {
    mask_json_with(value, values, true);
}

/// `mask_json` of strings only, for a tree that must still deserialize
/// into a typed value (a number field cannot hold `MASK`, a struct field
/// cannot be renamed).
pub fn mask_json_strings(value: &mut serde_json::Value, values: &[String]) {
    mask_json_with(value, values, false);
}

fn mask_json_with(value: &mut serde_json::Value, values: &[String], keys_and_numbers: bool) {
    match value {
        serde_json::Value::String(s) => {
            if let Cow::Owned(masked) = mask_secrets(s, values) {
                *s = masked;
            }
        }
        serde_json::Value::Number(n) if keys_and_numbers => {
            let text = n.to_string();
            if values.contains(&text) {
                *value = serde_json::Value::String(MASK.to_string());
            }
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .for_each(|v| mask_json_with(v, values, keys_and_numbers)),
        serde_json::Value::Object(map) => {
            if keys_and_numbers
                && map
                    .keys()
                    .any(|k| matches!(mask_secrets(k, values), Cow::Owned(_)))
            {
                *map = std::mem::take(map)
                    .into_iter()
                    .map(|(k, v)| (mask_secrets(&k, values).into_owned(), v))
                    .collect();
            }
            map.values_mut()
                .for_each(|v| mask_json_with(v, values, keys_and_numbers));
        }
        _ => {}
    }
}

/// Whether `key` may name a config value or a secret: upper case, digits and `_`,
/// not starting with a digit.
pub fn is_valid_value_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `key` may name an `env:` variable: letters, digits and `_`, not
/// starting with a digit.
pub fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The prefix of the names TofuPilot sets itself (credentials, the CLI's
/// own variables): never a config value, a secret or an `env:` key.
pub const RESERVED_PREFIX: &str = "TOFUPILOT_";

/// What a procedure or sequence file's top-level `vars:` was renamed to,
/// in place of serde's list of every field. A refusal, not an alias.
pub(crate) fn renamed_top_level_block(e: &super::yaml::Error) -> Option<&'static str> {
    (matches!(e.path(), None | Some("vars")) && e.message().starts_with("unknown field `vars`"))
        .then_some("`vars:` was renamed `config:` (markers `{config.X}`, phase parameter `config`)")
}

/// Every `{run.*}` field, for messages.
pub fn run_fields() -> impl Iterator<Item = &'static str> {
    RUN_FIELDS.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_secret_escaped_by_repr_is_masked() {
        let secret = "-----BEGIN KEY-----\nabc\\def'ghi\n-----END KEY-----".to_string();
        let repr = r"'-----BEGIN KEY-----\nabc\\def\'ghi\n-----END KEY-----'";
        let masked = mask_python_repr(repr, &[secret]);
        assert!(!masked.contains("abc"), "{masked}");
    }

    #[test]
    fn strings_read_as_the_declared_type() {
        assert_eq!(ValueType::Integer.coerce(&json!("30")), Some(json!(30)));
        assert_eq!(ValueType::Integer.coerce(&json!(30.0)), Some(json!(30)));
        assert_eq!(ValueType::Integer.coerce(&json!("3.5")), None);
        assert_eq!(ValueType::Number.coerce(&json!("1.5")), Some(json!(1.5)));
        assert_eq!(ValueType::Number.coerce(&json!("2")), Some(json!(2)));
        assert_eq!(ValueType::Number.coerce(&json!("nan")), None);
        assert_eq!(ValueType::Boolean.coerce(&json!("TRUE")), Some(json!(true)));
        assert_eq!(ValueType::Boolean.coerce(&json!("0")), Some(json!(false)));
        assert_eq!(ValueType::Boolean.coerce(&json!("maybe")), None);
        assert_eq!(ValueType::String.coerce(&json!(5)), Some(json!("5")));
        assert_eq!(ValueType::String.coerce(&json!([1])), None);
    }

    #[test]
    fn masking_replaces_exact_values_of_eight_characters_or_more() {
        let values = RunValues {
            secrets: BTreeMap::from([
                (
                    "LONG".to_string(),
                    ResolvedValue::new("s3cr3t-token-123", ValueSource::Dotenv),
                ),
                (
                    "SHORT".to_string(),
                    ResolvedValue::new("abc", ValueSource::Dotenv),
                ),
                (
                    "INNER".to_string(),
                    ResolvedValue::new("s3cr3t-token", ValueSource::Dotenv),
                ),
            ]),
            ..Default::default()
        };
        assert_eq!(values.masked_values(), ["s3cr3t-token-123", "s3cr3t-token"]);
        assert_eq!(values.mask("auth s3cr3t-token-123 abc"), "auth •••• abc");
        assert_eq!(values.mask("auth s3cr3t-token."), "auth ••••.");
        assert!(matches!(values.mask("nothing here"), Cow::Borrowed(_)));
        let mut json = json!({"a": ["x s3cr3t-token-123"], "b": 1});
        mask_json(&mut json, &values.masked_values());
        assert_eq!(json, json!({"a": ["x ••••"], "b": 1}));
    }

    /// A number whose text is a secret, and an object key holding one,
    /// are masked too (adversarial review S5).
    #[test]
    fn masking_covers_numbers_and_object_keys() {
        let masks = vec!["12345678".to_string(), "s3cr3t-key-name".to_string()];
        let mut json = json!({
            "pin": 12345678,
            "near": 123456789,
            "float": 12345678.5,
            "s3cr3t-key-name": {"x": [12345678, 1]},
            "prefix s3cr3t-key-name": 2,
        });
        mask_json(&mut json, &masks);
        assert_eq!(
            json,
            json!({
                "pin": MASK,
                "near": 123456789,
                "float": 12345678.5,
                "••••": {"x": [MASK, 1]},
                "prefix ••••": 2,
            })
        );
        // Strings only: numbers and keys stay for a typed value.
        let mut typed = json!({"s3cr3t-key-name": 12345678, "s": "pin 12345678"});
        mask_json_strings(&mut typed, &masks);
        assert_eq!(typed, json!({"s3cr3t-key-name": 12345678, "s": "pin ••••"}));
    }

    const PROCEDURE: &str = "name: Board
execution:
  slots:
    - key: s1
      name: Nest 1
config:
  MES_URL: {required: true}
  TIMEOUT: {type: integer, default: 30}
  DEBUG: {type: boolean}
secrets:
  TOKEN: {required: true}
unit:
  serial_number:
    default_value: \"SN-{run.slot.key}-{config.TIMEOUT}\"
plugs:
  - name: Mes
    python: plugs.mes:Mes
    with:
      url: \"{config.MES_URL}\"
      timeout: \"{config.TIMEOUT}\"
      auth: \"Bearer {secrets.TOKEN}\"
      nest: \"{run.slot.name}\"
    env:
      DEBUG: \"{config.DEBUG}\"
main:
  - name: M
    python: phases.m:m
";

    fn definition() -> ProcedureDefinition {
        crate::procedure::loader::load_procedure_definition_from_str(PROCEDURE).unwrap()
    }

    fn provided() -> ResolvedValues {
        ResolvedValues {
            config: BTreeMap::from([
                (
                    "MES_URL".to_string(),
                    ResolvedValue::new("http://mes", ValueSource::Dotenv),
                ),
                (
                    "UNDECLARED".to_string(),
                    ResolvedValue::new("x", ValueSource::Shell),
                ),
            ]),
            secrets: BTreeMap::from([(
                "TOKEN".to_string(),
                ResolvedValue::new("s3cr3t-token", ValueSource::Process),
            )]),
        }
    }

    #[test]
    fn missing_required_values_are_a_config_missing_error() {
        let err = resolve_values(
            &definition(),
            RunContext::default(),
            &ResolvedValues::default(),
        )
        .unwrap_err();
        let ValuesError::ConfigMissing(missing) = &err else {
            panic!("{err:?}")
        };
        assert_eq!(
            missing.keys,
            [
                ValueKey {
                    kind: ValueKind::Config,
                    key: "MES_URL".into()
                },
                ValueKey {
                    kind: ValueKind::Secret,
                    key: "TOKEN".into()
                },
            ]
        );
        assert!(
            err.to_string().contains("config.MES_URL, secrets.TOKEN"),
            "{err}"
        );
        // A null value is no value.
        let mut nulls = provided();
        nulls.config.get_mut("MES_URL").unwrap().value = serde_json::Value::Null;
        assert!(matches!(
            resolve_values(&definition(), RunContext::default(), &nulls),
            Err(ValuesError::ConfigMissing(_))
        ));
    }

    #[test]
    fn values_are_typed_defaulted_and_undeclared_ones_dropped() {
        let mut given = provided();
        given.config.insert(
            "DEBUG".into(),
            ResolvedValue::new("true", ValueSource::Shell),
        );
        let values = resolve_values(&definition(), RunContext::default(), &given).unwrap();
        assert_eq!(
            values.config["TIMEOUT"],
            ResolvedValue::new(30, ValueSource::Default)
        );
        assert_eq!(
            values.config["DEBUG"],
            ResolvedValue::new(true, ValueSource::Shell)
        );
        assert!(!values.config.contains_key("UNDECLARED"));
        assert_eq!(values.secrets["TOKEN"].source, ValueSource::Process);
    }

    #[test]
    fn a_value_not_of_its_type_is_refused() {
        let mut given = provided();
        given.config.insert(
            "TIMEOUT".into(),
            ResolvedValue::new("soon", ValueSource::Dotenv),
        );
        let err = resolve_values(&definition(), RunContext::default(), &given).unwrap_err();
        assert_eq!(
            err,
            ValuesError::TypeMismatch(vec![ValueTypeMismatch {
                key: ValueKey {
                    kind: ValueKind::Config,
                    key: "TIMEOUT".into()
                },
                expected: ValueType::Integer,
                source: ValueSource::Dotenv,
            }])
        );
        assert_eq!(
            err.to_string(),
            "config.TIMEOUT is declared integer but its value (from dotenv) is not one"
        );
    }

    #[test]
    fn plug_with_and_env_expand_for_the_slot() {
        let def = definition();
        let context = RunContext {
            station: StationContext {
                id: None,
                name: Some("Bench".into()),
            },
            ..Default::default()
        };
        let values = resolve_values(&def, context, &provided()).unwrap();
        let slot = SlotContext {
            key: "s1".into(),
            name: "Nest 1".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plugs")).unwrap();
        std::fs::write(dir.path().join("plugs/mes.py"), "class Mes:\n    pass\n").unwrap();
        let config = def.plugs[0]
            .to_resolved_config_json(dir.path(), &values, Some(&slot))
            .unwrap();
        assert_eq!(
            config["config"],
            serde_json::json!({
                "url": "http://mes",
                "timeout": 30,
                "auth": "Bearer s3cr3t-token",
                "nest": "Nest 1",
            })
        );
        // DEBUG has no value: empty in the environment.
        assert_eq!(config["env"], serde_json::json!({"DEBUG": ""}));
        let unit = values.expand_unit_config(def.unit.as_ref().unwrap(), Some(&slot));
        assert_eq!(
            unit.serial_number.unwrap().default_value.as_deref(),
            Some("SN-s1-30")
        );
        assert_eq!(values.masked_values(), ["s3cr3t-token"]);
    }

    #[test]
    fn value_keys() {
        assert!(is_valid_value_key("MES_URL"));
        assert!(is_valid_value_key("_X1"));
        assert!(!is_valid_value_key("mes_url"));
        assert!(!is_valid_value_key("1X"));
        assert!(!is_valid_value_key(""));
        assert!(is_valid_env_key("Path_1"));
        assert!(!is_valid_env_key("A-B"));
    }
}
