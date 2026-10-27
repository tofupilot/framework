use super::codes;
use super::error::CommandError;
use super::expand::{FsSequenceLocator, SequenceLocator};
use crate::procedure::schema::{
    ProcedureDefinition, ProcedureYaml, RefList, RefLocation, StageScope,
};
use std::collections::HashSet;
use std::path::Path;
use validator::Validate;

fn validate_file_path(path: &Path) -> Result<(), CommandError> {
    if !path.exists() {
        return Err(CommandError::file_not_found(display_path(path)));
    }

    // Case-insensitive: the CLI's yaml-hint check accepts `.YML`/`.YAML`
    // and routes the file here, so the loader must agree.
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("yaml" | "yml")) {
        return Err(CommandError::new(
            super::error::ErrorCode::InvalidFileExtension,
            format!("{}: expected .yaml or .yml", display_path(path)),
        ));
    }

    Ok(())
}

/// The procedure file as a console line names it: relative to the current
/// directory when it is under it (`procedure.yaml`), as given otherwise.
pub fn display_path(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .filter(|rel| !rel.as_os_str().is_empty())
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

/// Every refusal, one compiler-style line each:
/// `procedure.yaml:12:5: main[2].depends_on[0]: unknown phase `a1``.
#[must_use = "procedure definition should be checked for validation errors"]
pub fn load_procedure_definition(file_path: &Path) -> Result<ProcedureDefinition, String> {
    load_procedure_definition_with(file_path, &FsSequenceLocator)
}

/// `load_procedure_definition` with the locator that resolves `uses:`
/// references: the CLI passes one that knows the procedure's Python
/// environment, so `<package>:<path>` references resolve.
#[must_use = "procedure definition should be checked for validation errors"]
pub fn load_procedure_definition_with(
    file_path: &Path,
    locator: &dyn SequenceLocator,
) -> Result<ProcedureDefinition, String> {
    load_procedure_definition_rooted(file_path, None, locator)
}

/// `load_procedure_definition_with` for a file whose Python references
/// resolve against `python_root` rather than its own directory (a nested
/// manifest `entry_point`). `None` keeps the file's directory.
pub fn load_procedure_definition_rooted(
    file_path: &Path,
    python_root: Option<&Path>,
    locator: &dyn SequenceLocator,
) -> Result<ProcedureDefinition, String> {
    validate_file_path(file_path).map_err(|e| e.message)?;
    let file = display_path(file_path);
    let content =
        std::fs::read_to_string(file_path).map_err(|e| format!("{file}: cannot read: {e}"))?;

    let base_dir = file_path.parent().map(Path::to_path_buf);
    let outcome = inspect_procedure_definition_rooted(
        &content,
        base_dir.as_deref(),
        python_root.or(base_dir.as_deref()),
        locator,
    );
    match (outcome.definition, outcome.errors.is_empty()) {
        (Some(def), true) => Ok(def),
        (_, _) => Err(outcome
            .errors
            .iter()
            .map(|e| e.render(&file))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

/// The file's own `version:`, read tolerantly like `procedure_name_from_str`:
/// a partial parse, so a procedure that does not fully load (a `uses:` of
/// the `<package>:<path>` form with no interpreter at hand, a file mid-edit)
/// still tells its version. `None` when absent, empty or unparsable.
pub fn read_procedure_version(file_path: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct JustTheVersion {
        #[serde(default)]
        version: Option<String>,
    }
    let content = std::fs::read_to_string(file_path).ok()?;
    let parsed: JustTheVersion = super::yaml::from_str(&content).ok()?;
    let trimmed = parsed.version?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Names of the plugs whose entry states an explicit `scope:` line.
///
/// The loaded definition cannot answer this, by design:
/// `From<PlugDefinitionYaml>` applies the `slot` default on the way in
/// and `PlugDefinition::to_yaml` erases an explicit `slot` on the way
/// out, so an author who wrote the default is indistinguishable from one
/// who omitted it — everywhere except the text. Studio needs the
/// difference to draw an inherited scope as inherited rather than as a
/// choice someone made.
///
/// Tolerant in the same way, and for the same reason, as
/// `procedure_name_from_str`: `scope` is read as an opaque value so an
/// invalid one still counts as stated, and every field is optional so a procedure being
/// edited still answers. Unparsable text yields an empty set, which
/// renders as "nothing stated" — the safe direction.
pub fn plugs_with_explicit_scope(content: &str) -> HashSet<String> {
    #[derive(serde::Deserialize)]
    struct JustPlugs {
        #[serde(default)]
        plugs: Vec<JustScope>,
    }
    #[derive(serde::Deserialize)]
    struct JustScope {
        #[serde(default)]
        name: String,
        #[serde(default)]
        scope: Option<serde::de::IgnoredAny>,
    }

    super::yaml::from_str::<JustPlugs>(content)
        .map(|parsed| {
            parsed
                .plugs
                .into_iter()
                .filter(|p| p.scope.is_some())
                // Trimmed to match `PlugDefinitionYaml.name`, which
                // deserializes through `serde_trim` — an untrimmed key
                // here would never match the projection's plug name.
                .map(|p| p.name.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The definition's own `name:`, read tolerantly: a partial parse that
/// only needs the `name` key, NOT the whole definition to be valid. A
/// procedure mid-edit (broken phases, bad refs) still needs a display
/// label so it can be listed, selected and fixed. `None` when the text
/// is unparsable or the name is empty. The one name-reading mechanism
/// for every consumer — Studio discovery and the hello frame both call
/// it, so "the procedure's display name" cannot drift between them.
pub fn procedure_name_from_str(content: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct JustTheName {
        name: String,
    }
    let parsed: JustTheName = super::yaml::from_str(content).ok()?;
    let trimmed = parsed.name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `procedure_name_from_str` over a file. Sync (small files, called
/// from startup paths); async callers read the file themselves and use
/// the `_from_str` form.
pub fn read_procedure_name(file_path: &Path) -> Option<String> {
    procedure_name_from_str(&std::fs::read_to_string(file_path).ok()?)
}

/// Same parse + validation chain as `load_procedure_definition`, on
/// content that is not (or not yet) on disk. Validation is purely
/// structural — nothing after the file read touches the filesystem —
/// so proposed content can be checked before it is written. A text with
/// sequence calls needs `inspect_procedure_definition_at`.
#[must_use = "procedure definition should be checked for validation errors"]
pub fn load_procedure_definition_from_str(content: &str) -> Result<ProcedureDefinition, String> {
    load_procedure_definition_from_str_located(content).map_err(join_messages)
}

fn stage_name(stage: StageScope) -> &'static str {
    match stage {
        StageScope::SetupAll => "shared setup",
        StageScope::SetupEach => "setup",
        StageScope::Main => "main",
        StageScope::TeardownEach => "teardown",
        StageScope::TeardownAll => "shared teardown",
    }
}

/// The one-string form of a refusal: every collected rule failure, one
/// per line (`path: message`), so a caller printing the error shows all
/// of them.
fn join_messages(errors: Vec<LoadError>) -> String {
    errors
        .iter()
        .map(LoadError::detail)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A refusal to load, with where it points when the loader knows: the
/// YAML position of a parse error, or the entry a post-parse rule refused
/// (a duplicate key, a bad `depends_on`). Rules that judge the whole file
/// carry neither. `message` alone is what `load_procedure_definition`
/// callers see; Studio uses the rest to underline the right line.
///
/// A parse or schema failure is returned alone — nothing after it can be
/// judged. The post-parse rules are all evaluated and returned together,
/// so fixing one does not reveal the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    /// What is wrong, without path or position.
    pub message: String,
    /// 1-based position: the parser's own for a parse error, the refused
    /// node's (or its closest written ancestor's) otherwise.
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub location: Option<RefLocation>,
    /// YAML path of the refused node: `main[2].measurements[0].validators[0]`.
    pub path: Option<String>,
    /// Stable key of the rule (`codes::DUPLICATE_KEY`…).
    pub code: &'static str,
    parse: bool,
}

impl LoadError {
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line: None,
            column: None,
            location: None,
            path: None,
            code: codes::INVALID_PROCEDURE,
            parse: false,
        }
    }

    pub(crate) fn with_code(self, code: &'static str) -> Self {
        Self { code, ..self }
    }
    pub fn at(message: impl Into<String>, location: RefLocation) -> Self {
        Self {
            path: Some(location_path(&location)),
            location: Some(location),
            ..Self::text(message)
        }
    }
    /// A refusal of another file's content (a sequence file), reported at
    /// the procedure entry that reads it. Its own position is in `message`:
    /// no path, so no line of the procedure file is taken for it.
    pub fn at_call(message: impl Into<String>, location: Option<RefLocation>) -> Self {
        Self {
            location,
            ..Self::text(message)
        }
    }
    /// A rule failure whose text starts with the YAML path it is about
    /// (`unit.serial_number: ...`): the path moves to `path`.
    pub(crate) fn rule(text: impl Into<String>) -> Self {
        let text = text.into();
        match super::yaml::split_path(&text) {
            Some((path, message)) => Self {
                path: Some(path.to_string()),
                ..Self::text(message)
            },
            None => Self::text(text),
        }
    }
    pub(crate) fn on(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            ..Self::text(message)
        }
    }

    /// The text failed to parse: nothing after this error was judged.
    pub fn is_parse_error(&self) -> bool {
        self.parse
    }

    /// `path: message`, or the message alone at the root.
    pub fn detail(&self) -> String {
        match &self.path {
            Some(path) => format!("{path}: {}", self.message),
            None => self.message.clone(),
        }
    }

    /// Compiler-style line: `procedure.yaml:12:5: main[2].depends_on: ...`,
    /// leaving out what is unknown.
    pub fn render(&self, file: &str) -> String {
        let at = match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{file}:{line}:{column}"),
            (Some(line), None) => format!("{file}:{line}"),
            _ => file.to_string(),
        };
        format!("{at}: {}", self.detail())
    }
}

impl From<String> for LoadError {
    fn from(message: String) -> Self {
        Self::text(message)
    }
}

/// `main[2].depends_on` for `RefLocation::key(Main, 2, "depends_on")`.
pub fn location_path(location: &RefLocation) -> String {
    if location.list == crate::procedure::schema::RefList::Root {
        return location.key.clone().unwrap_or_default();
    }
    let entry = format!("{}[{}]", location.list.as_str(), location.index);
    match &location.key {
        Some(key) => format!("{entry}.{key}"),
        None => entry,
    }
}

/// `load_procedure_definition_from_str`, keeping positions. Refuses the
/// definition when any rule failed — the runner's contract.
pub fn load_procedure_definition_from_str_located(
    content: &str,
) -> Result<ProcedureDefinition, Vec<LoadError>> {
    let outcome = inspect_procedure_definition(content);
    match (outcome.definition, outcome.errors.is_empty()) {
        (Some(def), true) => Ok(def),
        (_, _) => Err(outcome.errors),
    }
}

/// Everything the loader can say about a procedure text: the definition
/// whenever the text PARSED, and every schema or rule failure found on it.
///
/// Loading is a refusal ("can the runner execute this?"); inspecting is
/// not. A duplicate phase key makes a procedure unrunnable, but the
/// struct behind it is fully built and safe to look at — so Studio lints
/// it and shows the duplicate next to the dangling `python:` three lines
/// below, instead of revealing one problem per fix. Only a parse failure
/// leaves nothing to inspect.
pub struct LoadOutcome {
    pub definition: Option<ProcedureDefinition>,
    pub errors: Vec<LoadError>,
    /// What loads today but a future version will refuse. Never blocks a
    /// load.
    pub warnings: Vec<super::yaml::Warning>,
}

/// `inspect_procedure_definition` over a file; an unreadable file is a
/// single error with no definition.
pub fn inspect_procedure_definition_file(file_path: &Path) -> LoadOutcome {
    inspect_procedure_definition_file_with(file_path, &FsSequenceLocator)
}

/// `inspect_procedure_definition_file` with the locator for `uses:`.
pub fn inspect_procedure_definition_file_with(
    file_path: &Path,
    locator: &dyn SequenceLocator,
) -> LoadOutcome {
    match std::fs::read_to_string(file_path) {
        Ok(content) => inspect_procedure_definition_at(&content, file_path.parent(), locator),
        Err(e) => LoadOutcome {
            definition: None,
            errors: vec![LoadError::text(format!(
                "{}: cannot read: {e}",
                display_path(file_path)
            ))],
            warnings: Vec::new(),
        },
    }
}

/// The text-only inspection. A file with sequence calls (`uses:`) cannot
/// be expanded without knowing where it lives: this reports that as the
/// one error. Callers that know the file's directory use
/// `inspect_procedure_definition_at`.
pub fn inspect_procedure_definition(content: &str) -> LoadOutcome {
    inspect_procedure_definition_at(content, None, &FsSequenceLocator)
}

/// Everything the loader can say about a procedure text whose file lives
/// in `base_dir` (its `uses:` references resolve from there). `None`
/// when the text has no location: fine for a file without sequence
/// calls, an error for one with them.
pub fn inspect_procedure_definition_at(
    content: &str,
    base_dir: Option<&Path>,
    locator: &dyn SequenceLocator,
) -> LoadOutcome {
    inspect_procedure_definition_rooted(content, base_dir, base_dir, locator)
}

/// `inspect_procedure_definition_at` with the directory the sequence
/// phases' `python:` specs are rebased against, when it is not the file's
/// own (a nested manifest `entry_point` resolves Python from the package
/// directory). See `expand_procedure_rooted`.
pub fn inspect_procedure_definition_rooted(
    content: &str,
    base_dir: Option<&Path>,
    python_root: Option<&Path>,
    locator: &dyn SequenceLocator,
) -> LoadOutcome {
    let (raw, warnings, positions): (ProcedureYaml, _, _) =
        match super::yaml::from_str_located(content) {
            Ok(parsed) => parsed,
            Err(e) => {
                // Written for CLI 1.9.0 to 1.9.2, which asked for the block.
                let (message, path) = if e.message().starts_with("unknown field `dashboard`") {
                    (
                        "no longer supported; remove the block".to_string(),
                        Some("dashboard".to_string()),
                    )
                } else if let Some(renamed) = super::values::renamed_top_level_block(&e) {
                    (renamed.to_string(), Some("vars".to_string()))
                } else {
                    (shaped(&e), e.path().map(str::to_string))
                };
                let mut errors = vec![LoadError {
                    message,
                    line: e.line(),
                    column: e.column(),
                    location: None,
                    path,
                    code: e.code(),
                    parse: true,
                }];
                errors.extend(further_unknown_fields(content, &e));
                return LoadOutcome {
                    definition: None,
                    errors,
                    warnings: Vec::new(),
                };
            }
        };
    // Where a marker sits and the old spellings: read off the text, which
    // knows the YAML path of every value.
    let text_pass =
        super::value_checks::text_diagnostics(content, super::value_checks::FileKind::Procedure);
    let mut warnings = warnings;
    warnings.extend(text_pass.warnings);
    let mut outcome = inspect_parsed(raw, warnings, base_dir, python_root, locator);
    outcome
        .errors
        .extend(text_pass.errors.into_iter().map(|e| LoadError {
            line: Some(e.line),
            column: Some(e.column),
            path: Some(e.path),
            ..LoadError::text(e.message)
        }));
    for error in &mut outcome.errors {
        if error.line.is_none() {
            if let Some((line, column)) = error.path.as_deref().and_then(|p| positions.locate(p)) {
                error.line = Some(line);
                error.column = Some(column);
            }
        }
    }
    outcome
}

/// "expected a mapping" says what is wrong, not what to write: for the
/// mappings people most often write as a bare value, the shape too.
fn shaped(e: &super::yaml::Error) -> String {
    let message = e.message();
    if message != "expected a mapping" {
        return message.to_string();
    }
    let field = e.path().and_then(|p| p.rsplit('.').next()).unwrap_or("");
    match field {
        "retry" => {
            format!("{message}: write `retry: {{limit: 2}}` (retries after the first attempt)")
        }
        "then" => format!("{message}: write `then: {{fail: stop}}`"),
        _ => message.to_string(),
    }
}

/// How many unknown fields one look reports: past that, the file is not
/// the procedure format at all.
const MAX_UNKNOWN_FIELDS: usize = 20;

/// The parser stops at the first unknown field. The others, so a file with
/// several misspellings is fixed in one edit: each unknown key is cut from
/// a copy of the text (its line and the lines indented under it) and the
/// copy parsed again, positions mapped back to the file's own lines. Stops
/// at the first refusal of another kind, which is reported too, or at a
/// key it cannot cut alone (one opening a list entry, or inside a flow
/// mapping).
fn further_unknown_fields(content: &str, first: &super::yaml::Error) -> Vec<LoadError> {
    let mut out = Vec::new();
    let mut lines: Vec<&str> = content.lines().collect();
    // The file's own line number of each line still in `lines`.
    let mut original: Vec<u32> = (1..=lines.len() as u32).collect();
    let mut error_line = first.line();
    let mut error_column = first.column();
    let mut code = first.code();
    while code == codes::UNKNOWN_FIELD && out.len() < MAX_UNKNOWN_FIELDS {
        let (Some(line), Some(column)) = (error_line, error_column) else {
            break;
        };
        let at = original.iter().position(|&l| l == line);
        let Some(at) = at else { break };
        let text = lines[at];
        let indent = text.len() - text.trim_start().len();
        // The key must open its line alone: under a `- ` or inside a flow
        // mapping, cutting the line would cut its siblings too.
        if indent + 1 != column as usize {
            break;
        }
        let key_indent = column as usize - 1;
        let mut end = at + 1;
        while end < lines.len() {
            let next = lines[end];
            let next_indent = next.len() - next.trim_start().len();
            if !next.trim().is_empty() && next_indent <= key_indent {
                break;
            }
            end += 1;
        }
        lines.drain(at..end);
        original.drain(at..end);
        let stripped = lines.join("\n");
        match super::yaml::from_str_located::<ProcedureYaml>(&stripped) {
            Ok(_) => break,
            Err(e) => {
                code = e.code();
                error_line = e.line().and_then(|l| original.get(l as usize - 1).copied());
                error_column = e.column();
                out.push(LoadError {
                    message: e.message().to_string(),
                    line: error_line,
                    column: error_column,
                    location: None,
                    path: e.path().map(str::to_string),
                    code,
                    parse: true,
                });
                if code != codes::UNKNOWN_FIELD {
                    break;
                }
            }
        }
    }
    out
}

/// Every `#[validate]` failure of the definition, one error per field, in
/// path order: `main[0].key: invalid `1x`: expected a Python identifier`.
fn validation_errors(errors: &validator::ValidationErrors) -> Vec<LoadError> {
    fn walk(errors: &validator::ValidationErrors, prefix: &str, out: &mut Vec<LoadError>) {
        let mut fields: Vec<_> = errors.errors().iter().collect();
        fields.sort_by(|a, b| a.0.cmp(b.0));
        for (field, kind) in fields {
            let path = if field == "__all__" {
                prefix.to_string()
            } else if prefix.is_empty() {
                field.to_string()
            } else {
                format!("{prefix}.{field}")
            };
            match kind {
                validator::ValidationErrorsKind::Struct(inner) => walk(inner, &path, out),
                validator::ValidationErrorsKind::List(items) => {
                    for (index, inner) in items {
                        walk(inner, &format!("{path}[{index}]"), out);
                    }
                }
                validator::ValidationErrorsKind::Field(failures) => {
                    for failure in failures {
                        out.push(LoadError::on(path.clone(), failure_message(field, failure)));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(errors, "", &mut out);
    out
}

/// One `#[validate]` failure in words. Never the crate's `Display`, which
/// prints its parameters as Rust `Debug`.
fn failure_message(field: &str, failure: &validator::ValidationError) -> String {
    if let Some(message) = &failure.message {
        return message.to_string();
    }
    let param = |name: &str| failure.params.get(name);
    let number = |name: &str| param(name).and_then(serde_json::Value::as_f64);
    // Durations are stored in milliseconds and written as durations.
    let shown = |n: f64| {
        if matches!(field, "timeout" | "init_timeout" | "delay") {
            super::schema::format_duration(n as u64)
        } else {
            format!("{n}")
        }
    };
    let bounds = |unit: &str| match (number("min"), number("max"), number("equal")) {
        (_, _, Some(eq)) => format!("expected {}{unit}", shown(eq)),
        (Some(min), Some(max), _) => format!("expected {}-{}{unit}", shown(min), shown(max)),
        (Some(min), None, _) => format!("expected at least {}{unit}", shown(min)),
        (None, Some(max), _) => format!("expected at most {}{unit}", shown(max)),
        (None, None, _) => "out of range".to_string(),
    };
    match failure.code.as_ref() {
        "length" => {
            let (unit, got) = match param("value") {
                Some(serde_json::Value::String(s)) => (" characters", s.chars().count()),
                Some(serde_json::Value::Array(items)) => (" items", items.len()),
                _ => ("", 0),
            };
            format!("{}, got {got}", bounds(unit))
        }
        "range" => match number("value") {
            Some(value) => format!("{}, got {}", bounds(""), shown(value)),
            None => bounds(""),
        },
        "invalid_python_identifier" | "invalid_phase_key" => {
            match param("value").and_then(|v| v.as_str()) {
                Some(value) => format!("invalid `{value}`: expected a Python identifier"),
                None => "expected a Python identifier".to_string(),
            }
        }
        code => code.replace('_', " "),
    }
}

fn inspect_parsed(
    raw: ProcedureYaml,
    warnings: Vec<super::yaml::Warning>,
    base_dir: Option<&Path>,
    python_root: Option<&Path>,
    locator: &dyn SequenceLocator,
) -> LoadOutcome {
    let procedure_def = if raw.has_sequence_calls() {
        let Some(dir) = base_dir else {
            let refs: Vec<String> = raw
                .setup
                .iter()
                .chain(raw.main.iter())
                .chain(raw.teardown.iter())
                .filter_map(|e| e.as_call().map(|c| c.uses.clone()))
                .collect();
            return LoadOutcome {
                definition: None,
                errors: vec![LoadError::text(format!(
                    "calls sequences ({}); needs its file location to resolve them",
                    refs.join(", ")
                ))],
                warnings,
            };
        };
        match super::expand::expand_procedure_rooted(raw, dir, python_root.unwrap_or(dir), locator)
        {
            Ok(def) => def,
            Err(errors) => {
                return LoadOutcome {
                    definition: None,
                    errors,
                    warnings,
                }
            }
        }
    } else {
        match ProcedureDefinition::try_from(raw) {
            Ok(def) => def,
            Err(e) => {
                return LoadOutcome {
                    definition: None,
                    errors: vec![LoadError::rule(e.to_string())],
                    warnings,
                }
            }
        }
    };

    // Every check below is evaluated; the failures are returned together.
    let mut errors: Vec<LoadError> = Vec::new();

    if let Err(e) = procedure_def.validate() {
        errors.extend(validation_errors(&e));
    }

    if let Some(unit) = &procedure_def.unit {
        if let Err(e) = unit.validate_auto_identify() {
            errors.push(LoadError::rule(e));
        }

        if let Err(e) = unit.validate_affixes() {
            errors.push(LoadError::rule(e));
        }

        if let Err(e) = unit.validate_defaults() {
            errors.push(LoadError::rule(e));
        }

        let slots = procedure_def
            .execution
            .as_ref()
            .map(|e| e.slots.as_slice())
            .unwrap_or(&[]);
        if let Err(e) = unit.validate_uploaded_defaults(slots) {
            errors.push(LoadError::rule(e));
        }

        if let Some(md) = &unit.metadata {
            if let Err(e) = crate::procedure::schema::validate_metadata_keys(
                md.keys().map(|k| k.as_str()),
                "unit.metadata",
            ) {
                errors.push(LoadError::rule(e));
            }
        }
    }

    // `operated_by` is free text server-side (no charset), but its
    // affixes still carry the length bound.
    if let Some(operated_by) = &procedure_def.operated_by {
        if let Err(e) =
            crate::procedure::schema::validate_unit_field_affixes("operated_by", operated_by, false)
        {
            errors.push(LoadError::rule(e));
        }
        if let Err(e) = crate::procedure::schema::validate_field_default("operated_by", operated_by)
        {
            errors.push(LoadError::rule(e));
        }
    }

    for (list, phases) in [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ] {
        for (index, phase) in phases.iter().enumerate() {
            if let Err(e) = phase.validate_single_runtime() {
                errors.push(LoadError::at(e, RefLocation::entry(list, index)));
            }
            if let Some(ui) = &phase.ui {
                if let Some(components) = &ui.components {
                    for (c, comp) in components.iter().enumerate() {
                        for check in [
                            comp.validate_width(),
                            comp.validate_aspect(),
                            comp.validate_fit(),
                            comp.validate_options_count(),
                            comp.validate_identity_affixes(),
                        ] {
                            // `width: ...` under this component's path.
                            if let Err(e) = check {
                                errors.push(LoadError::rule(format!(
                                    "{}[{index}].ui.components[{c}].{e}",
                                    list.as_str()
                                )));
                            }
                        }

                        // A choice component without options is not a load error:
                        // a Python phase may fill the list at run time with
                        // `ui.set(key, options=[...])` (TP-1246), and the screen
                        // shows "Waiting for options" until then. Whether anything
                        // fills it is `resolve_runtime_refs`' job: a warning when
                        // the phase's Python never names it, an error when no
                        // Python phase exists and the choice is required (`run`
                        // stops on those, Studio only reports them, so the editor
                        // keeps loading while the author fixes the component).
                    }
                }
            }
        }
    }

    // `scope: station` is a plug lifetime, not a phase grouping — a phase
    // can't run "across runs". Phases silently coerce non-`all` scopes to
    // per-slot (`iter_phases_with_stage`), so without this check a
    // station-scoped phase would quietly behave as `slot`. Catch it at
    // load with an error that points at the authoring bug.
    for (list, phases) in [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ] {
        for (index, phase) in phases.iter().enumerate() {
            if phase.scope == Some(crate::procedure::schema::Scope::Station) {
                errors.push(LoadError::at(
                    "`station` only valid on plugs; use `execution` or `slot`",
                    RefLocation::key(list, index, "scope"),
                ));
            }
        }
    }

    // A procedure with no main phases isn't a test — the runner would exit
    // PASS without doing anything, which is misleading.
    if procedure_def.main.is_empty() {
        errors.push(LoadError::on("main", "empty; add at least 1 phase"));
    }

    // A written phase key is a Python identifier of 1 to 100 characters.
    // The expanded type accepts the `/` path form for the phases a call
    // brought in; a procedure's own phase must not spell one, or the TUI
    // and the dashboard read it as a phase of a call. The expansion checks
    // the phases of sequence files; this covers a procedure without calls,
    // which the expansion never sees. A key that is no identifier path at
    // all is already refused by the `#[validate]` pass above.
    for (list, phases) in [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ] {
        for (index, phase) in phases.iter().enumerate() {
            if !phase.sequence_path.is_empty()
                || !crate::procedure::schema::is_valid_phase_key_path(&phase.key)
            {
                continue;
            }
            if phase.key.contains('/') || phase.key.chars().count() > 100 {
                errors.push(LoadError::at(
                    format!(
                        "invalid `{}`: expected a Python identifier of 1 to 100 characters",
                        phase.key
                    ),
                    procedure_def.written_location(list, index, "key"),
                ));
            }
        }
    }

    // Phase keys must be unique across setup, main and teardown. Duplicates
    // corrupt the dependency graph, produce phantom duplicate phase events
    // at runtime because the scheduler indexes jobs by key, and make two
    // phases overwrite each other's results (`phase_results` is keyed by
    // phase key for the whole run).
    let mut seen_keys: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let stages = [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ];
    for (list, phases) in stages {
        for (index, phase) in phases.iter().enumerate() {
            match seen_keys.get(phase.key.as_str()) {
                Some(first) => errors.push(
                    LoadError::at(
                        format!("duplicate key {} (also {first})", phase.key),
                        procedure_def.written_location(list, index, "key"),
                    )
                    .with_code(codes::DUPLICATE_KEY),
                ),
                None => {
                    seen_keys.insert(phase.key.as_str(), format!("{}[{index}]", list.as_str()));
                }
            }
        }
    }

    // Plug keys must be unique too, and case-insensitively so: the worker
    // matches a phase parameter to a plug with `param_name.lower() ==
    // plug_key.lower()` (tp_worker.py), so `DMM` and `dmm` are one binding
    // as far as a phase is concerned. Duplicates have no failure mode a
    // user can read: the scope map keeps the LAST entry
    // (`orchestrator/initialization.rs`) while the config lookup finds the
    // FIRST (`orchestrator/plugs.rs`), so two declared instruments quietly
    // become one process built from one entry and torn down by the other's
    // lifetime. Keys are usually derived from `name`, which makes this
    // easy to hit without noticing — "Power Supply" and "Power-Supply" are
    // two distinct names that derive the same key.
    let mut seen_plug_keys: std::collections::HashMap<String, (usize, &str)> =
        std::collections::HashMap::new();
    for (index, plug) in procedure_def.plugs.iter().enumerate() {
        match seen_plug_keys.get(&plug.key.to_lowercase()) {
            Some((first, name)) => errors.push(
                LoadError::at(
                    format!(
                        "key {} duplicates plugs[{first}] ({name}); set `key:`",
                        plug.key
                    ),
                    RefLocation::entry(RefList::Plugs, index),
                )
                .with_code(codes::DUPLICATE_KEY),
            ),
            None => {
                seen_plug_keys.insert(plug.key.to_lowercase(), (index, plug.name.as_str()));
            }
        }
    }

    // Slot keys too: the orchestrator keys a slot's unit, plugs and results
    // by it, so two slots sharing one test one unit under two names. Keys
    // are derived from `name` when omitted, and `Bay 1` and `Bay-1` both
    // derive `bay_1`. Case-insensitively, like plug keys: `BAY_1` and
    // `bay_1` are one slot to whoever reads the run.
    let slots = procedure_def
        .execution
        .as_ref()
        .map(|e| e.slots.as_slice())
        .unwrap_or(&[]);
    let mut seen_slot_keys: std::collections::HashMap<String, (usize, &str)> =
        std::collections::HashMap::new();
    for (index, slot) in slots.iter().enumerate() {
        match seen_slot_keys.get(&slot.key.to_lowercase()) {
            Some((first, name)) => errors.push(
                LoadError::on(
                    format!("execution.slots[{index}]"),
                    format!(
                        "key {} duplicates slots[{first}] ({name}); each slot needs its own `key:` (derived from `name:` when omitted)",
                        slot.key
                    ),
                )
                .with_code(codes::DUPLICATE_KEY),
            ),
            None => {
                seen_slot_keys.insert(slot.key.to_lowercase(), (index, slot.name.as_str()));
            }
        }
    }

    // `dashboard` is the parameter the worker fills in with the API
    // client, resolved before plug keys and phase results. A plug or
    // phase keyed `dashboard` would be unreachable from every phase,
    // and a plug `config:` entry of that name would be overridden at
    // construction. The older built-ins (`log`, `unit`, ...) shadow the
    // same way, but procedures keyed like them exist and load today, so
    // only the name this block introduces is refused.
    for (index, plug) in procedure_def.plugs.iter().enumerate() {
        if plug.key.eq_ignore_ascii_case("dashboard") {
            errors.push(LoadError::at(
                "`dashboard` reserved for the TofuPilot client; set another `key:`",
                RefLocation::key(RefList::Plugs, index, "key"),
            ));
        }
        if plug
            .with
            .as_ref()
            .is_some_and(|c| c.contains_key("dashboard"))
        {
            errors.push(LoadError::at(
                "reserved, filled in by the runtime; remove it",
                RefLocation::key(
                    RefList::Plugs,
                    index,
                    format!("{}.dashboard", plug.with_key()),
                ),
            ));
        }
    }
    for (list, phases) in [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ] {
        for (index, phase) in phases.iter().enumerate() {
            // A phase brought in by a sequence call carries the call's prefix
            // and is checked by the expansion, at its call.
            if phase.sequence_path.is_empty() && phase.key.eq_ignore_ascii_case("dashboard") {
                errors.push(LoadError::at(
                    "`dashboard` reserved for the TofuPilot client; set another `key:`",
                    procedure_def.written_location(list, index, "key"),
                ));
            }
        }
    }

    // `depends_on` must reference phase keys that exist in the procedure,
    // in any stage. Silently ignoring unknown dependencies lets a typo mask
    // broken ordering. A dependency on a later stage can never be met; one
    // on an earlier stage is redundant and accepted. A dependency naming a
    // sequence call was already resolved to the call's terminal phases by
    // the expansion, so the keys here are phase keys only.
    let stage_of: std::collections::HashMap<&str, StageScope> = procedure_def
        .get_all_phases_with_stage_scope()
        .into_iter()
        .map(|(stage, p)| (p.key.as_str(), stage))
        .collect();
    let mut known_keys: Vec<&str> = stage_of.keys().copied().collect();
    known_keys.sort_unstable();
    let stages = [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ];
    for (list, phases) in stages {
        for (index, phase) in phases.iter().enumerate() {
            let Some(&stage) = stage_of.get(phase.key.as_str()) else {
                continue;
            };
            for (n, dep) in phase.depends_on.iter().enumerate() {
                // The entry itself, under its key: the caret lands on the
                // name to fix.
                let entry = format!("depends_on[{n}]");
                let at = || procedure_def.written_location(list, index, &entry);
                match stage_of.get(dep.as_str()) {
                    None => errors.push(
                        LoadError::at(
                            match super::pysource::closest(dep, known_keys.iter().copied())
                                .filter(|near| near.len().max(dep.len()) > 2)
                            {
                                Some(near) => {
                                    format!("unknown phase `{dep}`; did you mean `{near}`?")
                                }
                                None => format!(
                                    "unknown phase `{dep}` (phases: {})",
                                    known_keys.join(", ")
                                ),
                            },
                            at(),
                        )
                        .with_code(codes::UNKNOWN_DEPENDENCY),
                    ),
                    Some(dep_stage) if dep_stage.rank() > stage.rank() => {
                        errors.push(LoadError::at(
                            format!(
                                "`{dep}` runs later ({}); a {} phase cannot wait for it",
                                stage_name(*dep_stage),
                                stage_name(stage)
                            ),
                            at(),
                        ))
                    }
                    Some(_) => {}
                }
            }
        }
    }

    // Belt: explicit self-reference pre-check. Three-color DFS below
    // catches this too, but the error text is clearer when handled
    // directly ("depends on itself" vs "a -> a").
    //
    // Per stage list: dependencies only point to the same or an earlier
    // stage (checked above), so a cycle can only close inside one list.
    for (list, phases) in [
        (RefList::Setup, &procedure_def.setup),
        (RefList::Main, &procedure_def.main),
        (RefList::Teardown, &procedure_def.teardown),
    ] {
        let mut self_dependent = false;
        for (index, phase) in phases.iter().enumerate() {
            if phase.depends_on.iter().any(|d| d == &phase.key) {
                self_dependent = true;
                errors.push(LoadError::at(
                    format!("{} depends on itself", phase.key),
                    procedure_def.written_location(list, index, "depends_on"),
                ));
            }
        }

        // Braces: full cycle detection in the dependency graph. Skipped when a
        // self-reference was already reported, which the DFS would repeat as
        // `a -> a`.
        if !self_dependent {
            if let Some(cycle) = find_dependency_cycle(phases) {
                // Point at the first phase of the cycle as written in the file.
                let index = phases
                    .iter()
                    .position(|p| cycle.contains(&p.key))
                    .unwrap_or(0);
                errors.push(LoadError::at(
                    format!("cycle {}", cycle.join(" -> ")),
                    procedure_def.written_location(list, index, "depends_on"),
                ));
            }
        }
    }

    // `with:`, `env:`, `config:` and `secrets:`: declarations, keys, and the
    // run-start markers in the values kept for the run.
    errors.extend(super::value_checks::check_definition(&procedure_def));

    LoadOutcome {
        definition: Some(procedure_def),
        errors,
        warnings,
    }
}

/// DFS with three-color marking. Returns the cycle as an ordered list of
/// phase keys if one exists.
fn find_dependency_cycle(
    phases: &[crate::procedure::schema::PhaseDefinition],
) -> Option<Vec<String>> {
    use std::collections::HashMap;

    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let by_key: HashMap<&str, &crate::procedure::schema::PhaseDefinition> =
        phases.iter().map(|p| (p.key.as_str(), p)).collect();
    let mut color: HashMap<&str, Color> = phases
        .iter()
        .map(|p| (p.key.as_str(), Color::White))
        .collect();

    fn dfs<'a>(
        node: &'a str,
        by_key: &HashMap<&'a str, &'a crate::procedure::schema::PhaseDefinition>,
        color: &mut HashMap<&'a str, Color>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        color.insert(node, Color::Gray);
        stack.push(node);
        if let Some(phase) = by_key.get(node) {
            for dep in &phase.depends_on {
                let dep_s: &str = dep.as_str();
                match color.get(dep_s).copied().unwrap_or(Color::White) {
                    Color::White => {
                        if let Some(c) = dfs(dep_s, by_key, color, stack) {
                            return Some(c);
                        }
                    }
                    Color::Gray => {
                        // Cycle: rewind the stack to where the cycle starts.
                        let start = stack.iter().position(|n| *n == dep_s).unwrap_or(0);
                        let mut cycle: Vec<String> =
                            stack[start..].iter().map(|s| s.to_string()).collect();
                        cycle.push(dep_s.to_string());
                        return Some(cycle);
                    }
                    Color::Black => {}
                }
            }
        }
        stack.pop();
        color.insert(node, Color::Black);
        None
    }

    for phase in phases {
        if color
            .get(phase.key.as_str())
            .copied()
            .unwrap_or(Color::White)
            == Color::White
        {
            let mut stack = Vec::new();
            if let Some(cycle) = dfs(phase.key.as_str(), &by_key, &mut color, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the projection cannot make for itself: an
    /// omitted `scope:` and a stated one that happens to name the
    /// default are the same `PlugDefinition` after loading.
    #[test]
    fn explicit_scope_separates_stated_from_inherited() {
        let yaml = r#"
name: P
version: 1.0.0
plugs:
  - name: Inherited
    python: plugs.a:A
  - name: Stated Default
    scope: slot
    python: plugs.b:B
  - name: Stated Other
    scope: station
    python: plugs.c:C
"#;
        let explicit = plugs_with_explicit_scope(yaml);
        assert!(!explicit.contains("Inherited"));
        assert!(explicit.contains("Stated Default"));
        assert!(explicit.contains("Stated Other"));
    }

    /// Unparsable and plugless text answers "nothing stated" rather
    /// than failing: a procedure mid-edit still has to render.
    #[test]
    fn explicit_scope_tolerates_broken_and_plugless_text() {
        assert!(plugs_with_explicit_scope("{{{ not yaml").is_empty());
        assert!(plugs_with_explicit_scope("name: P\n").is_empty());
        // An invalid scope value is still a stated one — the page shows
        // the parse's spelling, and the parse is what rejects it.
        assert!(
            plugs_with_explicit_scope("plugs:\n  - name: Bad\n    scope: nonsense\n")
                .contains("Bad")
        );
    }

    /// Names are trimmed to match `PlugDefinitionYaml.name`, which
    /// deserializes through `serde_trim` — an untrimmed key would never
    /// match the plug it belongs to.
    #[test]
    fn explicit_scope_trims_names() {
        let yaml = "plugs:\n  - name: \"  Padded  \"\n    scope: execution\n";
        assert!(plugs_with_explicit_scope(yaml).contains("Padded"));
    }

    fn load_from_str(yaml: &str) -> Result<ProcedureDefinition, String> {
        let dir = std::env::temp_dir().join(format!("tp-loader-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(&path, yaml).unwrap();
        let result = load_procedure_definition(&path);
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    /// Public API on a shared crate: the CLI reads it to name a
    /// procedure, and Studio's project discovery labels every row in the
    /// switcher with it. A blank name has to read as "no name" so the
    /// caller falls back to the holding directory, rather than
    /// labelling the row with an empty string.
    #[test]
    fn procedure_name_from_str_reads_the_name_or_nothing() {
        assert_eq!(
            procedure_name_from_str("name: Board Test\nversion: 1.0.0\n").as_deref(),
            Some("Board Test")
        );
        // Trimmed, because the YAML carries whatever the author typed.
        assert_eq!(
            procedure_name_from_str("name: \"  Padded  \"\n").as_deref(),
            Some("Padded")
        );
        // Present but empty, and whitespace-only, are both "no name".
        assert_eq!(procedure_name_from_str("name: \"\"\n"), None);
        assert_eq!(procedure_name_from_str("name: \"   \"\n"), None);
        // No `name:` key at all, and not a mapping at all.
        assert_eq!(procedure_name_from_str("version: 1.0.0\n"), None);
        assert_eq!(procedure_name_from_str("- just\n- a list\n"), None);
        // Unparseable YAML answers None instead of panicking: this runs
        // over files the operator is in the middle of editing.
        assert_eq!(procedure_name_from_str("name: [unclosed\n"), None);
        assert_eq!(procedure_name_from_str(""), None);
    }

    #[test]
    fn version_may_be_omitted() {
        let result = load_from_str(
            r#"
name: No Version
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        );
        let def = result.expect("version is optional");
        assert_eq!(def.version, "");
    }

    #[test]
    fn blank_version_loads_as_empty() {
        let result = load_from_str(
            r#"
name: Blank Version
version: "   "
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        );
        let def = result.expect("a whitespace-only version trims to empty and still loads");
        assert_eq!(def.version, "");
    }

    #[test]
    fn version_over_50_chars_rejected() {
        let result = load_from_str(&format!(
            r#"
name: Long Version
version: "{}"
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
            "1".repeat(51)
        ));
        let err = result.expect_err("a version over 50 chars must still be rejected");
        assert!(
            err.contains("version: expected at most 50 characters, got 51"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn station_scope_on_plug_loads() {
        let result = load_from_str(
            r#"
name: Station Plug OK
version: "1.0"
plugs:
  - name: Power Supply
    python: instruments.psu:PowerSupply
    scope: station
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        );
        let def = result.expect("station scope on a plug is valid");
        assert!(def.plugs[0].scope_is_station());
    }

    /// An unlocked default is a pre-fill the operator overwrites: a hint
    /// with a space loaded on 1.9.0 and must keep loading.
    #[test]
    fn unlocked_identity_default_outside_the_server_charset_still_loads() {
        load_from_str(
            r#"
name: Scan Hint
version: "1.0"
unit:
  serial_number:
    default_value: "SCAN HERE"
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect("an unlocked hint is not uploaded as-is");
    }

    /// A locked or auto-identify default is uploaded as-is: refuse at load
    /// what `runs.create` would refuse after the test.
    #[test]
    fn locked_identity_default_outside_the_server_rules_is_refused() {
        let err = load_from_str(
            r#"
name: Locked Bad
version: "1.0"
unit:
  serial_number:
    default_value: "SN 1"
    locked: true
  part_number:
    default_value: "PCB"
    locked: true
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("a locked serial with a space would fail every upload");
        assert!(err.contains("serial_number"), "got: {err}");
    }

    #[test]
    fn slot_placeholder_expanding_outside_the_server_charset_is_refused_per_slot() {
        let err = load_from_str(
            r#"
name: Slot Names
version: "1.0"
execution:
  slots:
    - key: s1
      name: Nest-1
    - key: s2
      name: Nest 2
unit:
  auto_identify: true
  serial_number:
    default_value: "SMOKE-{slot}"
  part_number:
    default_value: "PCB"
  batch_number:
    default_value: "{slot_name}"
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("slot s2 gives batch_number 'Nest 2'");
        assert!(
            err.contains("batch_number") && err.contains("s2"),
            "got: {err}"
        );
        assert!(!err.contains("s1"), "slot s1 is fine: {err}");
    }

    /// The load check expanded `{slot_name}` then `{slot}`, the identify
    /// step `{slot}` then `{slot_name}`: a name holding `{slot}` was
    /// checked as `Nests1` and uploaded as `Nest{slot}`. Both now expand
    /// in one pass, so the check sees what is uploaded.
    #[test]
    fn slot_placeholders_expand_the_same_at_load_and_at_identify() {
        use crate::identify_unit::SlotRef;
        use crate::procedure::schema::expand_slot_placeholders;
        let value = "SN-{slot_name}-{slot}";
        assert_eq!(
            expand_slot_placeholders(value, "s1", "Nest{slot}"),
            "SN-Nest{slot}-s1"
        );
        assert_eq!(
            SlotRef::new("s1", Some("Nest{slot}")).expand(value),
            expand_slot_placeholders(value, "s1", "Nest{slot}")
        );
        assert_eq!(expand_slot_placeholders("{slot", "s1", "n"), "{slot");

        let err = load_from_str(
            r#"
name: Slot Names
execution:
  slots:
    - key: s1
      name: "Nest{slot}"
unit:
  auto_identify: true
  serial_number:
    default_value: "{slot_name}"
  part_number:
    default_value: "PCB"
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("identify uploads 'Nest{slot}', which the server refuses");
        assert!(err.contains("Nest{slot}"), "got: {err}");
    }

    #[test]
    fn every_unknown_field_is_reported_at_once() {
        let text = "name: T\nversoin: \"1\"\nmain:\n  - key: a\n    name: A\n    python: phases/a.py:a\n    retires:\n      limit: 2\n  - key: b\n    name: B\n    python: phases/b.py:b\n    timout: 3\n";
        let outcome = inspect_procedure_definition_at(text, None, &FsSequenceLocator);
        let found: Vec<(Option<u32>, &str)> =
            outcome.errors.iter().map(|e| (e.line, e.code)).collect();
        assert_eq!(
            found,
            [
                (Some(2), codes::UNKNOWN_FIELD),
                (Some(7), codes::UNKNOWN_FIELD),
                (Some(12), codes::UNKNOWN_FIELD)
            ],
            "{:?}",
            outcome.errors
        );
    }

    #[test]
    fn duplicate_phase_key_across_stages_rejected() {
        // Phase results are keyed by phase key across the whole run, so a
        // setup phase and a main phase sharing one would overwrite each
        // other's results.
        let err = load_from_str(
            r#"
name: Same Key Twice
version: "1.0"
setup:
  - key: power
    name: Power on
    python: phases.power_on
main:
  - key: power
    name: Power check
    python: phases.power_check
"#,
        )
        .expect_err("a key reused between setup and main must be rejected");
        assert!(
            err.contains("main[0].key: duplicate key power (also setup[0])"),
            "unexpected error: {err}"
        );
    }

    /// Two names that derive the same key are the realistic way to hit
    /// this — neither entry looks wrong on its own.
    #[test]
    fn duplicate_plug_key_rejected() {
        let err = load_from_str(
            r#"
name: Two Plugs One Key
version: "1.0"
plugs:
  - name: Power Supply
    python: instruments.psu:PowerSupply
  - name: Power-Supply
    python: instruments.psu2:PowerSupply
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("two plugs deriving `power_supply` must be rejected");
        assert!(
            err.contains(
                "plugs[1]: key power_supply duplicates plugs[0] (Power Supply); set `key:`"
            ),
            "unexpected error: {err}"
        );
    }

    fn slots_yaml(slots: &str) -> String {
        format!(
            "name: Slots\nexecution:\n  slots:\n{slots}main:\n  - key: p1\n    name: P1\n    python: phases.p1\n"
        )
    }

    /// Two slots that derive one key would test one unit under two names.
    #[test]
    fn duplicate_slot_key_rejected() {
        let err = load_from_str(&slots_yaml("    - name: Bay 1\n    - name: Bay-1\n"))
            .expect_err("two slots deriving `bay_1` must be rejected");
        assert!(
            err.contains("execution.slots[1]: key bay_1 duplicates slots[0] (Bay 1)"),
            "unexpected error: {err}"
        );
        let err = load_from_str(&slots_yaml(
            "    - name: A\n      key: bay\n    - name: B\n      key: BAY\n",
        ))
        .expect_err("keys differing only in case must be rejected");
        assert!(
            err.contains("key BAY duplicates slots[0]"),
            "unexpected error: {err}"
        );

        load_from_str(&slots_yaml("    - name: Bay 1\n    - name: Bay 2\n"))
            .expect("distinct slot keys load");
    }

    #[test]
    fn slot_unknown_field_rejected_with_position() {
        let errs = load_procedure_definition_from_str_located(&slots_yaml(
            "    - name: Bay 1\n      port: 3\n",
        ))
        .unwrap_err();
        assert!(errs[0].message.contains("unknown field `port`"), "{errs:?}");
        assert_eq!(errs[0].line, Some(5), "{errs:?}");
    }

    /// It was only a `warn!` in the log, and the slot loaded with a key
    /// phase code cannot name.
    #[test]
    fn invalid_slot_key_rejected_with_position() {
        let errs = load_procedure_definition_from_str_located(&slots_yaml(
            "    - name: Bay 1\n    - name: Bay 2\n      key: bay-2\n",
        ))
        .unwrap_err();
        assert_eq!(
            errs[0].detail(),
            "execution.slots[1].key: invalid `bay-2`: expected a Python identifier; fix or remove it",
            "{errs:?}"
        );
        assert_eq!(errs[0].line, Some(6), "{errs:?}");
    }

    fn limit_yaml(validator: &str) -> String {
        format!(
            "name: A\nmain:\n  - key: p1\n    name: P1\n    python: phases.p1\n    measurements:\n      - name: Voltage\n        validators:\n          - {validator}\n"
        )
    }

    /// The one error of a limit that can never judge a value: a parse
    /// error on the validator's own line (9), so Studio underlines it.
    fn limit_error(validator: &str) -> String {
        let errs = load_procedure_definition_from_str_located(&limit_yaml(validator))
            .expect_err("a limit that can never judge must not load");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(
            (errs[0].line, errs[0].column),
            (Some(9), Some(13)),
            "{errs:?}"
        );
        assert_eq!(
            errs[0].path.as_deref(),
            Some("main[0].measurements[0].validators[0]"),
            "{errs:?}"
        );
        errs[0].message.clone()
    }

    /// Each of these loaded and evaluated to UNSET at run time, which does
    /// not fail a phase: the measurement passed with its limit unchecked.
    #[test]
    fn a_numeric_limit_written_as_text_is_refused() {
        let err = limit_error("operator: \"<=\"\n            expected_value: \"3.3\"");
        assert_eq!(
            err,
            "`<=` needs a number as `expected_value`, got text `\"3.3\"`; remove the quotes"
        );
        let err = limit_error("operator: \">\"\n            expected_value: abc");
        assert_eq!(err, "`>` needs a number as `expected_value`, got `\"abc\"`");
        let err = limit_error("operator: \">=\"\n            expected_value: [1, 2]");
        assert!(err.contains("needs a number"), "{err}");
    }

    #[test]
    fn membership_without_a_list_is_refused() {
        let err = limit_error("operator: \"in\"\n            expected_value: 3");
        assert!(err.contains("needs a list"), "{err}");
        let err = limit_error("operator: \"not in\"\n            expected_value: a");
        assert!(err.contains("needs a list"), "{err}");
    }

    #[test]
    fn a_pattern_that_is_not_a_regex_is_refused() {
        let err = limit_error("operator: \"matches\"\n            expected_value: \"^(v\"");
        assert!(err.starts_with("invalid regex `\"^(v\"`: "), "{err}");
        let err = limit_error("operator: \"matches\"\n            expected_value: 3");
        assert!(err.contains("needs text"), "{err}");
    }

    #[test]
    fn an_operator_and_its_limit_go_together() {
        let err = limit_error("operator: \"<=\"");
        assert_eq!(err, "missing `expected_value` for `<=`");
        let err = limit_error("operator: \"==\"\n            expected_value: ~");
        assert_eq!(err, "missing `expected_value` for `==`");
        let err = limit_error("expected_value: 3.3");
        assert_eq!(err, "missing `operator`");
    }

    #[test]
    fn an_aggregation_limit_that_can_never_judge_is_refused() {
        let yaml = "name: A\nmain:\n  - key: p1\n    name: P1\n    python: phases.p1\n    measurements:\n      - name: Voltage\n        aggregations:\n          - type: max\n            validators:\n              - operator: \"<\"\n                expected_value: \"5\"\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        assert_eq!(
            errs[0].path.as_deref(),
            Some("main[0].measurements[0].aggregations[0].validators[0]"),
            "{errs:?}"
        );
        assert!(errs[0].message.contains("remove the quotes"), "{errs:?}");
        assert_eq!(errs[0].line, Some(11), "{errs:?}");
    }

    /// A limit on the measurement itself of a multi-dimensional one met a
    /// set of series it read as null, and FAILed every run as "the
    /// recorded value null is not a number". Limits go on an axis or an
    /// aggregation.
    #[test]
    fn a_limit_on_a_multi_dimensional_measurement_is_refused() {
        let yaml = "name: A\nmain:\n  - key: p1\n    name: P1\n    python: phases.p1\n    measurements:\n      - name: Trace\n        x_axis:\n          key: t\n        y_axis:\n          - key: v\n        validators:\n          - operator: \"<=\"\n            expected_value: 5\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(
            errs[0].path.as_deref(),
            Some("main[0].measurements[0].validators"),
            "{errs:?}"
        );
        assert!(
            errs[0].message.starts_with("not allowed with `x_axis`")
                && errs[0].message.contains("x_axis.validators")
                && errs[0].message.contains("aggregations"),
            "{errs:?}"
        );
        assert!(errs[0].line.is_some(), "{errs:?}");

        // An empty list judges nothing, and the limits on the axes load.
        load_procedure_definition_from_str(&yaml.replace(
            "        validators:\n          - operator: \"<=\"\n            expected_value: 5\n",
            "        validators: []\n",
        ))
        .expect("an empty validator list loads");
    }

    fn axis_yaml(axis: &str) -> String {
        format!(
            "name: A\nmain:\n  - key: p1\n    name: P1\n    python: phases.p1\n    measurements:\n      - name: Trace\n{axis}"
        )
    }

    /// Axis validators and the aggregations under an axis loaded whatever
    /// their limit, then read FAIL with `UnjudgedLimit` on every run.
    #[test]
    fn an_axis_limit_that_can_never_judge_is_refused() {
        let cases = [
            (
                "        x_axis:\n          key: t\n          validators:\n            - operator: \"==\"\n",
                "x_axis.validators[0]: missing `expected_value` for `==`",
            ),
            (
                "        y_axis:\n          - key: v\n            validators:\n              - expected_value: [1, 2]\n",
                "y_axis[0].validators[0]: missing `operator`",
            ),
            (
                "        y_axis:\n          - key: v\n            validators:\n              - operator: \"<\"\n",
                "y_axis[0].validators[0]: missing `expected_value` for `<`",
            ),
            (
                "        x_axis:\n          key: t\n          aggregations:\n            - type: max\n              validators:\n                - operator: \"<\"\n                  expected_value: \"5\"\n",
                "x_axis.aggregations[0].validators[0]: `<` needs a number",
            ),
            (
                "        y_axis:\n          - key: v\n            aggregations:\n              - type: mean\n                validators:\n                  - operator: \"in\"\n                    expected_value: 3\n",
                "needs a list",
            ),
        ];
        for (axis, expected) in cases {
            let errs = load_procedure_definition_from_str_located(&axis_yaml(axis))
                .expect_err(&format!("must not load:\n{axis}"));
            assert_eq!(errs.len(), 1, "{errs:?}");
            assert!(errs[0].detail().contains(expected), "{axis}\n{errs:?}");
            assert!(errs[0].line.is_some(), "{errs:?}");
        }
    }

    /// The axis rule stays a warning: a bound on an axis loads and is left
    /// UNSET at run time (`resolve_runtime_refs` names it), whatever the
    /// limit, since it is never judged.
    #[test]
    fn an_axis_limit_the_axis_rule_leaves_unset_still_loads() {
        for axis in [
            "        y_axis:\n          - key: v\n            validators:\n              - operator: \"<=\"\n                expected_value: 2\n",
            "        y_axis:\n          - key: v\n            validators:\n              - operator: \"<=\"\n                expected_value: \"2\"\n",
            "        x_axis:\n          key: t\n          validators:\n            - operator: \"==\"\n              expected_value: [1, 2]\n",
            "        y_axis:\n          - key: v\n            aggregations:\n              - type: max\n                validators:\n                  - operator: \"<=\"\n                    expected_value: 2\n",
        ] {
            load_procedure_definition_from_str(&axis_yaml(axis))
                .unwrap_or_else(|e| panic!("must load:\n{axis}\n{e}"));
        }
    }

    /// The checks refuse only what can never judge: a text limit under
    /// `==` is a legitimate string comparison, and `.inf` keeps loading
    /// during its deprecation.
    #[test]
    fn judgeable_limits_still_load() {
        for validator in [
            "operator: \"==\"\n            expected_value: \"3.3\"",
            "operator: \"<=\"\n            expected_value: [3.3]",
            "operator: \"in\"\n            expected_value: [1, a]",
            "operator: \"matches\"\n            expected_value: [\"^v\\\\d+\"]",
            "operator: \"<=\"\n            expected_value: .inf",
        ] {
            load_procedure_definition_from_str(&limit_yaml(validator))
                .unwrap_or_else(|e| panic!("`{validator}` must load: {e}"));
        }
    }

    /// A validator operator the evaluator does not know used to load and
    /// evaluate to UNSET, so the limit was silently gone and the
    /// measurement passed. It is a parse error now, positioned on the
    /// offending line so Studio underlines it.
    #[test]
    fn unknown_validator_operator_rejected_with_position() {
        let yaml = "name: A\nmain:\n  - key: p1\n    name: P1\n    python: phases.p1\n    measurements:\n      - name: Voltage\n        validators:\n          - operator: \"=>\"\n            expected_value: 3.1\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        let err = &errs[0];
        assert!(
            err.is_parse_error()
                && err.message == "unknown value `=>`; did you mean `>=`?"
                && err.path.as_deref() == Some("main[0].measurements[0].validators[0].operator"),
            "got: {}",
            err.message
        );
        assert_eq!(err.line, Some(9), "got: {err:?}");

        // Each with a limit it can judge: `in` with a scalar or `matches`
        // with a number never judges a value and is refused too.
        for (op, limit) in [
            ("==", "3.1"),
            ("!=", "3.1"),
            (">", "3.1"),
            (">=", "3.1"),
            ("<", "3.1"),
            ("<=", "3.1"),
            ("in", "[3.1, 3.2]"),
            ("not in", "[3.1, 3.2]"),
            ("matches", "\"^3\""),
        ] {
            let ok = yaml
                .replace("\"=>\"", &format!("\"{op}\""))
                .replace("expected_value: 3.1", &format!("expected_value: {limit}"));
            load_procedure_definition_from_str(&ok)
                .unwrap_or_else(|e| panic!("operator `{op}` must load: {e}"));
        }
    }

    /// The worker matches a parameter to a plug case-insensitively, so
    /// two keys differing only in case are one binding and must not load.
    #[test]
    fn plug_keys_differing_only_in_case_rejected() {
        let err = load_from_str(
            r#"
name: Case Clash
version: "1.0"
plugs:
  - name: Meter A
    key: DMM
    python: instruments.a:A
  - name: Meter B
    key: dmm
    python: instruments.b:B
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("`DMM` and `dmm` are one binding for a phase");
        assert!(
            err.contains("duplicates plugs[0]"),
            "unexpected error: {err}"
        );
    }

    /// A plug keyed `dashboard` can never reach a phase: the worker
    /// resolves the API client before plug keys.
    #[test]
    fn plug_keyed_dashboard_rejected() {
        let err = load_from_str(
            r#"
name: Shadowed
version: "1.0"
plugs:
  - name: Meter
    key: Dashboard
    python: instruments.a:A
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("`dashboard` must be refused as a plug key");
        assert!(
            err.contains("plugs[0].key: `dashboard` reserved"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn phase_keyed_dashboard_rejected() {
        let err = load_from_str(
            r#"
name: Shadowed Phase
version: "1.0"
setup:
  - key: dashboard
    name: Prep
    python: phases.prep
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("`dashboard` must be refused as a phase key");
        assert!(
            err.contains("setup[0].key: `dashboard` reserved"),
            "unexpected error: {err}"
        );
    }

    /// Keys of the older built-ins keep loading: procedures with a phase
    /// keyed `run` exist today.
    #[test]
    fn phase_keyed_run_still_loads() {
        load_from_str(
            r#"
name: Legacy
version: "1.0"
main:
  - key: run
    name: Run
    python: phases.run
"#,
        )
        .expect("a phase keyed `run` is not refused");
    }

    /// `dashboard` is filled in by the runtime; a `config:` entry of that
    /// name would be silently overridden.
    #[test]
    fn plug_config_dashboard_rejected() {
        let err = load_from_str(
            r#"
name: Configured Dashboard
version: "1.0"
plugs:
  - name: Registry
    python: plugs.registry:Registry
    config:
      dashboard: fake
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("config.dashboard must be refused");
        assert!(
            err.contains("plugs[0].config.dashboard: reserved"),
            "unexpected error: {err}"
        );
    }

    /// A procedure written for 1.9.0 to 1.9.2 names the removed block.
    #[test]
    fn leftover_dashboard_block_names_the_fix() {
        let err = load_from_str(
            r#"
name: Old
version: "1.0"
dashboard:
  identity: station
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect_err("the block is an unknown field");
        assert!(
            err.contains("dashboard: no longer supported; remove the block"),
            "unexpected error: {err}"
        );
    }

    /// The old top-level `vars:` is refused with its new name, not serde's
    /// list of every field.
    #[test]
    fn the_old_vars_block_names_its_new_spelling() {
        let err = load_from_str(
            "name: Old\nvars:\n  GAIN: {}\nmain:\n  - name: P1\n    python: phases.p1\n",
        )
        .expect_err("vars: is no longer a field");
        assert!(
            err.contains("vars: `vars:` was renamed `config:` (markers `{config.X}`, phase parameter `config`)"),
            "unexpected error: {err}"
        );
        assert!(!err.contains("expected one of"), "unexpected error: {err}");
    }

    /// The check must not fire on the ordinary case.
    #[test]
    fn distinct_plug_keys_load() {
        let def = load_from_str(
            r#"
name: Two Plugs
version: "1.0"
plugs:
  - name: Power Supply
    python: instruments.psu:PowerSupply
  - name: Multimeter
    python: instruments.dmm:Multimeter
main:
  - key: p1
    name: P1
    python: phases.p1
"#,
        )
        .expect("distinct keys load");
        assert_eq!(def.plugs.len(), 2);
    }

    #[test]
    fn station_scope_on_phase_rejected() {
        for stage in ["setup", "teardown"] {
            let result = load_from_str(&format!(
                r#"
name: Station Phase Bad
version: "1.0"
{stage}:
  - key: s1
    name: S1
    python: phases.s1
    scope: station
main:
  - key: p1
    name: P1
    python: phases.p1
"#
            ));
            let err = result.expect_err("station scope on a phase must be rejected");
            assert!(
                err.contains("only valid on plugs"),
                "unexpected error for {stage}: {err}"
            );
        }
    }

    #[test]
    fn an_empty_command_loads_and_is_reported_not_refused() {
        // The editor's "runtime chosen, command not yet" state. It has to
        // LOAD — Studio's Sequence view runs this same loader, so a
        // refusal would remove the panel the user needs to fix it — and
        // it has to be REPORTED, because `sh -c ""` exits 0: unreported,
        // it is a phase that silently passes without doing anything.
        let dir = std::env::temp_dir().join(format!("tp-exec-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(
            &path,
            r#"
name: Empty Command
main:
  - key: flash
    name: Flash
    executable:
      command: ""
  - key: measure
    name: Measure
    executable:
      command: ./measure.sh
"#,
        )
        .unwrap();

        let def = load_procedure_definition(&path)
            .expect("an empty command must load, so the editor can show it");
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(
            problems[0].message.contains("`flash`")
                && problems[0].message.contains("`executable.command` empty"),
            "got: {}",
            problems[0].message
        );

        // A partial run that does not include the phase is not blocked by
        // it — same rule as the python refs beside it.
        let filter: HashSet<String> = ["measure".to_string()].into_iter().collect();
        assert!(
            def.resolve_runtime_refs(&dir, Some(&filter))
                .iter()
                .all(|p| !p.is_error()),
            "a partial run must not be gated by an unrelated plug or phase"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_reports_dangling_refs_loading_lets_through() {
        let dir = std::env::temp_dir().join(format!("tp-loader-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("phases")).unwrap();
        std::fs::create_dir_all(dir.join("plugs")).unwrap();
        std::fs::write(dir.join("phases/main.py"), "def check():\n    pass\n").unwrap();
        std::fs::write(dir.join("plugs/psu.py"), "class PSU:\n    pass\n").unwrap();
        let path = dir.join("procedure.yaml");
        // Both refs use the dotted-callable spelling of the 2026-08-13
        // incident: every dot is a directory, so they resolve to
        // plugs/psu/PSU.py and phases/main/check.py — neither exists.
        std::fs::write(
            &path,
            r#"
name: Dangling Refs
plugs:
  - name: PSU
    key: psu
    python: plugs.psu.PSU
main:
  - key: p1
    name: P1
    python: phases.main.check
"#,
        )
        .unwrap();

        let def = load_procedure_definition(&path)
            .expect("structural loading must let dangling refs through");
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        assert!(
            problems[0].message.starts_with("Plug `psu`"),
            "got: {}",
            problems[0]
        );
        assert!(
            problems[1].message.starts_with("Phase `p1`"),
            "got: {}",
            problems[1]
        );
        for p in &problems {
            assert!(p.message.contains("Python file not found: "), "got: {p}");
        }

        // The ':' spelling resolves both against the same files.
        std::fs::write(
            &path,
            r#"
name: Resolving Refs
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PSU
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        )
        .unwrap();
        let def = load_procedure_definition(&path).expect("valid procedure");
        assert!(def.resolve_runtime_refs(&dir, None).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Scaffold for the lint tests: a procedure dir with one plug file and
    /// one phase file, and the YAML written by the caller.
    fn lint_fixture(
        plug_py: &str,
        phase_py: &str,
        yaml: &str,
    ) -> (std::path::PathBuf, ProcedureDefinition) {
        let dir = std::env::temp_dir().join(format!("tp-lint-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("phases")).unwrap();
        std::fs::create_dir_all(dir.join("plugs")).unwrap();
        std::fs::write(dir.join("plugs/psu.py"), plug_py).unwrap();
        std::fs::write(dir.join("phases/main.py"), phase_py).unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(&path, yaml).unwrap();
        let def = load_procedure_definition(&path).expect("fixture must load");
        (dir, def)
    }

    /// A panel problem stops the run like a dangling `python:`, at the
    /// phase's `ui` entry; once clean, the file loads as written.
    #[test]
    fn resolve_runtime_refs_judges_the_panel_then_it_loads() {
        let yaml = r#"
name: Panel
main:
  - key: p1
    name: P1
    python: phases.main:check
    ui:
      panel: panels/p1.html
      components:
        - { type: switch, key: armed }
"#;
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def check(ui):\n    ui.speed = 1\n",
            yaml,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].is_error());
        assert!(
            problems[0]
                .message
                .starts_with("Phase `p1`: `panel: panels/p1.html`: file not found"),
            "{}",
            problems[0]
        );
        assert_eq!(problems[0].location.key.as_deref(), Some("ui.panel"));

        std::fs::create_dir_all(dir.join("panels")).unwrap();
        std::fs::write(
            dir.join("panels/p1.html"),
            "<input type=\"checkbox\" name=\"armd\">",
        )
        .unwrap();
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].message.contains("no component `armd`"),
            "{}",
            problems[0]
        );

        // A `ui.<key>` the phase writes without declaring it is a display
        // value; one it never writes is a warning, read from its source.
        std::fs::write(dir.join("panels/p1.html"), "<b data-bind=\"ui.sped\"></b>").unwrap();
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(
            problems.len() == 1 && !problems[0].is_error(),
            "{problems:?}"
        );
        assert!(
            problems[0].message.contains("never writes `ui.sped"),
            "{}",
            problems[0]
        );

        let html = "<input type=\"checkbox\" name=\"armed\"><b data-bind=\"ui.speed\"></b><button name=\"tare\">Tare</button>";
        std::fs::write(dir.join("panels/p1.html"), html).unwrap();
        assert!(def.resolve_runtime_refs(&dir, None).is_empty());

        let panel = crate::ui::panel::load(&dir, "panels/p1.html").unwrap();
        assert_eq!(panel.html, html);
        assert_eq!(panel.events, vec!["tare".to_string()]);
    }

    /// The run-level panel is judged like a phase's, at the root's
    /// `ui.panel`.
    #[test]
    fn resolve_runtime_refs_judges_the_procedure_panel_at_the_root() {
        let yaml = r#"
name: Panel
ui:
  panel: panels/run.html
main:
  - key: p1
    name: P1
    python: phases.main:check
"#;
        let (dir, def) = lint_fixture("class PSU:\n    pass\n", "def check():\n    pass\n", yaml);
        std::fs::create_dir_all(dir.join("panels")).unwrap();
        std::fs::write(
            dir.join("panels/run.html"),
            "<b data-bind=\"unit.serial\"></b>",
        )
        .unwrap();
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().all(|p| p.is_error()));
        assert!(
            problems.iter().any(|p| p.message.contains("exactly one")),
            "{problems:?}"
        );
        assert_eq!(location_path(&problems[0].location), "ui.panel");
        let positions = crate::procedure::yaml::positions(yaml);
        assert!(
            problems[0]
                .render("procedure.yaml", &positions)
                .starts_with("procedure.yaml:4:3: ui.panel: "),
            "{}",
            problems[0].render("procedure.yaml", &positions)
        );

        std::fs::write(
            dir.join("panels/run.html"),
            "<b data-bind=\"unit.serial_number\"></b><slot></slot>",
        )
        .unwrap();
        assert!(def.resolve_runtime_refs(&dir, None).is_empty());
    }

    #[test]
    fn resolve_runtime_refs_warns_on_a_declared_measurement_the_phase_never_records() {
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def check(measurements):\n    measurements.motor_current = 4.3\n",
            r#"
name: Mismatch
main:
  - key: p1
    name: P1
    python: phases.main:check
    measurements:
      - key: top_motor_current
        name: motor_current
      - key: motor_current
        name: Motor current
      - key: operator_note
        name: Operator note
    ui:
      components:
        - key: note
          type: text_input
          bind: measurements.operator_note
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(!problems[0].is_error(), "the static scan can only warn");
        let expected = crate::measurements::MatchIssue::NotRecorded {
            measurement: "top_motor_current".into(),
            part: crate::measurements::DeclaredPart::Measurement,
        };
        assert_eq!(problems[0].message, format!("Phase `p1`: {expected}"));
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "measurements")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// No Python, required, no options: a prompt nobody can answer. The
    /// file still loads (Studio must keep its editor up while the author
    /// fixes it) and the lint reports an error, which `run` stops on; the
    /// optional variant is a warning.
    #[test]
    fn required_choice_nothing_can_fill_is_a_lint_error_not_a_load_error() {
        for (required, is_error) in [(true, true), (false, false)] {
            let (dir, def) = lint_fixture(
                "class PSU:\n    pass\n",
                "",
                &format!(
                    "name: Stuck\nmain:\n  - key: p1\n    name: P1\n    ui:\n      components:\n        - key: mo\n          type: select\n          required: {required}\n"
                ),
            );
            let problems = def.resolve_runtime_refs(&dir, None);
            assert_eq!(problems.len(), 1, "required={required}: {problems:?}");
            assert_eq!(problems[0].is_error(), is_error, "required={required}");
            assert!(
                problems[0]
                    .message
                    .contains("no Python phase can fill them"),
                "{}",
                problems[0].message
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    /// A select declared without options is legitimate when the phase
    /// fills it at run time (TP-1246): the loader accepts it and the lint
    /// stays quiet once the phase's Python names it in `ui.set`.
    #[test]
    fn resolve_runtime_refs_accepts_a_choice_the_phase_fills_at_run_time() {
        let yaml = r#"
name: Runtime options
main:
  - key: p1
    name: P1
    python: phases.main:pick
    ui:
      components:
        - key: mo
          type: select
          required: true
"#;
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def pick(ui):\n    ui.set(\"mo\", options=[\"MO-1\"])\n",
            yaml,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();

        // Handing `ui` to a helper may fill it out of sight: quiet too.
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def pick(ui):\n    fill_orders(ui)\n",
            yaml,
        );
        assert!(def.resolve_runtime_refs(&dir, None).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nothing fills the list: a warning that names the fix, never an
    /// error, so a procedure running in the field keeps loading.
    #[test]
    fn resolve_runtime_refs_warns_on_a_choice_nothing_fills() {
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def pick(ui):\n    ui.set(\"other\", label=\"x\")\n",
            r#"
name: Runtime options
main:
  - key: p1
    name: P1
    python: phases.main:pick
    ui:
      components:
        - key: mo
          type: select
          required: true
        - key: other
          type: text_input
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(!problems[0].is_error());
        assert_eq!(
            problems[0].message,
            "Phase `p1`: `mo` has no options and `pick` never calls `ui.set(\"mo\", ...)`; add `options:` or call `ui.set(\"mo\", options=[...])` in the phase"
        );
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "ui")
        );
        std::fs::remove_dir_all(&dir).ok();

        // No Python at all: nothing can ever fill it.
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "",
            r#"
name: Runtime options
main:
  - key: p1
    name: P1
    ui:
      components:
        - key: mo
          type: radio
          required: false
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(
            problems[0]
                .message
                .contains("no Python phase can fill them"),
            "{}",
            problems[0].message
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_warns_on_a_numeric_limit_declared_on_an_axis() {
        let (dir, def) = lint_fixture(
            "class PSU:\n    pass\n",
            "def check(measurements):\n    measurements.motor_current = 4.3\n",
            r#"
name: Axis limit
main:
  - key: p1
    name: P1
    python: phases.main:check
    measurements:
      - key: motor_current
        name: Motor current
        y_axis:
          - key: top
            validators: [{ operator: "<=", expected_value: 2 }]
            aggregations:
              - type: max
                validators: [{ operator: "<=", expected_value: 2 }]
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(
            !problems[0].is_error(),
            "the run starts and leaves the limit UNSET"
        );
        let expected = crate::measurements::MatchIssue::UnsupportedAxisOperator {
            measurement: "motor_current".into(),
            axis: Some("top".into()),
            operator: "<=".into(),
        };
        assert_eq!(problems[0].message, format!("Phase `p1`: {expected}"));
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "measurements")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A text answer under a numeric limit reads FAIL on every run.
    #[test]
    fn resolve_runtime_refs_warns_on_a_text_component_bound_under_a_numeric_limit() {
        let yaml = |component: &str| {
            format!(
                r#"
name: Bound
main:
  - key: p1
    name: P1
    python: phases.main:check
    measurements:
      - key: vout
        name: Vout
        validators: [{{ operator: "<=", expected_value: 3.3 }}]
      - key: fw
        name: FW
        validators: [{{ operator: "==", expected_value: "1.2" }}]
    ui:
      components:
        - type: {component}
          key: vout_in
          label: Vout
          bind: measurements.vout
        - type: text_input
          key: fw_in
          label: FW
          bind: measurements.fw
"#
            )
        };
        for component in ["text_input", "textarea", "select", "switch"] {
            let (dir, def) = lint_fixture(
                "class PSU:\n    pass\n",
                "def check():\n    pass\n",
                &yaml(component),
            );
            let problems: Vec<_> = def
                .resolve_runtime_refs(&dir, None)
                .into_iter()
                .filter(|p| p.message.contains("compares numbers"))
                .collect();
            assert_eq!(problems.len(), 1, "{component}: {problems:?}");
            assert!(!problems[0].is_error(), "phase code may record over it");
            assert!(
                problems[0]
                    .message
                    .contains(&format!("`vout_in` (`{component}`)"))
                    && problems[0].message.contains("`<=` limit")
                    && problems[0].message.contains("number_input"),
                "{}",
                problems[0].message
            );
            assert_eq!(
                problems[0].location,
                RefLocation::key(RefList::Main, 0, "ui")
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        for component in ["number_input", "slider"] {
            let (dir, def) = lint_fixture(
                "class PSU:\n    pass\n",
                "def check():\n    pass\n",
                &yaml(component),
            );
            assert!(
                def.resolve_runtime_refs(&dir, None)
                    .iter()
                    .all(|p| !p.message.contains("compares numbers")),
                "{component} records a number"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn resolve_runtime_refs_warns_on_a_membership_limit_declared_on_an_axis() {
        // `in` / `not in` would test the whole series for membership: they
        // are left UNSET at run time like the numeric operators.
        for operator in ["in", "not in"] {
            let (dir, def) = lint_fixture(
                "class PSU:\n    pass\n",
                "def check(measurements):\n    measurements.motor_current = 4.3\n",
                &format!(
                    r#"
name: Axis membership
main:
  - key: p1
    name: P1
    python: phases.main:check
    measurements:
      - key: motor_current
        name: Motor current
        y_axis:
          - key: top
            validators: [{{ operator: "{operator}", expected_value: [0] }}]
"#
                ),
            );
            let problems = def.resolve_runtime_refs(&dir, None);
            let expected = crate::measurements::MatchIssue::UnsupportedAxisOperator {
                measurement: "motor_current".into(),
                axis: Some("top".into()),
                operator: operator.into(),
            };
            assert_eq!(problems.len(), 1, "{operator}: {problems:?}");
            assert_eq!(problems[0].message, format!("Phase `p1`: {expected}"));
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn resolve_runtime_refs_reports_a_symbol_the_file_does_not_define() {
        // The file exists, the identifier is valid — and the name is a
        // typo. Before this rule the first signal was an import failure
        // on the bench.
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    def __init__(self, address):\n        pass\n",
            "def check():\n    pass\n",
            r#"
name: Typos
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PowerSuply
main:
  - key: p1
    name: P1
    python: phases.main:chek
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        assert!(
            problems[0].message.contains("class `PowerSuply` not found"),
            "got: {}",
            problems[0]
        );
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Plugs, 0, "python")
        );
        assert!(
            problems[1].message.contains("`chek` not found"),
            "got: {}",
            problems[1]
        );
        assert_eq!(
            problems[1].location,
            RefLocation::key(RefList::Main, 0, "python")
        );
        assert!(problems.iter().all(|p| p.is_error()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_accepts_re_exports_and_factories() {
        // A lint that blocks a run must not call a working reference
        // broken: `module:Name` resolves through a top-level import or
        // assignment just as well as through `class Name`.
        let (dir, def) = lint_fixture(
            "from .impl import PowerSupply\nMeter = make_plug('dmm')\n",
            "from .steps import check\n",
            r#"
name: Indirect
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PowerSupply
  - name: DMM
    key: dmm
    python: plugs.psu:Meter
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        );
        // The modules the re-exports name, so the files would import.
        std::fs::write(dir.join("plugs/impl.py"), "class PowerSupply:\n    pass\n").unwrap();
        std::fs::write(dir.join("phases/steps.py"), "def check():\n    pass\n").unwrap();
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_checks_plug_config_against_init() {
        // `config:` is splatted into the class as kwargs: an unknown key
        // and a missing required one are both a TypeError at setup. The
        // suggestion uses the same two-edit threshold as the Inspector.
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    def __init__(self, address: str, port: int = 5025, initial_voltage: float = 0.0):\n        pass\n",
            "def check():\n    pass\n",
            r#"
name: Config
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PowerSupply
    config:
      port: 1
      intial_voltage: 3.3
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        let unknown = problems
            .iter()
            .find(|p| p.message.contains("`intial_voltage` is not a parameter"))
            .expect("unknown key reported");
        assert!(
            unknown.message.contains("did you mean `initial_voltage`"),
            "got: {unknown}"
        );
        assert_eq!(
            unknown.location,
            RefLocation::key(RefList::Plugs, 0, "config.intial_voltage")
        );
        assert!(unknown.is_error(), "a def __init__ is authoritative");
        let missing = problems
            .iter()
            .find(|p| p.message.contains("missing `config.address`"))
            .expect("missing required reported");
        assert_eq!(
            missing.location,
            RefLocation::key(RefList::Plugs, 0, "config")
        );
        assert!(missing.is_error());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_does_not_require_dashboard_in_plug_config() {
        // `dashboard` is injected by the framework, never set by `config:`.
        let (dir, def) = lint_fixture(
            "class SerialRegistry:\n    def __init__(self, dashboard, mode=\"api\"):\n        pass\n",
            "def check():\n    pass\n",
            r#"
name: Dashboard
plugs:
  - name: Registry
    key: registry
    python: plugs.psu:SerialRegistry
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_does_not_read_a_docstring_as_dataclass_fields() {
        // A Google-style docstring is the house style, and its
        // `Attributes:` / `Raises:` entries are indented `name: text` —
        // the shape of an annotated field. Read as fields on a
        // `@dataclass`, which is a certain constructor, every one becomes
        // an Error there is no `config:` the author can write to satisfy.
        let (dir, def) = lint_fixture(
            concat!(
                "from dataclasses import dataclass\n",
                "\n",
                "@dataclass\n",
                "class PowerSupply:\n",
                "    \"\"\"A PSU config.\n",
                "\n",
                "    Attributes:\n",
                "        address: the VISA address\n",
                "    Raises:\n",
                "        ValueError: if bad\n",
                "    \"\"\"\n",
                "\n",
                "    address: str = \"192.168.1.1\"\n",
                "    port: int = 5025\n",
            ),
            "def check():\n    pass\n",
            r#"
name: Docstring
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PowerSupply
    config:
      port: 1
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_config_rules_are_warnings_off_attributes_and_silent_on_kwargs() {
        // No `def __init__`: the annotated attributes may be a dataclass —
        // or a plain class inheriting its constructor from another file.
        // The lint cannot tell, so it warns and never refuses.
        let (dir, def) = lint_fixture(
            "class PowerSupply(Base):\n    address: str\n    port: int = 1\n\nclass Meter:\n    def __init__(self, **kwargs):\n        pass\n\nclass Opaque(Base):\n    pass\n",
            "def check():\n    pass\n",
            r#"
name: Uncertain
plugs:
  - name: PSU
    key: psu
    python: plugs.psu:PowerSupply
    config:
      baud: 9600
  - name: DMM
    key: dmm
    python: plugs.psu:Meter
    config:
      anything: goes
  - name: Opaque
    key: opaque
    python: plugs.psu:Opaque
    config:
      whatever: 1
main:
  - key: p1
    name: P1
    python: phases.main:check
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        // PSU: unknown `baud` + missing `address`, both warnings. DMM takes
        // **kwargs: nothing. Opaque: signature unknowable: nothing.
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        assert!(problems.iter().all(|p| !p.is_error()), "got: {problems:?}");
        assert!(
            problems.iter().all(|p| p.message.starts_with("Plug `psu`")),
            "got: {problems:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_reports_duplicate_measurement_keys() {
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check(measurements):\n    measurements.voltage = 1\n    measurements.current = 1\n",
            r#"
name: Dup Measurements
main:
  - key: p1
    name: P1
    python: phases.main:check
    measurements:
      - key: voltage
        name: Voltage
      - key: current
        name: Current
      - key: voltage
        name: Voltage again
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(
            problems[0]
                .message
                .contains("duplicate measurement key voltage"),
            "got: {}",
            problems[0]
        );
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "measurements")
        );
        assert!(problems[0].is_error());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Derived and stated keys collide the same way, and the finding
    /// never blocks: procedures in the field may already carry one.
    #[test]
    fn resolve_runtime_refs_warns_on_duplicate_component_keys() {
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check():\n    pass\n",
            r#"
name: Dup Components
main:
  - key: p1
    name: P1
    python: phases.main:check
    ui:
      components:
        - type: text
        - type: text
          label: Text
        - type: text_input
          key: serial
        - type: text
          key: text
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(
            problems[0]
                .message
                .contains("key `text` used by 3 operator UI components"),
            "got: {}",
            problems[0]
        );
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "ui")
        );
        assert!(!problems[0].is_error());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_runtime_refs_is_silent_on_distinct_component_keys() {
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check():\n    pass\n",
            r#"
name: Distinct Components
main:
  - key: p1
    name: P1
    python: phases.main:check
    ui:
      components:
        - type: text
        - type: text
          label: Text 2
  - key: p2
    name: P2
    python: phases.main:check
    ui:
      components:
        - type: text
"#,
        );
        // The same key in two phases is fine: the slot is per phase.
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The engine lists which fields a file STATES, the protocol crate
    /// lists which types READ them. A field in one list only would be
    /// one the lint silently never checks, or one it cannot look up.
    #[test]
    fn inert_fields_cover_the_reader_table() {
        use crate::procedure::schema::UIComponent;
        // State every type-specific field at once, on a type that reads
        // none of them: whatever comes back is what the engine knows of.
        let all_stated: UIComponent = crate::procedure::yaml::from_str(
            r#"
type: progress
key: everything
placeholder: p
min_length: 1
max_length: 2
pattern: "x"
pattern_message: m
prefix: a
suffix: b
rows: 2
step: 1
options:
  - value: a
    label: A
columns: 2
width: 50%
height: 10px
aspect: square
fit: cover
size: lg
color: red
font: monospace
"#,
        )
        .unwrap();
        // `progress` reads `min` and `max`, so those two are asked of a
        // type that does not.
        let range_on_text: UIComponent =
            crate::procedure::yaml::from_str("type: text\nkey: t\nmin: 0\nmax: 1\n").unwrap();
        let mut known: Vec<&str> = all_stated.inert_fields();
        known.extend(range_on_text.inert_fields());
        known.sort_unstable();
        let mut table: Vec<&str> = station_protocol::COMPONENT_FIELD_READERS
            .iter()
            .map(|(name, _)| *name)
            .collect();
        table.sort_unstable();
        assert_eq!(known, table);
    }

    /// One finding per component, every idle field named in it.
    #[test]
    fn resolve_runtime_refs_warns_on_fields_the_component_type_ignores() {
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check():\n    pass\n",
            r#"
name: Idle Fields
main:
  - key: p1
    name: P1
    python: phases.main:check
    ui:
      components:
        - type: text_input
          key: serial
          color: red
          size: lg
        - type: text
          key: note
          rows: 4
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        assert!(problems.iter().all(|p| !p.is_error()), "got: {problems:?}");
        assert!(
            problems[0]
                .message
                .contains("`size`, `color` have no effect on `serial` (`text_input`)"),
            "got: {}",
            problems[0]
        );
        assert!(
            problems[1]
                .message
                .contains("`rows` has no effect on `note` (`text`); remove it"),
            "got: {}",
            problems[1]
        );
        assert_eq!(
            problems[0].location,
            RefLocation::key(RefList::Main, 0, "ui")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The readers a renderer-only audit misses. Each line below is a
    /// field that looks misplaced and is not: a finding on any of them
    /// would send someone to delete a line that works.
    #[test]
    fn resolve_runtime_refs_keeps_quiet_on_fields_with_a_less_obvious_reader() {
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check():\n    pass\n",
            r#"
name: Read Elsewhere
main:
  - key: p1
    name: P1
    python: phases.main:check
    ui:
      components:
        # Length and pattern rules are enforced by response validation,
        # though TextAreaInput.tsx never reads them.
        - type: textarea
          key: notes
          rows: 3
          min_length: 2
          max_length: 200
          pattern: "[A-Z].*"
          pattern_message: Start with a capital
          placeholder: Notes
        # Image options are drawn by ImageGridInput, which takes the
        # image layout fields and columns.
        - type: radio
          key: variant
          columns: 2
          aspect: square
          fit: contain
          width: 50%
          height: 200px
          options:
            - value: a
              label: A
              image: a.png
        - type: checklist
          key: checks
          columns: 3
          options:
            - value: a
              label: A
        # A switch reads its two labels from options.
        - type: switch
          key: powered
          options:
            - value: "true"
              label: On
            - value: "false"
              label: Off
        - type: number_input
          key: voltage
          min: 0
          max: 5
          step: 0.1
          prefix: "~"
          suffix: V
          placeholder: "3.3"
        - type: slider
          key: level
          min: 0
          max: 100
          step: 1
        - type: progress
          key: progress
          min: 0
          max: 100
        - type: image
          key: board
          width: 50%
          height: 300px
          aspect: auto
          fit: cover
          placeholder: No image
        - type: text
          key: note
          size: lg
          color: red
          font: monospace
        - type: select
          key: station
          placeholder: Pick one
          options:
            - value: a
              label: A
"#,
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems.is_empty(), "unexpected: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The file's own shape, kept through the parse: `key` alone cannot
    /// tell a stated key from a derived one, and the Inspector needs to.
    #[test]
    fn component_key_stated_survives_the_parse() {
        let def = load_procedure_definition_from_str(
            r#"
name: Keys
main:
  - key: p1
    name: P1
    ui:
      components:
        - type: text_input
          label: Serial Number
        - type: text_input
          label: Serial Number
          key: sn
        - type: text
          label: "!!!"
"#,
        )
        .unwrap();
        let components = def.main[0]
            .ui
            .as_ref()
            .unwrap()
            .components
            .as_ref()
            .unwrap();
        let seen: Vec<(&str, bool)> = components
            .iter()
            .map(|c| (c.key.as_str(), c.key_stated))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("serial_number", false),
                ("sn", true),
                // A label no identifier survives falls through to the
                // type name instead of deriving "", which refused to load.
                ("text", false),
            ]
        );
    }

    /// What `tofupilot run` prints: one compiler-style line per error,
    /// lint findings and deprecations in the same shape.
    #[test]
    fn errors_render_compiler_style() {
        let yaml = "name: A\nplugs:\n  - name: P\n    python: plugs.p:P\n    config:\n      a: .inf\nmain:\n  - key: a\n    name: A\n    depends_on: [b]\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        assert_eq!(
            errs[0].render("procedure.yaml"),
            "procedure.yaml:10:18: main[0].depends_on[0]: unknown phase `b` (phases: a)"
        );

        let outcome = inspect_procedure_definition(yaml);
        assert_eq!(
            outcome.warnings[0].render("procedure.yaml"),
            "procedure.yaml:6:10: plugs[0].config.a: `.inf` deprecated, plug gets None; write a number or remove the key"
        );
        assert!(outcome.warnings[0]
            .message
            .ends_with(crate::procedure::yaml::DEPRECATION_NOTICE));

        let problem = crate::procedure::schema::RefProblem::error(
            crate::procedure::codes::PLUG_PYTHON_INVALID,
            "Plug `p`: Python file not found: plugs/p.py",
            RefLocation::key(RefList::Plugs, 0, "python"),
        );
        assert_eq!(
            problem.render("procedure.yaml", &crate::procedure::yaml::positions(yaml)),
            "procedure.yaml:4:5: plugs[0].python: Python file not found: plugs/p.py"
        );
    }

    #[test]
    fn located_load_errors_point_at_the_entry_or_the_parse_position() {
        let dup = "name: A\nplugs:\n  - name: Power Supply\n    python: plugs.a:A\n  - name: Power-Supply\n    python: plugs.b:B\nmain:\n  - key: p\n    name: P\n";
        let errs = load_procedure_definition_from_str_located(dup).unwrap_err();
        assert_eq!(errs.len(), 1, "got: {errs:?}");
        let err = &errs[0];
        assert!(
            err.message.contains("key power_supply duplicates plugs[0]"),
            "got: {}",
            err.message
        );
        assert_eq!(err.location, Some(RefLocation::entry(RefList::Plugs, 1)));
        // The entry's own position, resolved from its path.
        assert_eq!(err.path.as_deref(), Some("plugs[1]"));
        assert_eq!((err.line, err.column), (Some(5), Some(5)));
        assert_eq!(
            err.render("procedure.yaml"),
            "procedure.yaml:5:5: plugs[1]: key power_supply duplicates plugs[0] (Power Supply); set `key:`"
        );

        let cycle = "name: A\nmain:\n  - key: a\n    name: A\n    depends_on: [b]\n  - key: b\n    name: B\n    depends_on: [a]\n";
        let errs = load_procedure_definition_from_str_located(cycle).unwrap_err();
        assert!(
            errs[0].message.contains("cycle a -> b -> a"),
            "got: {}",
            errs[0].message
        );
        assert_eq!(
            errs[0].location,
            Some(RefLocation::key(RefList::Main, 0, "depends_on"))
        );

        let bad_yaml = "name: A\nmain:\n  - key: [unclosed\n";
        let errs = load_procedure_definition_from_str_located(bad_yaml).unwrap_err();
        let err = &errs[0];
        assert!(err.is_parse_error(), "got: {err:?}");
        assert!(err.line.is_some() && err.column.is_some(), "got: {err:?}");
        assert_eq!(err.location, None);

        // The string form is `path: message`, one line per error.
        assert_eq!(
            load_procedure_definition_from_str(dup).unwrap_err(),
            load_procedure_definition_from_str_located(dup).unwrap_err()[0].detail()
        );
    }

    #[test]
    fn inspect_keeps_the_definition_when_only_rules_fail() {
        let dup = "name: A\nplugs:\n  - name: P\n    key: p\n    python: plugs.a:A\nmain:\n  - key: m\n    name: M\n  - key: m\n    name: M2\n";
        let outcome = inspect_procedure_definition(dup);
        assert_eq!(outcome.errors.len(), 1, "got: {:?}", outcome.errors);
        let def = outcome
            .definition
            .expect("a duplicate key is unrunnable, not uninspectable");
        assert_eq!(def.plugs.len(), 1);
        // The runner-facing loaders still refuse it.
        assert!(load_procedure_definition_from_str_located(dup).is_err());
        assert!(load_procedure_definition_from_str(dup).is_err());

        // A parse failure leaves nothing to inspect.
        let broken = inspect_procedure_definition("name: A\nmain:\n  - key: [x\n");
        assert!(broken.definition.is_none());
        assert_eq!(broken.errors.len(), 1);
    }

    #[test]
    fn bound_identity_affix_outside_the_server_charset_fails_at_load() {
        // The same guard `unit:` fields get, on the phase-prompt path
        // that composes the affix into the recorded serial: catch the
        // bad character here, not at upload after the test has run.
        let yaml = "name: A\nmain:\n  - key: id\n    name: Identify\n    ui:\n      components:\n        - key: sn\n          type: text_input\n          bind: unit.serial_number\n          prefix: \"SN#\"\n";
        let outcome = inspect_procedure_definition(yaml);
        assert_eq!(outcome.errors.len(), 1, "got: {:?}", outcome.errors);
        assert!(
            outcome.errors[0].detail()
                == "main[0].ui.components[0].prefix: invalid `SN#` for `bind: unit.serial_number`: expected A-Z a-z 0-9 _ . : + -",
            "{:?}",
            outcome.errors
        );
        let ok = yaml.replace("\"SN#\"", "\"SN-\"");
        assert!(inspect_procedure_definition(&ok).errors.is_empty());
    }

    #[test]
    fn loader_reports_every_failed_rule_not_just_the_first() {
        // A duplicate plug key AND an unknown depends_on AND a duplicate
        // phase key: fixing one must not be how you discover the next.
        let yaml = "name: A\nplugs:\n  - name: P\n    key: p\n    python: plugs.a:A\n  - name: P2\n    key: p\n    python: plugs.b:B\nmain:\n  - key: a\n    name: A\n    depends_on: [zzz]\n  - key: a\n    name: A2\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        let msgs: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(errs.len(), 3, "got: {msgs:?}");
        assert!(
            msgs.iter()
                .any(|m| m.contains("duplicate key a (also main[0])")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("key p duplicates plugs[0]")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("unknown phase `zzz`")),
            "{msgs:?}"
        );
        // One line each in the string form.
        assert_eq!(
            load_procedure_definition_from_str(yaml)
                .unwrap_err()
                .lines()
                .count(),
            3
        );
    }

    /// TP-1093: `locked: true` with no `default_value` is a load error,
    /// on unit fields and on the root `operated_by`, reported alongside
    /// the other rules rather than hiding behind them.
    #[test]
    fn loader_refuses_a_locked_field_without_a_default() {
        let yaml = "name: A\nunit:\n  serial_number: {}\n  part_number:\n    locked: true\noperated_by:\n  locked: true\nmain:\n  - key: a\n    name: A\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        let msgs: Vec<String> = errs.iter().map(LoadError::detail).collect();
        assert_eq!(errs.len(), 2, "got: {msgs:?}");
        assert!(
            msgs.iter()
                .any(|m| m == "unit.part_number: `locked` needs `default_value`"),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m == "operated_by: `locked` needs `default_value`"),
            "{msgs:?}"
        );

        // With the defaults in place the same file loads.
        let ok = "name: A\nunit:\n  serial_number: {}\n  part_number:\n    default_value: PCB\n    locked: true\noperated_by:\n  default_value: line@acme.com\n  locked: true\nmain:\n  - key: a\n    name: A\n";
        load_procedure_definition_from_str_located(ok).unwrap();
    }

    /// A `default_value` that fails its own `pattern` is a load error on
    /// every unit field, locked or not — Studio's validator reports it as
    /// an issue and the run refuses to start. Unit metadata was the gap:
    /// the form pre-filled a value the operator had to correct each run.
    #[test]
    fn loader_refuses_a_default_that_fails_its_own_pattern() {
        let yaml = "name: A\nunit:\n  serial_number: {}\n  part_number: {}\n  metadata:\n    color:\n      pattern: \"^(red|blue)$\"\n      default_value: green\nmain:\n  - key: a\n    name: A\n";
        let errs = load_procedure_definition_from_str_located(yaml).unwrap_err();
        let msgs: Vec<String> = errs.iter().map(LoadError::detail).collect();
        assert_eq!(errs.len(), 1, "got: {msgs:?}");
        assert!(
            msgs[0].starts_with("unit.metadata.color.default_value: "),
            "{msgs:?}"
        );
        // Positioned on the field that holds the default.
        assert_eq!(errs[0].line, Some(8), "{errs:?}");

        let ok = yaml.replace("default_value: green", "default_value: blue");
        load_procedure_definition_from_str_located(&ok).unwrap();
    }

    #[test]
    fn resolve_runtime_refs_warns_on_a_phase_module_outside_the_tree() {
        // `phases2` is not a directory here, so the spec may resolve to an
        // installed package at run time: say so, do not block.
        let (dir, def) = lint_fixture(
            "class PowerSupply:\n    pass\n",
            "def check():\n    pass\n",
            "name: Outside\nmain:\n  - key: p1\n    name: P1\n    python: phases2.measure:check\n",
        );
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 1, "unexpected: {problems:?}");
        assert!(!problems[0].is_error());
        assert!(
            problems[0].message.contains("not found in the project"),
            "got: {}",
            problems[0]
        );
        // Inside the tree the same miss is an error, as before.
        std::fs::create_dir_all(dir.join("phases2")).unwrap();
        let problems = def.resolve_runtime_refs(&dir, None);
        assert!(problems[0].is_error(), "got: {problems:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn resolve_runtime_refs_suggests_the_colon_spelling() {
        let dir = std::env::temp_dir().join(format!("tp-loader-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("phases")).unwrap();
        std::fs::create_dir_all(dir.join("plugs")).unwrap();
        std::fs::write(dir.join("phases/main.py"), "def check():\n    pass\n").unwrap();
        std::fs::write(dir.join("plugs/psu.py"), "class PSU:\n    pass\n").unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(
            &path,
            r#"
name: Hinted Refs
plugs:
  - name: PSU
    key: psu
    python: plugs.psu.PSU
  - name: Ghost
    key: ghost
    python: plugs.nowhere.Thing
main:
  - key: p1
    name: P1
    python: phases.main.check
"#,
        )
        .unwrap();

        let def = load_procedure_definition(&path).expect("valid structure");
        let problems = def.resolve_runtime_refs(&dir, None);
        assert_eq!(problems.len(), 3, "unexpected: {problems:?}");
        // The dotted-class spelling whose ':' variant resolves gets the
        // did-you-mean; a spec that is broken either way does not.
        assert!(
            problems[0].message.contains("did you mean `plugs.psu:PSU`"),
            "got: {}",
            problems[0]
        );
        assert!(
            !problems[1].message.contains("did you mean"),
            "got: {}",
            problems[1]
        );
        assert!(
            problems[2]
                .message
                .contains("did you mean `phases.main:check`"),
            "got: {}",
            problems[2]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn depends_on_unknown_key_in_setup_rejected() {
        let err = load_from_str(
            r#"
name: Typo In Setup
version: "1.0"
setup:
  - key: cal
    name: Cal
    python: phases.cal
    depends_on: [calibrat]
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("a setup phase depending on an unknown key must be rejected");
        assert!(
            err.contains("setup[0].depends_on[0]: unknown phase `calibrat` (phases: cal, m)"),
            "got: {err}"
        );
    }

    #[test]
    fn depends_on_later_stage_rejected() {
        let err = load_from_str(
            r#"
name: Setup Waits For Main
version: "1.0"
setup:
  - key: cal
    name: Cal
    python: phases.cal
    depends_on: [m]
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("a setup phase cannot wait for a main phase");
        assert!(
            err.contains("setup[0].depends_on[0]: `m` runs later (main)"),
            "got: {err}"
        );
    }

    #[test]
    fn shared_phase_cannot_depend_on_slot_phase() {
        let err = load_from_str(
            r#"
name: Shared Waits For Slot
version: "1.0"
setup:
  - key: per_slot
    name: Per slot
    python: phases.per_slot
  - key: shared
    name: Shared
    scope: execution
    python: phases.shared
    depends_on: [per_slot]
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("shared setup runs before per-slot setup, it cannot wait for it");
        assert!(
            err.contains("setup[1].depends_on[0]: `per_slot` runs later (setup)"),
            "got: {err}"
        );
    }

    /// Shared teardown runs after every slot's main phases: a dependency
    /// on one is redundant, and loaded on 1.7.18. It must keep loading.
    #[test]
    fn shared_teardown_may_depend_on_a_main_phase() {
        load_from_str(
            r#"
name: Shared Teardown Waits For Main
version: "1.0"
main:
  - key: measure
    name: Measure
    python: phases.measure
teardown:
  - key: power_off
    name: Power off
    scope: execution
    python: phases.power_off
    depends_on: [measure]
"#,
        )
        .expect("an earlier-stage dependency of a shared teardown is valid");
    }

    #[test]
    fn dependency_cycle_in_setup_rejected() {
        let err = load_from_str(
            r#"
name: Setup Cycle
version: "1.0"
setup:
  - key: a
    name: A
    python: phases.a
    depends_on: [b]
  - key: b
    name: B
    python: phases.b
    depends_on: [a]
main:
  - key: m
    name: M
    python: phases.m
"#,
        )
        .expect_err("a cycle in setup never starts");
        assert!(
            err.contains("setup[0].depends_on: cycle a -> b -> a"),
            "got: {err}"
        );
    }

    #[test]
    fn self_dependency_in_teardown_rejected() {
        let err = load_from_str(
            r#"
name: Teardown Self Dependency
version: "1.0"
main:
  - key: m
    name: M
    python: phases.m
teardown:
  - key: off
    name: Off
    python: phases.off
    depends_on: [off]
"#,
        )
        .expect_err("a phase cannot wait for itself");
        assert!(err.contains("off") && err.contains("itself"), "got: {err}");
    }

    #[test]
    fn lint_is_quiet_for_a_parameter_with_a_default() {
        // A defaulted parameter is not resolved from phase results when
        // the result is withheld: it receives its default, the phase runs.
        let (dir, problems) = lint_bed(
            r#"
name: Defaulted
main:
  - key: a
    name: A
    python: phases.a
  - key: b
    name: B
    python: phases.reads_a
"#,
            "def reads_a(phase, a=None):\n    pass\n",
        );
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            problems.iter().all(|p| !p.message.contains("depends_on")),
            "unexpected: {problems:?}"
        );
    }

    #[test]
    fn lint_warns_when_a_shared_teardown_names_a_per_slot_main_phase() {
        // The orchestrator only passes a shared job the results of shared
        // jobs (`execution.rs` slot filter): an earlier per-slot phase is
        // not visible to it, whatever the stage order says.
        let (dir, problems) = lint_bed(
            r#"
name: Shared teardown reads main
main:
  - key: a
    name: A
    python: phases.a
teardown:
  - key: b
    name: B
    scope: execution
    python: phases.reads_a
"#,
            "def reads_a(phase, a):\n    pass\n",
        );
        std::fs::remove_dir_all(&dir).ok();
        let hits: Vec<_> = problems
            .iter()
            .filter(|p| p.message.starts_with("Phase `b`"))
            .collect();
        assert_eq!(hits.len(), 1, "unexpected: {problems:?}");
    }

    #[test]
    fn depends_on_earlier_stage_phase_is_allowed() {
        load_from_str(
            r#"
name: Main Waits For Setup
version: "1.0"
setup:
  - key: cal
    name: Cal
    python: phases.cal
main:
  - key: m
    name: M
    python: phases.m
    depends_on: [cal]
teardown:
  - key: off
    name: Off
    python: phases.off
    depends_on: [m]
"#,
        )
        .expect("an earlier-stage dependency is redundant but valid");
    }

    fn lint_bed(
        yaml: &str,
        reads_a: &str,
    ) -> (
        std::path::PathBuf,
        Vec<crate::procedure::schema::RefProblem>,
    ) {
        let dir = std::env::temp_dir().join(format!("tp-loader-lint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("phases")).unwrap();
        std::fs::write(
            dir.join("phases/a.py"),
            "def a(measurements):\n    measurements.voltage = 5.0\n",
        )
        .unwrap();
        std::fs::write(dir.join("phases/reads_a.py"), reads_a).unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(&path, yaml).unwrap();
        let def = load_procedure_definition(&path).expect("valid procedure");
        let problems = def.resolve_runtime_refs(&dir, None);
        (dir, problems)
    }

    #[test]
    fn lint_refuses_a_parameter_naming_an_undeclared_phase() {
        let (dir, problems) = lint_bed(
            r#"
name: Undeclared
main:
  - key: a
    name: A
    python: phases.a
  - key: b
    name: B
    python: phases.reads_a
"#,
            "def reads_a(phase, a):\n    pass\n",
        );
        std::fs::remove_dir_all(&dir).ok();
        let hits: Vec<_> = problems
            .iter()
            .filter(|p| p.message.contains("depends_on"))
            .collect();
        assert_eq!(hits.len(), 1, "unexpected: {problems:?}");
        assert!(hits[0].is_error(), "the phase fails at start on every run");
        assert!(
            hits[0].message.starts_with("Phase `b`") && hits[0].message.contains("`a`"),
            "got: {}",
            hits[0]
        );
    }

    #[test]
    fn lint_is_quiet_for_declared_or_earlier_stage_results() {
        for yaml in [
            r#"
name: Declared
main:
  - key: a
    name: A
    python: phases.a
  - key: b
    name: B
    python: phases.reads_a
    depends_on: [a]
"#,
            r#"
name: Earlier stage
setup:
  - key: a
    name: A
    python: phases.a
main:
  - key: b
    name: B
    python: phases.reads_a
"#,
        ] {
            let (dir, problems) = lint_bed(yaml, "def reads_a(phase, a):\n    pass\n");
            std::fs::remove_dir_all(&dir).ok();
            assert!(
                problems.iter().all(|p| !p.message.contains("depends_on")),
                "unexpected: {problems:?}"
            );
        }
    }

    #[test]
    fn resolve_runtime_refs_mirrors_the_runtime_not_stricter() {
        let dir = std::env::temp_dir().join(format!("tp-loader-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("phases")).unwrap();
        std::fs::write(dir.join("phases/ok.py"), "def ok():\n    pass\n").unwrap();
        let path = dir.join("procedure.yaml");
        std::fs::write(
            &path,
            r#"
name: Runtime Parity
plugs:
  - name: DMM
    key: dmm
    python: plugs.dmm:DMM
main:
  - key: ok
    name: OK
    python: phases.ok
  - key: wheel
    name: Wheel
    python: shared.phases:check
  - key: off_
    name: Off
    python: phases.gone
    enabled: false
  - key: broken
    name: Broken
    python: phases.gone
"#,
        )
        .unwrap();
        let def = load_procedure_definition(&path).expect("valid procedure");

        // `shared.phases:check` has no `shared/` dir in the tree, so it may
        // resolve through tp_worker's importlib fallback (workspace wheel):
        // not the gate's call. `phases.gone` IS tree-bound (phases/ exists)
        // but its phase is disabled — no job, no gate. On a full run the
        // dangling `plugs/dmm.py` gates too: every declared plug is built.
        let all = def.resolve_runtime_refs(&dir, None);
        // The wheel-style spec is reported, but as a warning: it may
        // resolve through importlib at run time, so it must not gate.
        let warnings: Vec<_> = all.iter().filter(|p| !p.is_error()).collect();
        assert_eq!(warnings.len(), 1, "unexpected: {all:?}");
        assert!(
            warnings[0].message.starts_with("Phase `wheel`")
                && warnings[0].message.contains("not found in the project"),
            "got: {}",
            warnings[0]
        );
        let problems: Vec<_> = all.iter().filter(|p| p.is_error()).collect();
        assert_eq!(problems.len(), 2, "unexpected: {problems:?}");
        assert!(
            problems[0].message.starts_with("Plug `dmm`"),
            "got: {}",
            problems[0]
        );
        assert!(
            problems[1].message.starts_with("Phase `broken`"),
            "got: {}",
            problems[1]
        );

        // Partial run on `ok`: `broken` is outside the dependency closure,
        // and plugs don't gate at all (the runtime narrows the plug set by
        // signature introspection, so `dmm` would never be built) — the
        // same procedure starts.
        let filter: std::collections::HashSet<String> = ["ok".to_string()].into_iter().collect();
        assert!(def.resolve_runtime_refs(&dir, Some(&filter)).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
