//! Load-time rules of `with:`, `env:`, `config:` and `secrets:`.
//!
//! Two passes. The text pass reads every value scalar with its YAML path,
//! so it knows where a marker sits: a run-start marker outside `with:`,
//! `env:` and identify defaults stays as written with a warning (refused
//! in a sequence call's `with:`), and the old slot spellings (`{slot}`,
//! `{slot_name}`) are warned about at their line. The
//! old `config:` of a plug is warned about by `yaml::warnings` only (the
//! run's console and logs): the loaded definition records the spelling
//! (`PlugDefinition::with_written_as_config`) for its own messages. The definition
//! pass judges what the loaded procedure keeps for the run: declarations,
//! `env:` keys, every marker against what is declared and against the
//! slot it can see, and the `with:` keys of a phase against the names the
//! worker already gives a parameter.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::loader::LoadError;
use super::markers::{
    did_you_mean, find_markers, placement, MarkerRef, Placement, NAMESPACES, RUN_FIELDS,
};
use super::schema::{PhaseStage, ProcedureDefinition, RefList, RefLocation, Scope};
use super::values::{
    is_valid_env_key, is_valid_value_key, ConfigSpecYaml, SecretSpecYaml, ValueType,
    RESERVED_PREFIX,
};
use super::yaml::Warning;

/// The parameters the worker fills itself. A phase's `with:` may not name
/// one: the phase would receive the injectable or the value depending on
/// lookup order.
pub const INJECTABLES: &[&str] = &[
    "phase",
    "run",
    "measurements",
    "log",
    "ui",
    "unit",
    "attach",
    "dashboard",
    "inputs",
    "config",
    "secrets",
];

/// A refusal found by the text pass: the YAML path of the scalar, its
/// position, and the message.
pub(crate) struct TextError {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

/// What the text pass found.
#[derive(Default)]
pub(crate) struct TextDiagnostics {
    pub errors: Vec<TextError>,
    pub warnings: Vec<Warning>,
}

/// Which file the text pass reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileKind {
    Procedure,
    Sequence,
}

/// The text pass over a procedure or a sequence file (see the module doc).
/// A text the raw parser refuses yields nothing: the typed read reports it.
pub(crate) fn text_diagnostics(text: &str, _kind: FileKind) -> TextDiagnostics {
    let mut scalars: Vec<(String, String, u32, u32)> = Vec::new();
    let visited = super::yaml::rewrite_value_scalars(text, |scalar| {
        scalars.push((
            scalar.path.clone(),
            scalar.value.to_string(),
            scalar.line as u32,
            scalar.column as u32,
        ));
        None
    });
    let mut out = TextDiagnostics::default();
    if visited.is_err() {
        return out;
    }
    // An entry with `uses:` is a sequence call: its `with:` is the call's
    // inputs, resolved when the procedure loads.
    let calls: HashSet<(PhaseStage, usize)> = scalars
        .iter()
        .filter_map(|(path, ..)| entry_of(path.strip_suffix(".uses")?))
        .collect();
    let is_call = |stage: PhaseStage, index: usize| calls.contains(&(stage, index));

    for (path, value, line, column) in &scalars {
        // A duration without a unit is milliseconds: `timeout: 30` gives a
        // phase 30ms. Under a second, that is a forgotten `s`.
        if is_duration_path(path)
            && !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u64>().is_ok_and(|n| n > 0 && n < 1000)
        {
            out.warnings.push(Warning::notice(
                *line,
                *column,
                super::codes::BARE_DURATION,
                format!(
                    "{path}: `{value}` is {value} milliseconds; write `{value}s` for seconds, or `{value}ms` to keep it"
                ),
            ));
        }
        let at = placement(path, is_call);
        for marker in find_markers(value) {
            let reference = &marker.reference;
            if !reference.is_known() {
                // `{vars.*}` was renamed: kept as text it would reach the
                // plug as a literal and fail far from the cause.
                if let (Some(_), MarkerRef::Namespaced { namespace, field }) = (at, reference) {
                    if namespace == "vars" {
                        out.errors.push(TextError {
                            path: path.clone(),
                            line: *line,
                            column: *column,
                            message: format!("`{{vars.{field}}}` is now `{{config.{field}}}`"),
                        });
                        continue;
                    }
                }
                // Text (a format string of the plug's own); warned about
                // only where it looks like a misspelt marker.
                if at.is_some_and(Placement::takes_run_markers) {
                    if let MarkerRef::Namespaced { namespace, field } = reference {
                        if let Some(ns) =
                            super::pysource::closest(namespace, NAMESPACES.iter().copied())
                        {
                            out.warnings.push(Warning::notice(
                                *line,
                                *column,
                                super::codes::UNKNOWN_MARKER,
                                format!(
                                    "{path}: `{}` is kept as text: markers are {{inputs.*}}, {{run.*}}, {{config.*}} and {{secrets.*}}; did you mean `{{{ns}.{field}}}`?",
                                    reference.written()
                                ),
                            ));
                        }
                    }
                }
                continue;
            }
            if !reference.is_run_start() {
                continue;
            }
            let written = reference.written();
            if let Some(replacement) = reference.replacement() {
                // An alias is only an alias where run-start markers resolve;
                // elsewhere, and under the old `config:`, it has always
                // been plain text.
                if at.is_some_and(Placement::takes_slot_aliases) {
                    out.warnings.push(Warning::deprecation(
                        *line,
                        *column,
                        format!("{path}: `{written}` is the old spelling of `{replacement}`; write `{replacement}`"),
                    ));
                }
                continue;
            }
            match at {
                // Text written before markers existed: kept as is.
                None => out.warnings.push(Warning::notice(
                    *line,
                    *column,
                    super::codes::MARKER_AS_TEXT,
                    format!(
                        "{path}: `{written}` is kept as text: markers resolve only in `with:`, `env:` and a unit field's `default_value`"
                    ),
                )),
                Some(Placement::CallWith(..)) => out.errors.push(TextError {
                    path: path.clone(),
                    line: *line,
                    column: *column,
                    message: format!(
                        "`{written}` cannot be passed to a sequence call: its `with:` is resolved when the procedure loads; declare the key under the sequence file's `{}:` and read it there",
                        reference.namespace()
                    ),
                }),
                Some(Placement::ProcedureEnv | Placement::PlugEnv(_) | Placement::PhaseEnv(..))
                    if reference.namespace() == "secrets" =>
                {
                    out.warnings.push(Warning::notice(
                        *line,
                        *column,
                        super::codes::SECRET_IN_ENV,
                        format!(
                            "{path}: `{written}` puts a secret in the environment, which every child process inherits; prefer `with:` or reading `secrets` in the phase"
                        ),
                    ));
                }
                Some(_) => {}
            }
        }
    }

    out.warnings.sort_by_key(|w| (w.line, w.column));
    out
}

/// A schema duration (phase `timeout`, `retry.delay`, plug
/// `init_timeout`), not a same-named key under `with:` or `inputs:`.
fn is_duration_path(path: &str) -> bool {
    let segments: Vec<&str> = path
        .split('.')
        .map(|s| s.split('[').next().unwrap_or(s))
        .collect();
    matches!(
        segments.as_slice(),
        ["setup" | "main" | "teardown", "timeout"]
            | ["setup" | "main" | "teardown", "retry", "delay"]
            | ["plugs", "init_timeout"]
    )
}

/// The text pass's warnings alone, plus the old `config:` of a plug: what
/// `yaml::warnings` adds for the run's console and logs.
pub(crate) fn text_warnings(text: &str, kind: FileKind) -> Vec<Warning> {
    let mut warnings = text_diagnostics(text, kind).warnings;
    if kind == FileKind::Procedure {
        warnings.extend(config_deprecations(text));
    }
    warnings
}

/// `config:` on a plug, the old spelling of `with:`, at its line.
pub(crate) fn config_deprecations(text: &str) -> Vec<Warning> {
    let positions = super::yaml::positions(text);
    let mut out = Vec::new();
    for index in 0.. {
        if positions.exact(&format!("plugs[{index}]")).is_none() {
            break;
        }
        if let Some((line, column)) = positions.exact(&format!("plugs[{index}].config")) {
            out.push(Warning::deprecation(
                line,
                column,
                format!(
                    "plugs[{index}].config: `config:` is the old spelling of `with:`; rename it"
                ),
            ));
        }
    }
    out
}

/// `(stage, index)` of `main[2]`.
fn entry_of(path: &str) -> Option<(PhaseStage, usize)> {
    let (list, rest) = path.split_once('[')?;
    let index: usize = rest.strip_suffix(']')?.parse().ok()?;
    let stage = match list {
        "setup" => PhaseStage::Setup,
        "main" => PhaseStage::Main,
        "teardown" => PhaseStage::Teardown,
        _ => return None,
    };
    Some((stage, index))
}

/// The rules of one file's `config:` and `secrets:` declarations, as
/// `config.KEY: message` lines.
pub(crate) fn check_declarations(
    config: &BTreeMap<String, ConfigSpecYaml>,
    secrets: &BTreeMap<String, SecretSpecYaml>,
) -> Vec<String> {
    let mut out = Vec::new();
    let keys = config
        .keys()
        .map(|k| ("config", k))
        .chain(secrets.keys().map(|k| ("secrets", k)));
    for (namespace, key) in keys {
        if !is_valid_value_key(key) {
            out.push(format!(
                "{namespace}.{key}: invalid key `{key}`: expected upper case letters, digits and _, not starting with a digit"
            ));
        } else if key.starts_with(RESERVED_PREFIX) {
            out.push(format!(
                "{namespace}.{key}: `{RESERVED_PREFIX}*` is reserved for the variables TofuPilot sets"
            ));
        }
    }
    for key in config.keys().filter(|k| secrets.contains_key(*k)) {
        out.push(format!(
            "secrets.{key}: `{key}` is declared under config: and secrets:; a key is one or the other"
        ));
    }
    for (key, spec) in config {
        if let Some(default) = &spec.default {
            if spec.required {
                out.push(format!(
                    "config.{key}: `required: true` and `default:` together: a config value with a default is never missing; keep one"
                ));
            } else if !accepts(spec.value_type, default) {
                out.push(format!(
                    "config.{key}.default: expected {}, got {}",
                    spec.value_type.with_article(),
                    describe(default)
                ));
            }
        }
    }
    out
}

/// Whether `value` is of `value_type` as written in the YAML (a whole
/// float passes for an integer, like an input's).
fn accepts(value_type: ValueType, value: &serde_json::Value) -> bool {
    match value_type {
        ValueType::String => value.is_string(),
        ValueType::Number => value.is_number(),
        ValueType::Integer => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        ValueType::Boolean => value.is_boolean(),
    }
}

fn describe(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => format!("the boolean {b}"),
        serde_json::Value::Number(n) => format!("the number {n}"),
        serde_json::Value::String(s) => format!("the string {s:?}"),
        serde_json::Value::Array(_) => "a list".to_string(),
        serde_json::Value::Object(_) => "a mapping".to_string(),
    }
}

/// The keys one place may read, and whether it has a slot.
struct MarkerScope<'a> {
    config: &'a BTreeSet<String>,
    secrets: &'a BTreeSet<String>,
    /// `None` when the place serves one slot; otherwise why it has none.
    no_slot: Option<&'a str>,
    /// `{slot}` / `{slot_name}` resolve here; under a plug's old
    /// `config:` they are text.
    slot_aliases: bool,
}

/// Every problem of the run-start markers in `value`, as messages.
fn check_markers(value: &serde_json::Value, scope: &MarkerScope<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for reference in super::markers::markers_in_value(value) {
        if let Some(problem) = check_marker(&reference, scope) {
            if !out.contains(&problem) {
                out.push(problem);
            }
        }
    }
    out
}

fn check_marker(reference: &MarkerRef, scope: &MarkerScope<'_>) -> Option<String> {
    if reference.is_alias() && !scope.slot_aliases {
        return None;
    }
    let written = reference.written();
    if reference.reads_slot() {
        if let Some(why) = scope.no_slot {
            return Some(format!("`{written}` reads the slot, but {why}"));
        }
    }
    let MarkerRef::Namespaced { namespace, field } = reference else {
        return None;
    };
    match namespace.as_str() {
        "inputs" => None,
        "run" => (!RUN_FIELDS.contains(&field.as_str())).then(|| {
            let hint = super::pysource::closest(field, RUN_FIELDS.iter().copied())
                .map(|f| format!("; did you mean `{{run.{f}}}`?"))
                .unwrap_or_default();
            format!(
                "unknown field `{written}`: {{run.*}} fields are {}{hint}",
                RUN_FIELDS.join(", ")
            )
        }),
        "config" | "secrets" => {
            let declared = if namespace == "config" {
                scope.config
            } else {
                scope.secrets
            };
            if declared.contains(field) {
                return None;
            }
            let other = if namespace == "config" {
                scope.secrets
            } else {
                scope.config
            };
            if other.contains(field) {
                let other_ns = if namespace == "config" {
                    "secrets"
                } else {
                    "config"
                };
                return Some(format!(
                    "`{written}`: `{field}` is declared under {other_ns}:; write `{{{other_ns}.{field}}}`"
                ));
            }
            Some(format!(
                "`{written}` is not declared: add `{field}` under {namespace}:{}",
                did_you_mean(field, declared.iter().map(String::as_str))
            ))
        }
        // Any other namespace is text (the text pass warns about a
        // likely misspelling).
        _ => None,
    }
}

/// Every refusal of the definition pass (see the module doc).
pub(crate) fn check_definition(def: &ProcedureDefinition) -> Vec<LoadError> {
    let mut errors = Vec::new();

    for message in check_declarations(&def.config, &def.secrets) {
        errors.push(LoadError::rule(message));
    }
    // A key read by the procedure and a sequence file is one value: one
    // kind, one type.
    let mut seen: BTreeMap<String, (&str, ValueType, String)> = BTreeMap::new();
    let procedure_label = "the procedure".to_string();
    let mut declare = |key: &String,
                       namespace: &'static str,
                       value_type: ValueType,
                       owner: &String| {
        match seen.get(key) {
            None => {
                seen.insert(key.clone(), (namespace, value_type, owner.clone()));
            }
            Some((ns, t, first)) if *ns != namespace || *t != value_type => {
                errors.push(LoadError::text(format!(
                    "`{key}` is declared as {} under {ns}: in {first} but as {} under {namespace}: in {owner}; one key is one value",
                    t.with_article(),
                    value_type.with_article()
                )));
            }
            Some(_) => {}
        }
    };
    for (key, spec) in &def.config {
        declare(key, "config", spec.value_type, &procedure_label);
    }
    for (key, spec) in &def.secrets {
        declare(key, "secrets", spec.value_type, &procedure_label);
    }
    for instance in &def.sequences {
        for (key, spec) in &instance.config {
            declare(key, "config", spec.value_type, &instance.file);
        }
        for (key, spec) in &instance.secrets {
            declare(key, "secrets", spec.value_type, &instance.file);
        }
    }

    let procedure_config: BTreeSet<String> = def.config.keys().cloned().collect();
    let procedure_secrets: BTreeSet<String> = def.secrets.keys().cloned().collect();
    let procedure_scope = |no_slot| MarkerScope {
        config: &procedure_config,
        secrets: &procedure_secrets,
        no_slot,
        slot_aliases: true,
    };

    // The procedure's env: serves every process of the run, all slots.
    for (key, value) in &def.env {
        if let Some(problem) = env_key_problem(key) {
            errors.push(LoadError::on(format!("env.{key}"), problem));
        }
        for problem in check_markers(
            &serde_json::Value::String(value.clone()),
            &procedure_scope(Some("the procedure's env: is shared by every slot")),
        ) {
            errors.push(LoadError::on(format!("env.{key}"), problem));
        }
    }

    for (index, plug) in def.plugs.iter().enumerate() {
        let at = |key: &str| RefLocation::key(RefList::Plugs, index, key);
        if plug.with_written_twice {
            errors.push(LoadError::at(
                "`with:` and `config:` both set; `config:` is the old spelling of `with:`, keep `with:` only",
                RefLocation::entry(RefList::Plugs, index),
            ));
        }
        let no_slot = match plug.scope {
            Scope::Slot => None,
            Scope::Execution => {
                Some("this plug is execution-scoped: one instance serves every slot")
            }
            Scope::Station => {
                Some("this plug is station-scoped: one instance serves every run of the station")
            }
        };
        if let Some(with) = &plug.with {
            for (key, value) in with {
                let scope = MarkerScope {
                    slot_aliases: !plug.with_written_as_config,
                    ..procedure_scope(no_slot)
                };
                for problem in check_markers(value, &scope) {
                    errors.push(LoadError::at(
                        problem,
                        at(&format!("{}.{key}", plug.with_key())),
                    ));
                }
            }
        }
        if plug.scope == Scope::Station && !plug.env.is_empty() {
            errors.push(LoadError::at(
                "a station-scoped plug serves runs of several procedures and only receives the procedure's env:; move these variables there",
                at("env"),
            ));
        }
        for (key, value) in &plug.env {
            if let Some(problem) = env_key_problem(key) {
                errors.push(LoadError::at(problem, at(&format!("env.{key}"))));
            }
            for problem in check_markers(
                &serde_json::Value::String(value.clone()),
                &procedure_scope(no_slot),
            ) {
                errors.push(LoadError::at(problem, at(&format!("env.{key}"))));
            }
        }
    }

    // Identify defaults are expanded per slot.
    let mut identify_fields: Vec<(String, &super::schema::UnitFieldConfig)> = def
        .unit
        .as_ref()
        .map(|unit| unit.named_fields())
        .unwrap_or_default();
    if let Some(operated_by) = &def.operated_by {
        identify_fields.push(("operated_by".to_string(), operated_by));
    }
    for (path, field) in identify_fields {
        if let Some(value) = &field.default_value {
            for problem in check_markers(
                &serde_json::Value::String(value.clone()),
                &procedure_scope(None),
            ) {
                errors.push(LoadError::on(format!("{path}.default_value"), problem));
            }
        }
    }

    // Phases: `with:` keys, `env:` keys, and the markers of both.
    let plug_keys: HashSet<String> = def.plugs.iter().map(|p| p.key.to_lowercase()).collect();
    let phase_keys: HashSet<&str> = def
        .setup
        .iter()
        .chain(def.main.iter())
        .chain(def.teardown.iter())
        .flat_map(|p| [p.key.as_str(), p.local_key()])
        .collect();
    for (list, phases) in [
        (RefList::Setup, &def.setup),
        (RefList::Main, &def.main),
        (RefList::Teardown, &def.teardown),
    ] {
        for (index, phase) in phases.iter().enumerate() {
            let at = |key: &str| def.written_location(list, index, key);
            let shared = list != RefList::Main && phase.scope == Some(Scope::Execution);
            let no_slot =
                shared.then_some("this phase runs once for all slots (`scope: execution`)");
            let (config, secrets) = match &phase.value_scope {
                Some(scope) => (&scope.config, &scope.secrets),
                None => (&procedure_config, &procedure_secrets),
            };
            let scope = MarkerScope {
                config,
                secrets,
                no_slot,
                slot_aliases: true,
            };
            let named = format!("Phase `{}`: ", phase.key);
            for (key, value) in &phase.with {
                if let Some(problem) = with_key_problem(key, phase, &plug_keys, &phase_keys) {
                    errors.push(LoadError::at(format!("{named}{problem}"), at("with")));
                }
                for problem in check_markers(value, &scope) {
                    errors.push(LoadError::at(
                        format!("{named}with.{key}: {problem}"),
                        at("with"),
                    ));
                }
            }
            for (key, value) in &phase.env {
                if let Some(problem) = env_key_problem(key) {
                    errors.push(LoadError::at(
                        format!("{named}env.{key}: {problem}"),
                        at("env"),
                    ));
                }
                for problem in check_markers(&serde_json::Value::String(value.clone()), &scope) {
                    errors.push(LoadError::at(
                        format!("{named}env.{key}: {problem}"),
                        at("env"),
                    ));
                }
            }
        }
    }
    errors
}

fn env_key_problem(key: &str) -> Option<String> {
    if !is_valid_env_key(key) {
        return Some(format!(
            "invalid variable name `{key}`: expected letters, digits and _, not starting with a digit"
        ));
    }
    key.to_ascii_uppercase()
        .starts_with(RESERVED_PREFIX)
        .then(|| format!("`{RESERVED_PREFIX}*` is reserved for the variables TofuPilot sets"))
}

/// Why a phase's `with:` may not use `key`, if it may not.
fn with_key_problem(
    key: &str,
    phase: &super::schema::PhaseDefinition,
    plug_keys: &HashSet<String>,
    phase_keys: &HashSet<&str>,
) -> Option<String> {
    if !crate::python::is_valid_python_identifier(key) {
        return Some(format!(
            "with.{key}: `{key}` is not a Python identifier, so no parameter can take it"
        ));
    }
    if INJECTABLES.contains(&key) {
        return Some(format!(
            "with.{key}: `{key}` is a parameter the runtime fills; rename the function's parameter"
        ));
    }
    let lower = key.to_lowercase();
    if plug_keys.contains(&lower)
        || phase
            .plug_aliases
            .keys()
            .any(|alias| alias.to_lowercase() == lower)
    {
        return Some(format!(
            "with.{key}: `{key}` is a plug key, which a parameter of that name receives; rename the function's parameter"
        ));
    }
    if phase_keys.contains(key) {
        return Some(format!(
            "with.{key}: `{key}` is a phase key, whose results a parameter of that name receives; rename the function's parameter"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_schema_duration_without_its_unit_is_flagged() {
        let text = "name: P\nplugs:\n  - key: dmm\n    name: DMM\n    python: d:D\n    init_timeout: 20\n    with:\n      timeout: 5\nmain:\n  - name: A\n    timeout: 30\n    retry:\n      limit: 2\n      delay: 500ms\n";
        let flagged: Vec<String> = text_diagnostics(text, FileKind::Procedure)
            .warnings
            .into_iter()
            .filter(|w| w.code == super::super::codes::BARE_DURATION)
            .map(|w| w.message)
            .collect();
        assert_eq!(flagged.len(), 2, "{flagged:?}");
        assert!(
            flagged[0].starts_with("plugs[0].init_timeout: `20`"),
            "{flagged:?}"
        );
        assert!(
            flagged[1].starts_with("main[0].timeout: `30`"),
            "{flagged:?}"
        );
    }

    #[test]
    fn declarations() {
        let config: BTreeMap<String, ConfigSpecYaml> = crate::procedure::yaml::from_str(
            "lower: {}\nTOFUPILOT_X: {}\nBOTH: {}\nBAD: {type: integer, default: 1.5}\nREQ: {required: true, default: a}\nOK: {type: integer, default: 3}\n",
        )
        .unwrap();
        let secrets: BTreeMap<String, SecretSpecYaml> =
            crate::procedure::yaml::from_str("BOTH: {}\nTOKEN: {required: true}\n").unwrap();
        let problems = check_declarations(&config, &secrets);
        let has = |s: &str| problems.iter().any(|p| p.contains(s));
        assert!(has("config.lower: invalid key"), "{problems:?}");
        assert!(
            has("config.TOFUPILOT_X: `TOFUPILOT_*` is reserved"),
            "{problems:?}"
        );
        assert!(
            has("secrets.BOTH: `BOTH` is declared under config: and secrets:"),
            "{problems:?}"
        );
        assert!(
            has("config.BAD.default: expected an integer, got the number 1.5"),
            "{problems:?}"
        );
        assert!(
            has("config.REQ: `required: true` and `default:` together"),
            "{problems:?}"
        );
        assert_eq!(problems.len(), 5, "{problems:?}");
    }
}

/// The rules through the loader, on procedure texts.
#[cfg(test)]
mod load_tests {
    use crate::procedure::loader::inspect_procedure_definition;

    const MAIN: &str = "main:\n  - name: M\n    python: phases.m:m\n";

    fn errors(yaml: &str) -> Vec<String> {
        inspect_procedure_definition(yaml)
            .errors
            .iter()
            .map(|e| e.detail())
            .collect()
    }

    fn has(errors: &[String], needle: &str) -> bool {
        errors.iter().any(|e| e.contains(needle))
    }

    #[test]
    fn markers_in_with_env_and_defaults_load() {
        let yaml = format!(
            "name: A
config:
  MES_URL: {{required: true}}
  GAIN: {{type: number, default: 1.5}}
secrets:
  MES_TOKEN: {{}}
env:
  MES: \"{{config.MES_URL}}\"
  STATION: \"{{run.station.name}}\"
unit:
  serial_number:
    default_value: \"SN-{{run.slot.key}}-{{config.MES_URL}}\"
plugs:
  - name: Mes
    python: plugs.mes:Mes
    with:
      url: \"{{config.MES_URL}}\"
      token: \"{{secrets.MES_TOKEN}}\"
      slot: \"{{run.slot.name}}\"
    env:
      GAIN: \"{{config.GAIN}}\"
{MAIN}    with:
      gain: \"{{config.GAIN}}\"
      where: \"{{run.deployment.environment}}/{{run.slot.key}}\"
    env:
      X: \"{{run.procedure.version}}\"
"
        );
        let outcome = inspect_procedure_definition(&yaml);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let def = outcome.definition.unwrap();
        assert_eq!(def.main[0].with["gain"], "{config.GAIN}");
        assert_eq!(def.plugs[0].env["GAIN"], "{config.GAIN}");
        assert_eq!(def.env["STATION"], "{run.station.name}");
    }

    #[test]
    fn a_run_start_marker_elsewhere_is_text_with_a_warning() {
        let yaml =
            format!("name: A\nconfig:\n  X: {{}}\n{MAIN}    description: \"uses {{config.X}}\"\n");
        let outcome = inspect_procedure_definition(&yaml);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let warning = outcome
            .warnings
            .iter()
            .find(|w| w.detail.contains("is kept as text"))
            .unwrap_or_else(|| panic!("{:?}", outcome.warnings));
        assert!(
            warning
                .detail
                .starts_with("main[0].description: `{config.X}`"),
            "{warning:?}"
        );
        assert_eq!(warning.line, 7);
        assert_eq!(
            outcome.definition.unwrap().main[0].description.as_deref(),
            Some("uses {config.X}")
        );
        // `{slot}` outside the run-start places is text, as it always was.
        assert!(errors(&format!(
            "name: A\n{MAIN}    description: \"slot {{slot}}\"\n"
        ))
        .is_empty());
    }

    #[test]
    fn the_old_vars_marker_is_refused_with_its_new_name() {
        let yaml = format!(
            "name: A
config:
  GAIN: {{}}
env:
  E: \"{{vars.GAIN}}\"
unit:
  serial_number:
    default_value: \"SN-{{vars.PREFIX}}\"
{MAIN}    with:
      gain: \"{{vars.GAIN}}\"
    description: \"see {{vars.GAIN}}\"
"
        );
        let errors = errors(&yaml);
        assert!(
            has(&errors, "`{vars.GAIN}` is now `{config.GAIN}`"),
            "{errors:?}"
        );
        assert!(
            has(&errors, "`{vars.PREFIX}` is now `{config.PREFIX}`"),
            "{errors:?}"
        );
        // with:, env: and the unit default; a description is plain text.
        assert_eq!(
            errors
                .iter()
                .filter(|e| e.contains("is now `{config."))
                .count(),
            3,
            "{errors:?}"
        );
    }

    /// Procedures that loaded before markers existed keep loading
    /// (adversarial review C1).
    #[test]
    fn text_written_before_markers_still_loads() {
        let yaml = format!(
            "name: A
plugs:
  - name: Log
    python: plugs.log:Log
    with:
      fmt: \"{{record.levelname}}: {{record.message}}\"
  - name: Fixture
    scope: execution
    python: plugs.f:F
    config:
      label: \"fixture-{{slot}}\"
      fmt: \"{{record.levelname}}\"
{MAIN}    description: \"Prints {{run.station.name}}\"
    with:
      fmt: \"{{record.levelname}}: {{record.message}}\"
    env:
      FMT: \"{{record.levelname}}\"
"
        );
        let outcome = inspect_procedure_definition(&yaml);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        // No noise for a namespace that is no misspelt marker, no
        // deprecation for `{{slot}}` under `config:`.
        let details: Vec<&str> = outcome.warnings.iter().map(|w| w.detail.as_str()).collect();
        assert_eq!(
            details,
            ["main[0].description: `{run.station.name}` is kept as text: markers resolve only in `with:`, `env:` and a unit field's `default_value`"],
        );
        let def = outcome.definition.unwrap();
        assert_eq!(
            def.plugs[0].with.as_ref().unwrap()["fmt"],
            "{record.levelname}: {record.message}"
        );
        assert_eq!(
            def.main[0].description.as_deref(),
            Some("Prints {run.station.name}")
        );

        // At run time: the unknown namespace and `{slot}` under `config:`
        // reach the plug as written.
        let values =
            crate::procedure::values::resolve_values(&def, Default::default(), &Default::default())
                .unwrap();
        let slot = crate::procedure::values::SlotContext {
            key: "s1".into(),
            name: "Nest 1".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plugs")).unwrap();
        std::fs::write(dir.path().join("plugs/log.py"), "class Log:\n    pass\n").unwrap();
        std::fs::write(dir.path().join("plugs/f.py"), "class F:\n    pass\n").unwrap();
        let log = def.plugs[0]
            .to_resolved_config_json(dir.path(), &values, Some(&slot))
            .unwrap();
        assert_eq!(log["config"]["fmt"], "{record.levelname}: {record.message}");
        let fixture = def.plugs[1]
            .to_resolved_config_json(dir.path(), &values, None)
            .unwrap();
        assert_eq!(fixture["config"]["label"], "fixture-{slot}");
        let with = values.expand(
            &serde_json::Value::Object(def.main[0].with.clone().into_iter().collect()),
            Some(&slot),
        );
        assert_eq!(with["fmt"], "{record.levelname}: {record.message}");
        assert_eq!(
            values.expand_env(&def.main[0].env, Some(&slot)),
            [("FMT".to_string(), "{record.levelname}".to_string())]
        );
    }

    #[test]
    fn the_slot_aliases_still_resolve_under_with() {
        let yaml = format!(
            "name: A\nplugs:\n  - name: Fixture\n    scope: execution\n    python: plugs.f:F\n    with:\n      label: \"fixture-{{slot}}\"\n{MAIN}"
        );
        assert!(has(
            &errors(&yaml),
            "`{slot}` reads the slot, but this plug is execution-scoped"
        ));
    }

    #[test]
    fn unknown_namespaces_fields_and_undeclared_keys_say_what_was_meant() {
        let yaml = format!(
            "name: A
config:
  MES_URL: {{}}
secrets:
  TOKEN: {{}}
{MAIN}    with:
      a: \"{{confg.MES_URL}}\"
      b: \"{{run.slot.nme}}\"
      c: \"{{config.MES_ULR}}\"
      d: \"{{config.TOKEN}}\"
      e: \"{{secrets.NOPE}}\"
"
        );
        let outcome = inspect_procedure_definition(&yaml);
        // A namespace that is none of the four is text; a likely
        // misspelling is a warning.
        assert!(
            outcome.warnings.iter().any(|w| w.detail
                == "main[0].with.a: `{confg.MES_URL}` is kept as text: markers are {inputs.*}, {run.*}, {config.*} and {secrets.*}; did you mean `{config.MES_URL}`?"),
            "{:?}",
            outcome.warnings
        );
        let errors = errors(&yaml);
        assert!(!has(&errors, "{confg.MES_URL}"), "{errors:?}");
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(
            has(
                &errors,
                "unknown field `{run.slot.nme}`: {run.*} fields are slot.key"
            ),
            "{errors:?}"
        );
        assert!(
            has(&errors, "did you mean `{run.slot.name}`?"),
            "{errors:?}"
        );
        assert!(has(&errors, "`{config.MES_ULR}` is not declared: add `MES_ULR` under config:; did you mean `MES_URL`?"), "{errors:?}");
        assert!(
            has(
                &errors,
                "`{config.TOKEN}`: `TOKEN` is declared under secrets:; write `{secrets.TOKEN}`"
            ),
            "{errors:?}"
        );
        assert!(
            has(
                &errors,
                "`{secrets.NOPE}` is not declared: add `NOPE` under secrets:"
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn the_slot_is_refused_where_no_single_slot_exists() {
        let yaml = format!(
            "name: A
env:
  S: \"{{run.slot.key}}\"
plugs:
  - name: Station Psu
    key: spsu
    scope: station
    python: plugs.p:P
    with:
      nest: \"{{run.slot.name}}\"
  - name: Shared
    scope: execution
    python: plugs.p:P
    with:
      nest: \"{{slot}}\"
setup:
  - name: Once
    scope: execution
    python: phases.s:s
    with:
      nest: \"{{run.slot.key}}\"
{MAIN}"
        );
        let errors = errors(&yaml);
        assert!(has(&errors, "env.S: `{run.slot.key}` reads the slot, but the procedure's env: is shared by every slot"), "{errors:?}");
        assert!(
            has(
                &errors,
                "`{run.slot.name}` reads the slot, but this plug is station-scoped"
            ),
            "{errors:?}"
        );
        assert!(
            has(
                &errors,
                "`{slot}` reads the slot, but this plug is execution-scoped"
            ),
            "{errors:?}"
        );
        assert!(
            has(
                &errors,
                "`{run.slot.key}` reads the slot, but this phase runs once for all slots"
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn a_station_plug_takes_no_env_of_its_own() {
        let yaml = format!(
            "name: A\nplugs:\n  - name: P\n    scope: station\n    python: plugs.p:P\n    env:\n      A: b\n{MAIN}"
        );
        assert!(has(&errors(&yaml), "only receives the procedure's env:"));
    }

    #[test]
    fn env_keys_are_names_and_tofupilot_is_reserved() {
        let yaml = format!("name: A\nenv:\n  1X: a\n  TOFUPILOT_API_KEY: b\n  PORT: 8080\n{MAIN}");
        let outcome = inspect_procedure_definition(&yaml);
        let errors: Vec<String> = outcome.errors.iter().map(|e| e.detail()).collect();
        assert!(has(&errors, "env.1X: invalid variable name"), "{errors:?}");
        assert!(
            has(&errors, "env.TOFUPILOT_API_KEY: `TOFUPILOT_*` is reserved"),
            "{errors:?}"
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
    }

    #[test]
    fn declarations_are_checked() {
        let yaml = format!(
            "name: A\nconfig:\n  mes: {{}}\n  N: {{type: integer, default: \"3\"}}\nsecrets:\n  TOFUPILOT_X: {{}}\n{MAIN}"
        );
        let errors = errors(&yaml);
        assert!(has(&errors, "config.mes: invalid key"), "{errors:?}");
        assert!(
            has(
                &errors,
                "config.N.default: expected an integer, got the string \"3\""
            ),
            "{errors:?}"
        );
        assert!(
            has(&errors, "secrets.TOFUPILOT_X: `TOFUPILOT_*` is reserved"),
            "{errors:?}"
        );
        // A secret has no default.
        let outcome = inspect_procedure_definition(&format!(
            "name: A\nsecrets:\n  T: {{default: x}}\n{MAIN}"
        ));
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.message.contains("unknown field `default`")),
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn a_phase_with_key_may_not_shadow_an_injectable_a_plug_or_a_phase() {
        let yaml = "name: A
plugs:
  - name: Dmm
    python: plugs.d:D
main:
  - name: First
    python: phases.m:first
  - name: M
    python: phases.m:m
    with:
      config: 1
      dmm: 2
      first: 3
      ok_key: 4
";
        let errors = errors(yaml);
        assert!(
            has(
                &errors,
                "with.config: `config` is a parameter the runtime fills"
            ),
            "{errors:?}"
        );
        assert!(has(&errors, "with.dmm: `dmm` is a plug key"), "{errors:?}");
        assert!(
            has(&errors, "with.first: `first` is a phase key"),
            "{errors:?}"
        );
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn with_on_a_phase_without_python_and_env_on_a_call_are_refused() {
        assert!(has(
            &errors("name: A\nmain:\n  - name: M\n    with: {a: 1}\n"),
            "`with` only belongs on a sequence call or on a phase with `python:`"
        ));
    }

    #[test]
    fn config_is_the_old_spelling_of_with() {
        let yaml = format!(
            "name: A\nplugs:\n  - name: P\n    python: plugs.p:P\n    config:\n      a: 1\n{MAIN}"
        );
        let def = inspect_procedure_definition(&yaml).definition.unwrap();
        assert_eq!(def.plugs[0].with.as_ref().unwrap()["a"], 1);
        assert_eq!(def.plugs[0].with_key(), "config");
        let warnings = crate::procedure::yaml::warnings(&yaml);
        assert!(
            warnings.iter().any(|w| w.render("procedure.yaml")
                == "procedure.yaml:5:5: plugs[0].config: `config:` is the old spelling of `with:`; rename it"),
            "{warnings:?}"
        );
        // Both spellings at once.
        let both = format!("name: A\nplugs:\n  - name: P\n    python: plugs.p:P\n    with: {{a: 1}}\n    config: {{a: 2}}\n{MAIN}");
        assert!(has(&errors(&both), "`with:` and `config:` both set"));
    }

    #[test]
    fn the_slot_aliases_are_deprecated_where_markers_resolve() {
        let yaml = format!(
            "name: A\nunit:\n  serial_number:\n    default_value: \"SN-{{slot}}\"\n{MAIN}    with:\n      nest: \"{{slot_name}}\"\n"
        );
        let outcome = inspect_procedure_definition(&yaml);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let details: Vec<&str> = outcome.warnings.iter().map(|w| w.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "unit.serial_number.default_value: `{slot}` is the old spelling of `{run.slot.key}`; write `{run.slot.key}`",
                "main[0].with.nest: `{slot_name}` is the old spelling of `{run.slot.name}`; write `{run.slot.name}`",
            ]
        );
    }

    #[test]
    fn a_secret_in_env_is_a_warning() {
        let yaml =
            format!("name: A\nsecrets:\n  T: {{}}\nenv:\n  TOKEN: \"{{secrets.T}}\"\n{MAIN}");
        let outcome = inspect_procedure_definition(&yaml);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(outcome.warnings.iter().any(|w| w
            .detail
            .contains("puts a secret in the environment")
            && !w
                .message
                .contains(crate::procedure::yaml::DEPRECATION_NOTICE)));
    }
}
