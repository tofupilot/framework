//! Markers in procedure YAML values, by namespace and by the stage that
//! resolves them.
//!
//! - `{inputs.x}`: a sequence file's inputs, substituted in the file's text
//!   when the procedure loads (`expand::marker_rewrite`).
//! - `{run.*}`, `{config.X}`, `{secrets.X}`: known only when a run starts
//!   (the station, the slot, the values the host resolved). They are
//!   allowed only where the value is kept as written until then: `with:`
//!   of a phase or a plug, `env:` (procedure, phase, plug) and a unit
//!   field's `default_value`. Anywhere else they stay as written, with a
//!   warning (a sequence call's `with:` refuses them: the call's inputs
//!   are substituted into the sequence file, where they would resolve).
//! - `{slot}` / `{slot_name}`: the old spellings of `{run.slot.key}` /
//!   `{run.slot.name}`, kept as aliases with a deprecation warning. Under
//!   a plug's old `config:` they stay plain text, as they always were.
//!
//! A marker of any other namespace (`{record.levelname}`) is text: plug
//! arguments and environment values carry format strings of their own.
//!
//! A value that is exactly one marker keeps the marker's type; a marker
//! inside a longer string becomes text.

use std::collections::BTreeMap;

use super::schema::PhaseStage;

/// Every namespace a marker may name.
pub const NAMESPACES: &[&str] = &["inputs", "run", "config", "secrets"];

/// The namespaces resolved when the run starts.
pub const RUN_START_NAMESPACES: &[&str] = &["run", "config", "secrets"];

/// Every field of `{run.*}`, as the marker spells it after `run.`. The same
/// paths are read on the `run` object of a phase.
pub const RUN_FIELDS: &[&str] = &[
    "slot.key",
    "slot.name",
    "station.id",
    "station.name",
    "procedure.id",
    "procedure.name",
    "procedure.version",
    "deployment.id",
    "deployment.environment",
    "deployment.commit",
    "deployment.branch",
];

/// What one marker names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerRef {
    /// `{<namespace>.<field>}`, `field` holding every segment after the
    /// namespace (`slot.key` for `{run.slot.key}`).
    Namespaced { namespace: String, field: String },
    /// `{slot}`, the old spelling of `{run.slot.key}`.
    SlotAlias,
    /// `{slot_name}`, the old spelling of `{run.slot.name}`.
    SlotNameAlias,
}

impl MarkerRef {
    /// The marker as written.
    pub fn written(&self) -> String {
        match self {
            MarkerRef::Namespaced { namespace, field } => format!("{{{namespace}.{field}}}"),
            MarkerRef::SlotAlias => "{slot}".to_string(),
            MarkerRef::SlotNameAlias => "{slot_name}".to_string(),
        }
    }

    pub fn namespace(&self) -> &str {
        match self {
            MarkerRef::Namespaced { namespace, .. } => namespace,
            MarkerRef::SlotAlias | MarkerRef::SlotNameAlias => "run",
        }
    }

    /// Resolved when the run starts (a `run`, `config` or `secrets` marker,
    /// or a slot alias).
    pub fn is_run_start(&self) -> bool {
        RUN_START_NAMESPACES.contains(&self.namespace())
    }

    /// Names one of `NAMESPACES` (or is a slot alias). Any other marker is
    /// text.
    pub fn is_known(&self) -> bool {
        NAMESPACES.contains(&self.namespace())
    }

    /// `{slot}` or `{slot_name}`.
    pub fn is_alias(&self) -> bool {
        matches!(self, MarkerRef::SlotAlias | MarkerRef::SlotNameAlias)
    }

    /// Reads the slot: `{run.slot.*}` or one of its aliases.
    pub fn reads_slot(&self) -> bool {
        match self {
            MarkerRef::Namespaced { namespace, field } => {
                namespace == "run" && (field == "slot" || field.starts_with("slot."))
            }
            MarkerRef::SlotAlias | MarkerRef::SlotNameAlias => true,
        }
    }

    /// The new spelling of an alias.
    pub fn replacement(&self) -> Option<&'static str> {
        match self {
            MarkerRef::SlotAlias => Some("{run.slot.key}"),
            MarkerRef::SlotNameAlias => Some("{run.slot.name}"),
            MarkerRef::Namespaced { .. } => None,
        }
    }
}

/// One marker found in a string: its byte range and what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundMarker {
    pub start: usize,
    pub end: usize,
    pub reference: MarkerRef,
}

/// Every marker of `s`, in order. A marker is `{slot}`, `{slot_name}`, or
/// `{<identifier>.<segment>[.<segment>...]}`; anything else in braces
/// (`{3}`, `{value:.2f}`, `{channel}`) is text.
pub fn find_markers(s: &str) -> Vec<FoundMarker> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let Some(close) = s[i + 1..].find('}') else {
            break;
        };
        let inner = &s[i + 1..i + 1 + close];
        let end = i + 1 + close + 1;
        if let Some(reference) = parse_marker(inner) {
            out.push(FoundMarker {
                start: i,
                end,
                reference,
            });
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn parse_marker(inner: &str) -> Option<MarkerRef> {
    match inner {
        "slot" => return Some(MarkerRef::SlotAlias),
        "slot_name" => return Some(MarkerRef::SlotNameAlias),
        _ => {}
    }
    let (namespace, field) = inner.split_once('.')?;
    if !crate::python::is_valid_python_identifier(namespace) {
        return None;
    }
    let segment_ok =
        |seg: &str| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !field.split('.').all(segment_ok) {
        return None;
    }
    Some(MarkerRef::Namespaced {
        namespace: namespace.to_string(),
        field: field.to_string(),
    })
}

/// `Some(reference)` when `s` is exactly one marker.
pub fn whole_marker(s: &str) -> Option<MarkerRef> {
    let found = find_markers(s);
    match found.as_slice() {
        [only] if only.start == 0 && only.end == s.len() => Some(only.reference.clone()),
        _ => None,
    }
}

/// Where a value sits, for the placement rule: the places that keep their
/// value as written until the run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// `env:` of the procedure.
    ProcedureEnv,
    /// `plugs[i].with`.
    PlugWith(usize),
    /// `plugs[i].config`, the old spelling of `with`: `{slot}` and
    /// `{slot_name}` are plain text there, as they were before `with:`.
    PlugConfig(usize),
    /// `plugs[i].env`.
    PlugEnv(usize),
    /// `with:` of a phase entry.
    PhaseWith(PhaseStage, usize),
    /// `env:` of a phase entry.
    PhaseEnv(PhaseStage, usize),
    /// `with:` of a sequence call: the call's inputs, resolved when the
    /// procedure loads. Not a run-start placement.
    CallWith(PhaseStage, usize),
    /// A unit field's (or `operated_by`'s) `default_value`.
    IdentifyDefault,
}

impl Placement {
    /// A run-start marker may sit here.
    pub fn takes_run_markers(self) -> bool {
        !matches!(self, Placement::CallWith(..))
    }

    /// `{slot}` / `{slot_name}` resolve here (a run-start place other
    /// than the old `config:`).
    pub fn takes_slot_aliases(self) -> bool {
        self.takes_run_markers() && !matches!(self, Placement::PlugConfig(_))
    }
}

/// The placement of the value scalar at `path` (`main[2].with.channel`,
/// `plugs[0].config.address`, `unit.serial_number.default_value`), `None`
/// for every other field. `is_call` tells a sequence call entry from a
/// phase entry.
pub fn placement(path: &str, is_call: impl Fn(PhaseStage, usize) -> bool) -> Option<Placement> {
    if let Some(rest) = path.strip_prefix("env.") {
        return (!rest.is_empty()).then_some(Placement::ProcedureEnv);
    }
    if (path.starts_with("unit.") || path.starts_with("operated_by."))
        && path.ends_with(".default_value")
    {
        return Some(Placement::IdentifyDefault);
    }
    let (list, rest) = path.split_once('[')?;
    let (index, rest) = rest.split_once(']')?;
    let index: usize = index.parse().ok()?;
    let rest = rest.strip_prefix('.')?;
    let field = rest.split(['.', '[']).next().unwrap_or(rest);
    let stage = match list {
        "plugs" => {
            return match field {
                "with" => Some(Placement::PlugWith(index)),
                "config" => Some(Placement::PlugConfig(index)),
                "env" if rest.len() > "env".len() => Some(Placement::PlugEnv(index)),
                _ => None,
            }
        }
        "setup" => PhaseStage::Setup,
        "main" => PhaseStage::Main,
        "teardown" => PhaseStage::Teardown,
        _ => return None,
    };
    match field {
        "with" if is_call(stage, index) => Some(Placement::CallWith(stage, index)),
        "with" => Some(Placement::PhaseWith(stage, index)),
        "env" if rest.len() > "env".len() => Some(Placement::PhaseEnv(stage, index)),
        _ => None,
    }
}

/// What run-start markers resolve to: the run's context (slot included,
/// as JSON shaped like the `run` object of a phase) and the values.
pub struct MarkerValues<'a> {
    pub run: &'a serde_json::Value,
    pub config: &'a BTreeMap<String, serde_json::Value>,
    pub secrets: &'a BTreeMap<String, serde_json::Value>,
    /// `{slot}` / `{slot_name}` resolve; false under a plug's old
    /// `config:`, where they are text.
    pub slot_aliases: bool,
}

impl MarkerValues<'_> {
    /// The value a run-start marker names. `None` for an `{inputs.x}` or
    /// unknown marker, which stays as written (the loader refuses unknown
    /// ones where they would be resolved). A declared value the host did
    /// not provide is `null`.
    pub fn lookup(&self, reference: &MarkerRef) -> Option<serde_json::Value> {
        match reference {
            MarkerRef::SlotAlias | MarkerRef::SlotNameAlias if !self.slot_aliases => None,
            MarkerRef::SlotAlias => Some(self.run_field("slot.key")),
            MarkerRef::SlotNameAlias => Some(self.run_field("slot.name")),
            MarkerRef::Namespaced { namespace, field } => match namespace.as_str() {
                "run" => RUN_FIELDS
                    .contains(&field.as_str())
                    .then(|| self.run_field(field)),
                "config" => Some(self.config.get(field).cloned().unwrap_or_default()),
                "secrets" => Some(self.secrets.get(field).cloned().unwrap_or_default()),
                _ => None,
            },
        }
    }

    fn run_field(&self, field: &str) -> serde_json::Value {
        let mut at = self.run;
        for segment in field.split('.') {
            match at.get(segment) {
                Some(next) => at = next,
                None => return serde_json::Value::Null,
            }
        }
        at.clone()
    }

    /// `value` with every run-start marker in its strings resolved,
    /// through lists and mappings. A string that is exactly one marker
    /// takes the value's type; a marker inside text is interpolated (a
    /// missing value as nothing).
    pub fn expand(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => self.expand_str(s),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|v| self.expand(v)).collect())
            }
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.expand(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// `expand` of one string.
    pub fn expand_str(&self, s: &str) -> serde_json::Value {
        if let Some(reference) = whole_marker(s) {
            if let Some(value) = self.lookup(&reference) {
                return value;
            }
        }
        serde_json::Value::String(self.interpolate(s))
    }

    /// `s` with every run-start marker replaced by its value as text.
    pub fn interpolate(&self, s: &str) -> String {
        let found = find_markers(s);
        if found.is_empty() {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut copied = 0;
        for marker in found {
            let Some(value) = self.lookup(&marker.reference) else {
                continue;
            };
            out.push_str(&s[copied..marker.start]);
            out.push_str(&as_text(&value));
            copied = marker.end;
        }
        out.push_str(&s[copied..]);
        out
    }
}

/// A value as text inside a string: strings raw, `null` as nothing, the
/// rest as JSON.
pub fn as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Every marker in the strings of `value`, through lists and mappings.
pub fn markers_in_value(value: &serde_json::Value) -> Vec<MarkerRef> {
    let mut out = Vec::new();
    collect(value, &mut out);
    out
}

fn collect(value: &serde_json::Value, out: &mut Vec<MarkerRef>) {
    match value {
        serde_json::Value::String(s) => {
            out.extend(find_markers(s).into_iter().map(|m| m.reference))
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect(v, out)),
        _ => {}
    }
}

/// ` (did you mean `x`?)` for the closest candidate within two edits.
pub fn did_you_mean<'a>(wrong: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    super::pysource::closest(wrong, candidates)
        .map(|c| format!("; did you mean `{c}`?"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markers_are_namespaced_identifiers_or_the_two_aliases() {
        let found: Vec<String> = find_markers(
            "{run.slot.key}-{config.MES_URL} {slot} {slot_name} {3} {value:.2f} {channel} {a.b-c}",
        )
        .into_iter()
        .map(|m| m.reference.written())
        .collect();
        assert_eq!(
            found,
            [
                "{run.slot.key}",
                "{config.MES_URL}",
                "{slot}",
                "{slot_name}"
            ]
        );
        assert_eq!(
            whole_marker("{config.X}"),
            Some(MarkerRef::Namespaced {
                namespace: "config".into(),
                field: "X".into()
            })
        );
        assert_eq!(whole_marker(" {config.X}"), None);
        assert_eq!(whole_marker("{config.X}{config.Y}"), None);
    }

    #[test]
    fn placement_by_path() {
        let calls = |stage: PhaseStage, index: usize| stage == PhaseStage::Main && index == 1;
        assert_eq!(placement("env.PORT", calls), Some(Placement::ProcedureEnv));
        assert_eq!(
            placement("plugs[0].with.address", calls),
            Some(Placement::PlugWith(0))
        );
        assert_eq!(
            placement("plugs[2].config.list[1]", calls),
            Some(Placement::PlugConfig(2))
        );
        assert_eq!(
            placement("plugs[0].env.X", calls),
            Some(Placement::PlugEnv(0))
        );
        assert_eq!(placement("plugs[0].name", calls), None);
        assert_eq!(
            placement("main[0].with.x", calls),
            Some(Placement::PhaseWith(PhaseStage::Main, 0))
        );
        assert_eq!(
            placement("main[1].with.x", calls),
            Some(Placement::CallWith(PhaseStage::Main, 1))
        );
        assert_eq!(
            placement("setup[3].env.A", calls),
            Some(Placement::PhaseEnv(PhaseStage::Setup, 3))
        );
        assert_eq!(placement("main[0].name", calls), None);
        assert_eq!(placement("main[0].executable.command", calls), None);
        assert_eq!(
            placement("unit.serial_number.default_value", calls),
            Some(Placement::IdentifyDefault)
        );
        assert_eq!(
            placement("unit.metadata.line.default_value", calls),
            Some(Placement::IdentifyDefault)
        );
        assert_eq!(
            placement("operated_by.default_value", calls),
            Some(Placement::IdentifyDefault)
        );
        assert_eq!(placement("unit.serial_number.pattern", calls), None);
    }

    #[test]
    fn a_whole_marker_keeps_its_type_an_embedded_one_becomes_text() {
        let run = json!({"slot": {"key": "s1", "name": "Nest 1"}, "station": {"id": null, "name": "bench"}});
        let config = BTreeMap::from([
            ("GAIN".to_string(), json!(2.5)),
            ("ON".to_string(), json!(true)),
        ]);
        let secrets = BTreeMap::from([("TOKEN".to_string(), json!("hunter2hunter2"))]);
        let values = MarkerValues {
            run: &run,
            config: &config,
            secrets: &secrets,
            slot_aliases: true,
        };
        assert_eq!(values.expand_str("{config.GAIN}"), json!(2.5));
        assert_eq!(values.expand_str("{config.ON}"), json!(true));
        assert_eq!(values.expand_str("x{config.GAIN}"), json!("x2.5"));
        assert_eq!(
            values.expand_str("{run.slot.key}/{slot_name}"),
            json!("s1/Nest 1")
        );
        assert_eq!(values.expand_str("{slot}"), json!("s1"));
        assert_eq!(values.expand_str("{run.station.id}"), json!(null));
        assert_eq!(values.expand_str("id={run.station.id}"), json!("id="));
        assert_eq!(
            values.expand_str("Bearer {secrets.TOKEN}"),
            json!("Bearer hunter2hunter2")
        );
        // Unknown and load-stage markers stay as written.
        assert_eq!(
            values.expand_str("{inputs.x} {foo.bar} {run.nope}"),
            json!("{inputs.x} {foo.bar} {run.nope}")
        );
        assert_eq!(
            values.expand(&json!({"a": ["{config.GAIN}", {"b": "{slot}"}]})),
            json!({"a": [2.5, {"b": "s1"}]})
        );
        // Under the old `config:` the aliases are text.
        let config = MarkerValues {
            slot_aliases: false,
            ..values
        };
        assert_eq!(config.expand_str("{slot}"), json!("{slot}"));
        assert_eq!(
            config.expand_str("fixture-{slot}-{run.slot.key}"),
            json!("fixture-{slot}-s1")
        );
    }
}
