//! Sequence files and the tree of calls a procedure makes to them.
//!
//! A sequence file is a group of phases written once and called from a
//! procedure (or from another sequence) with `uses:`. It has the shape of
//! a procedure minus what only a procedure owns: no `version`, no
//! `execution`, no `unit`, no `operated_by`, no `scope` on its phases. In
//! exchange it declares `inputs:` the call fills with `with:`, and the
//! plugs it needs, bound to the procedure's plugs with `plugs:`.
//!
//! The YAML structs here are what a sequence FILE deserializes to. The
//! call entry itself (`SequenceCallYaml`) lives next to the phase entry
//! in `schema.rs`, since both are shapes of one list entry. The runtime
//! view is `SequenceInstance`: one resolved call, kept on the expanded
//! `ProcedureDefinition` so the orchestrator, the upload and Studio see
//! the tree the flat phase lists came from.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::schema::{PhaseEntryYaml, PhaseStage, PythonSpec, Scope, SequenceCallYaml, ThenConfig};

/// A sequence file: phases written once, called from procedures with
/// `uses:`. Same stages as a procedure (setup, main, teardown), run with
/// the same barriers inside the call. Not allowed here, because the
/// procedure owns them: version, execution, unit, operated_by, and scope
/// on a phase (the call's scope applies to every phase).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct SequenceYaml {
    /// Display name of the sequence, 1-100 characters. The call's name when
    /// the call states none.
    #[serde(deserialize_with = "super::schema::trimmed_text")]
    pub name: String,

    /// Free text shown with the sequence.
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "super::schema::trimmed_text"
    )]
    pub description: String,

    /// Parameters the call fills with `with:`, by name (a Python identifier).
    /// Phases read them on the `inputs` injectable (`inputs.target_voltage`);
    /// YAML fields of this file read them as `{inputs.<name>}`. An input
    /// without a default is required.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputSpecYaml>,

    /// The procedure config this sequence reads, declared like the procedure's
    /// config: (same keys, same types). Values come from the procedure's run:
    /// a sequence never sets one. Its phases read only these, as
    /// `{config.KEY}` and `config.KEY`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, super::values::ConfigSpecYaml>,

    /// The procedure secrets this sequence reads, declared like config:.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, super::values::SecretSpecYaml>,

    /// The plugs this sequence's phases need: requirements, not instances.
    /// Only the procedure creates plugs; the call binds each requirement to
    /// one of them with `plugs:`, or by key when the names match. No config:
    /// here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugs: Vec<PlugRequirementYaml>,

    /// Phases run before this sequence's main; the sequence's main waits
    /// for all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<PhaseEntryYaml>,

    /// The sequence's phases. A sequence call needs at least one phase
    /// somewhere in its lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main: Vec<PhaseEntryYaml>,

    /// Phases run after this sequence's main, whatever its outcome, also
    /// when the sequence was cut short.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<PhaseEntryYaml>,
}

/// The type of an input value. A `{inputs.<name>}` marker standing alone
/// in a YAML field takes this type; the call's `with:` value must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    String,
    Number,
    Integer,
    Boolean,
}

impl InputType {
    pub fn as_str(self) -> &'static str {
        match self {
            InputType::String => "string",
            InputType::Number => "number",
            InputType::Integer => "integer",
            InputType::Boolean => "boolean",
        }
    }

    /// Whether `value` is of this type. A number is accepted for `integer`
    /// only when it has no fractional part.
    pub fn accepts(self, value: &serde_json::Value) -> bool {
        match self {
            InputType::String => value.is_string(),
            InputType::Number => value.is_number(),
            InputType::Integer => {
                value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            InputType::Boolean => value.is_boolean(),
        }
    }
}

/// One input of a sequence file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct InputSpecYaml {
    /// string, number, integer or boolean.
    #[serde(rename = "type")]
    pub input_type: InputType,
    /// Value used when the call gives none. Without it the call must set the
    /// input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Free text, shown in Studio next to the input.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
}

/// A plug the sequence's phases need, by the parameter name they use for
/// it. The procedure's call binds it to one of the procedure's plugs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub struct PlugRequirementYaml {
    /// Display name of the requirement.
    #[serde(deserialize_with = "super::schema::trimmed_text")]
    pub name: String,
    /// The parameter name the sequence's phases use to receive this plug;
    /// lowercased and underscored from name when absent. The call binds it
    /// with `plugs: {<key>: <procedure plug key>}`, or by the same key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The plug class the sequence was written against, as
    /// `module:Class`. Optional: when given, a bound procedure plug of
    /// another class is reported as a warning, not refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSpec>,
    /// Free text.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_trim::option_string_trim"
    )]
    pub description: Option<String>,
}

impl PlugRequirementYaml {
    pub fn key(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| crate::python::to_python_identifier(&self.name))
    }
}

/// Where an expanded phase was written: the sequence file and the entry's
/// place in it, so a diagnostic on the phase points into that file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSource {
    /// The sequence file, as resolved on disk.
    pub file: String,
    /// The list of that file the entry sits in.
    pub list: PhaseStage,
    /// The entry's index in that list.
    pub index: usize,
}

/// One resolved sequence call of the expanded procedure. The flat phase
/// lists carry its phases (their `sequence_path` starts with `path`);
/// this is the node of the tree over them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceInstance {
    /// Instance keys from the procedure root to this call, this call's key
    /// last. `path.join("/")` is the sequence's `sequence_path` on the
    /// dashboard; `path.len()` is its depth, 1 for a call made in the
    /// procedure itself.
    pub path: Vec<String>,
    /// Display name: the call's `name:`, else the file's.
    pub name: String,
    /// The reference as written on the call (`./sequences/x.yaml`,
    /// `pkg:sequences/x.yaml`).
    pub source: String,
    /// The sequence file as resolved on disk.
    pub file: String,
    /// The call's own `description:`, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The sequence file's description, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_description: Option<String>,
    /// The list of its parent (the procedure, or the calling sequence) the
    /// call sits in.
    pub stage: PhaseStage,
    /// The entry's index in that list, so a diagnostic about the call
    /// points at its line (meaningful to the editor for a call written in
    /// the procedure, depth 1).
    #[serde(default)]
    pub entry_index: usize,
    /// The call's `scope:`, set only on a call made from the procedure's
    /// setup or teardown. Every phase of the call carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// The call's `then:`: what the parent does with the sequence's outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<ThenConfig>,
    /// The entry's own `enabled:`. The phases it brings in are disabled
    /// when it is, so this is read from the entry, not derived from them:
    /// a file whose every phase is disabled is still an enabled call.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The call's `depends_on`, as full phase keys after resolution (a
    /// dependency on another call names that call's terminal phases).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The inputs the sequence's phases received, defaults applied.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, serde_json::Value>,
    /// The `config:` the sequence file declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, super::values::ConfigSpecYaml>,
    /// The `secrets:` the sequence file declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, super::values::SecretSpecYaml>,
    /// Plug requirement key to the procedure plug key it is bound to.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugs: BTreeMap<String, String>,
    /// The `with:` as the call wrote it, kept for writing the call back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub with: BTreeMap<String, serde_json::Value>,
    /// The call's `plugs:` as written, for writing the call back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plug_bindings: BTreeMap<String, String>,
    /// The call's `depends_on` as written, for writing the call back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on_written: Vec<String>,
    /// The call's `name:` as written, markers in place, when stated (else
    /// the file's name is used). For writing the call back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_written: Option<String>,
    /// False when the entry is nested in a sequence file and gives an
    /// `{inputs.x}` marker to a typed field (`enabled`, `then`), so only
    /// the substituted entry types: `name_written`, `with`, `plug_bindings`,
    /// `depends_on_written` and `description` then hold one caller's values
    /// and must not be written back to the shared file.
    #[serde(default = "default_true")]
    pub written_form: bool,
    /// Whether the call's `key:` was stated (else derived).
    #[serde(default)]
    pub key_stated: bool,
    /// Full keys of the phases this call's lists hold directly or through
    /// nested calls, by the stage within THIS sequence they belong to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub main_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown_keys: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// The `inputs:` declarations of a sequence file's text, and nothing else
/// of it: the one read the expansion and Studio share, through the
/// procedure's reader. An error when the text does not parse or the
/// declarations do not type.
pub fn read_declared_inputs(
    text: &str,
) -> Result<BTreeMap<String, InputSpecYaml>, super::yaml::Error> {
    #[derive(Deserialize)]
    struct JustInputs {
        #[serde(default)]
        inputs: BTreeMap<String, InputSpecYaml>,
    }
    super::yaml::from_str::<JustInputs>(text).map(|just| just.inputs)
}

impl SequenceInstance {
    /// The call's own key: the last element of `path`.
    pub fn key(&self) -> &str {
        self.path.last().map(String::as_str).unwrap_or("")
    }

    /// `path` joined by `/`, the dashboard's `sequence_path`.
    pub fn path_str(&self) -> String {
        self.path.join("/")
    }

    /// The parent call's path, empty for a call made in the procedure.
    pub fn parent_path(&self) -> &[String] {
        &self.path[..self.path.len().saturating_sub(1)]
    }

    /// Every phase key of this sequence, nested calls included.
    pub fn all_keys(&self) -> impl Iterator<Item = &String> {
        self.setup_keys
            .iter()
            .chain(self.main_keys.iter())
            .chain(self.teardown_keys.iter())
    }

    /// The phases that finish last in this sequence: its teardown when it
    /// has one, else its main, else its setup. A dependency on the call
    /// waits for these, which (through the sequence's own barriers) means
    /// for the whole sequence.
    pub fn terminal_keys(&self) -> &[String] {
        if !self.teardown_keys.is_empty() {
            &self.teardown_keys
        } else if !self.main_keys.is_empty() {
            &self.main_keys
        } else {
            &self.setup_keys
        }
    }

    /// The `inputs:` the sequence file declares, read from the file now
    /// (the expansion keeps the resolved values, not the declarations).
    /// Empty when the file cannot be read or parsed: callers show the
    /// resolved values alone.
    pub fn declared_inputs(&self) -> BTreeMap<String, InputSpecYaml> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|text| read_declared_inputs(&text).ok())
            .unwrap_or_default()
    }

    /// The plug requirements the sequence file declares, same tolerance.
    pub fn declared_plugs(&self) -> Vec<PlugRequirementYaml> {
        #[derive(Deserialize)]
        struct JustPlugs {
            #[serde(default)]
            plugs: Vec<PlugRequirementYaml>,
        }
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|text| super::yaml::from_str::<JustPlugs>(&text).ok())
            .map(|j| j.plugs)
            .unwrap_or_default()
    }

    /// True when `phase_path` is this call or sits under it.
    pub fn contains_path(&self, phase_path: &[String]) -> bool {
        crate::job::path_has_prefix(phase_path, &self.path)
    }

    /// The call entry as it would be written back, from what the call
    /// stated (not the resolved values).
    pub fn to_yaml(&self) -> SequenceCallYaml {
        let auto_key = crate::python::to_python_identifier(&self.name);
        SequenceCallYaml {
            name: self.name_written.clone(),
            key: (self.key_stated || self.key() != auto_key).then(|| self.key().to_string()),
            uses: self.source.clone(),
            with: self.with.clone(),
            plugs: self.plug_bindings.clone(),
            description: self.description.clone(),
            enabled: self.enabled,
            depends_on: self.depends_on_written.clone(),
            scope: self.scope,
            then: self.then.clone(),
        }
    }
}
