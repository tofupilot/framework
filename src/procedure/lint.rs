//! The procedure lint: loader refusals, reference findings and deprecation
//! warnings as one list, with a stable `code` and a position on each.
//!
//! One list for every surface — `tofupilot validate`, Studio's Issues
//! panel and the assistant's guardrail through `lint`, the run's start
//! gate and `deploy` through `references` — so they cannot report
//! different rules for the same file.

use std::collections::HashSet;
use std::path::Path;

use super::loader::{location_path, LoadError, LoadOutcome};
use super::schema::{ProcedureDefinition, RefLocation, RefProblem, RefSeverity};
use super::yaml::{Positions, Warning};

use super::codes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable machine key (`plug_class_not_found`, `parse_error`…).
    pub code: &'static str,
    /// The sentence as a person reads it, entity included
    /// ("Plug `psu`: class `Psu` not found in plugs/psu.py").
    pub message: String,
    /// YAML path of the node (`plugs[0].python`), when known.
    pub yaml_path: Option<String>,
    pub location: Option<RefLocation>,
    /// 1-based position in the procedure file.
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The replacement the message suggests (`plugs.psu:PowerSupply`),
    /// for a tool that wants to apply it rather than read the sentence.
    pub hint: Option<String>,
    /// The message without the entity or path `render` already states.
    detail: String,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The message without the entity or YAML path the position states.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// `file:line:column`, leaving out what is unknown.
    pub fn position(&self, file: &str) -> String {
        match (self.line, self.column) {
            (Some(line), Some(column)) => format!("{file}:{line}:{column}"),
            (Some(line), None) => format!("{file}:{line}"),
            _ => file.to_string(),
        }
    }

    /// The finding in one sentence, led by the YAML path it is about:
    /// `plugs[0].python: class `Psu` not found in plugs/psu.py`.
    pub fn summary(&self) -> String {
        match &self.yaml_path {
            Some(path) if !path.is_empty() => format!("{path}: {}", self.detail),
            _ => self.detail.clone(),
        }
    }

    /// Compiler-style line: `procedure.yaml:7:5: plugs[0].python: ...`,
    /// leaving out what is unknown.
    pub fn render(&self, file: &str) -> String {
        format!("{}: {}", self.position(file), self.summary())
    }

    pub fn from_load_error(e: LoadError) -> Self {
        let detail = e.message.clone();
        Self {
            // Not checked is not broken: the file may well be right.
            severity: if e.code == codes::NOT_CHECKED {
                Severity::Warning
            } else {
                Severity::Error
            },
            code: e.code,
            hint: suggestion(&e.message),
            message: e.detail(),
            yaml_path: e.path,
            location: e.location,
            line: e.line,
            column: e.column,
            detail,
        }
    }

    pub fn from_problem(p: RefProblem, positions: &Positions) -> Self {
        let path = location_path(&p.location);
        let position = positions.locate(&path);
        Self {
            severity: match p.severity {
                RefSeverity::Error => Severity::Error,
                RefSeverity::Warning => Severity::Warning,
            },
            code: p.code,
            // The fix belongs in the sequence file, not at the `uses:`
            // the problem is located at: the message names it.
            // A missing parameter's did-you-mean is a rename in the
            // Python file, not a value for this YAML path.
            hint: p.hint.clone().or_else(|| {
                (!p.at_sequence_call() && p.code != codes::PHASE_MISSING_PARAM)
                    .then(|| suggestion(&p.message))
                    .flatten()
            }),
            detail: p.detail().to_string(),
            message: p.message,
            yaml_path: Some(path),
            location: Some(p.location),
            line: position.map(|(line, _)| line),
            column: position.map(|(_, column)| column),
        }
    }

    pub fn from_warning(w: Warning) -> Self {
        // A warning's text leads with the YAML path it is about.
        let (yaml_path, detail) = match super::yaml::split_path(&w.detail) {
            Some((path, rest)) => (Some(path.to_string()), rest.to_string()),
            None => (None, w.detail.clone()),
        };
        Self {
            severity: Severity::Warning,
            code: w.code,
            hint: suggestion(&w.detail),
            detail,
            message: w.message,
            yaml_path,
            location: None,
            line: Some(w.line),
            column: Some(w.column),
        }
    }

    /// A finding about a file other than the procedure YAML (a Python
    /// source): positioned in that file, `render` names it.
    pub fn in_source(
        code: &'static str,
        message: String,
        line: Option<u32>,
        column: Option<u32>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code,
            hint: None,
            detail: message.clone(),
            message,
            yaml_path: None,
            location: None,
            line,
            column,
        }
    }
}

/// What `lint` found, and the definition when the file loaded.
pub struct Lint {
    /// What the loader built, kept when it also refused something so the
    /// references can still be judged: `loaded` says whether a run would
    /// start from it.
    pub definition: Option<ProcedureDefinition>,
    /// The file loads: a definition and no refusal.
    pub loaded: bool,
    pub diagnostics: Vec<Diagnostic>,
}

/// The whole lint of a procedure text: loader refusals, then the
/// reference check against `procedure_dir` (only when the file loaded:
/// a definition that did not parse has no references to judge), then the
/// deprecation warnings, sorted by position. `text` is the procedure
/// source, for positions and the warnings the loader does not raise
/// itself (the old plug `config:`).
pub fn lint(outcome: LoadOutcome, procedure_dir: &Path, text: Option<&str>) -> Lint {
    let loaded = outcome.definition.is_some() && outcome.errors.is_empty();
    // A text that is not valid YAML has no keys to warn about: what a
    // warning would read past the broken line is the parser's confusion.
    let parsed = !outcome.errors.iter().any(|e| e.is_parse_error());
    let mut diagnostics: Vec<Diagnostic> = outcome
        .errors
        .into_iter()
        .map(Diagnostic::from_load_error)
        .collect();
    if let Some(def) = outcome.definition.as_ref() {
        diagnostics.extend(references(def, procedure_dir, text, None));
    }
    let mut warnings = if parsed { outcome.warnings } else { Vec::new() };
    if let Some(text) = text.filter(|_| parsed) {
        for w in super::yaml::warnings(text) {
            if !warnings
                .iter()
                .any(|x| (x.line, x.column, &x.message) == (w.line, w.column, &w.message))
            {
                warnings.push(w);
            }
        }
        warnings.sort_by_key(|w| (w.line, w.column));
    }
    diagnostics.extend(warnings.into_iter().map(Diagnostic::from_warning));
    sort(&mut diagnostics);
    Lint {
        definition: outcome.definition,
        loaded,
        diagnostics,
    }
}

/// The reference check of a loaded definition, positioned in `text`. The
/// run's start gate and `deploy` call it on the definition they already
/// hold (a run's is narrowed to `main_filter` and altered by its flags
/// first); `lint` calls it for everyone else.
pub fn references(
    def: &ProcedureDefinition,
    procedure_dir: &Path,
    text: Option<&str>,
    main_filter: Option<&HashSet<String>>,
) -> Vec<Diagnostic> {
    let positions = text.map(super::yaml::positions).unwrap_or_default();
    let mut diagnostics: Vec<Diagnostic> = def
        .resolve_runtime_refs(procedure_dir, main_filter)
        .into_iter()
        .map(|p| Diagnostic::from_problem(p, &positions))
        .collect();
    sort(&mut diagnostics);
    diagnostics
}

/// In file order, as compilers list them, an error before a warning on one
/// line; an error the file cannot place first (it may stop everything), a
/// warning it cannot place last.
fn sort(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by_key(|d| {
        let band = match (d.line, d.is_error()) {
            (None, true) => 0,
            (Some(_), _) => 1,
            (None, false) => 2,
        };
        (band, d.line, d.column, !d.is_error())
    });
}

/// The replacement a message proposes: its "did you mean `X`?" or its
/// "write `X`".
fn suggestion(message: &str) -> Option<String> {
    let (_, rest) = message
        .rsplit_once("did you mean `")
        .or_else(|| message.rsplit_once("; write `"))?;
    let (value, _) = rest.split_once('`')?;
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::loader::inspect_procedure_definition_at;
    use crate::procedure::FsSequenceLocator;

    fn lint_text(dir: &Path, text: &str) -> Lint {
        let outcome = inspect_procedure_definition_at(text, Some(dir), &FsSequenceLocator);
        lint(outcome, dir, Some(text))
    }

    #[test]
    fn a_dangling_plug_reference_is_coded_and_positioned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plugs")).unwrap();
        std::fs::write(
            dir.path().join("plugs/psu.py"),
            "class PowerSupply:\n    pass\n",
        )
        .unwrap();
        let text = "name: t\nplugs:\n  - key: psu\n    name: PSU\n    python: plugs.psu.PowerSupply\nmain:\n  - key: a\n    name: A\n    python: phases/a.py:run\n";
        let lint = lint_text(dir.path(), text);
        let plug = lint
            .diagnostics
            .iter()
            .find(|d| d.yaml_path.as_deref() == Some("plugs[0].python"))
            .unwrap_or_else(|| panic!("plug finding in {:#?}", lint.diagnostics));
        assert_eq!(plug.code, "plug_python_invalid");
        assert_eq!(plug.hint.as_deref(), Some("plugs.psu:PowerSupply"));
        assert_eq!(plug.line, Some(5));
        assert!(plug
            .render("procedure.yaml")
            .starts_with("procedure.yaml:5:"));
        let phase = lint
            .diagnostics
            .iter()
            .find(|d| d.yaml_path.as_deref() == Some("main[0].python"))
            .expect("phase finding");
        assert_eq!(phase.code, "phase_file_not_found");
    }

    #[test]
    fn a_parse_error_is_alone_and_coded() {
        let dir = tempfile::tempdir().unwrap();
        let lint = lint_text(dir.path(), "name: [unclosed\n");
        assert!(lint.definition.is_none());
        assert_eq!(lint.diagnostics.len(), 1);
        assert_eq!(lint.diagnostics[0].code, codes::PARSE_ERROR);
    }

    #[test]
    fn suggestion_reads_the_last_did_you_mean() {
        assert_eq!(suggestion("x; did you mean `a:b`?").as_deref(), Some("a:b"));
        assert_eq!(suggestion("no hint"), None);
    }
}
