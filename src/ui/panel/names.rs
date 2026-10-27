//! The names a panel binding reads: a dotted path, spelled like the
//! phase's own Python parameters (`tp_worker.py`, parameter injection),
//! resolved in the same order: the builtins, then a phase's key, then a
//! plug's key. There is no expression language: a binding names a value,
//! CSS and the engine's own verdicts do the rest.

use crate::procedure::markers::did_you_mean;
use crate::procedure::schema::UIComponent;

/// What a measurement path may read past its key. The bare key reads
/// the value.
const MEASUREMENT_FIELDS: [&str; 4] = ["outcome", "low", "high", "units"];
const UNIT_FIELDS: [&str; 4] = [
    "serial_number",
    "part_number",
    "revision_number",
    "batch_number",
];
const RUN_PATHS: [&str; 6] = [
    "status",
    "operated_by",
    "procedure.name",
    "procedure.version",
    "slot.key",
    "slot.name",
];
const PHASE_FIELDS: [&str; 2] = ["key", "name"];
pub(super) const BUILTINS: [&str; 5] = ["ui", "measurements", "unit", "run", "phase"];

/// Everything a panel may name.
#[derive(Clone)]
pub struct Names<'a> {
    /// The procedure's root panel, shown for the whole run: no
    /// components of its own, `measurements` and `phase` are the phase
    /// running now.
    pub procedure: bool,
    pub components: &'a [UIComponent],
    /// The phase's declared measurement keys; for the procedure's panel,
    /// every phase's.
    pub measurements: Vec<&'a str>,
    /// The procedure's phases: key, declared measurement keys.
    pub phases: Vec<(&'a str, Vec<&'a str>)>,
    pub plugs: Vec<&'a str>,
    /// The phase's Python source, when the station can read it: a
    /// `ui.<key>` no component declares is a value the phase writes
    /// (`ui.<key> = v`), and the source says whether it ever does.
    pub source: Option<String>,
}

/// Why a path does not resolve. A warning still binds: the value stays
/// empty until something by that name exists.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    Error(String),
    Warning(String),
}

/// `ui.armed` → `["ui", "armed"]`; `None` unless every segment is an
/// identifier.
pub fn split(path: &str) -> Option<Vec<&str>> {
    let segments: Vec<&str> = path.trim().split('.').collect();
    segments
        .iter()
        .all(|s| is_identifier(s))
        .then_some(segments)
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Names<'_> {
    /// The component a `ui.<key>` path names.
    pub fn component(&self, key: &str) -> Option<&UIComponent> {
        self.components.iter().find(|c| c.key == key)
    }

    fn declared_components(&self) -> String {
        if self.components.is_empty() {
            "the phase declares no `ui.components`".to_string()
        } else {
            let keys: Vec<&str> = self.components.iter().map(|c| c.key.as_str()).collect();
            format!("declared: {}", keys.join(", "))
        }
    }

    pub fn check(&self, path: &str) -> Result<(), Problem> {
        let Some(segments) = split(path) else {
            return Err(Problem::Error(format!(
                "`{path}` is not a name: write a dotted path such as `ui.armed` or `measurements.voltage` (no operators, no quotes)"
            )));
        };
        let root = segments[0];
        let rest = &segments[1..];
        match root {
            "ui" if self.procedure => Err(Problem::Error(format!(
                "`{path}`: the procedure's panel has no `ui` values; a phase's screen shows in its <slot>"
            ))),
            "ui" => match rest {
                [key] if self.component(key).is_some() => Ok(()),
                // A key no component declares: a value the phase writes
                // for the panel alone (`ui.<key> = v`).
                [key] => match &self.source {
                    Some(source) if !writes_ui(source, key) => Err(Problem::Warning(format!(
                        "`{path}`: no component `{key}` ({}), and the phase never writes `ui.{key} = ...`; the binding stays empty{}",
                        self.declared_components(),
                        did_you_mean(key, self.components.iter().map(|c| c.key.as_str()))
                    ))),
                    _ => Ok(()),
                },
                _ => Err(Problem::Error(format!("`{path}`: write `ui.<key>`"))),
            },
            "measurements" => {
                let owner = if self.procedure { "no phase" } else { "the phase" };
                self.measurement(path, rest, &self.measurements, owner)
            }
            "unit" => fields(path, rest, &UNIT_FIELDS),
            "phase" => fields(path, rest, &PHASE_FIELDS),
            "run" => {
                let joined = rest.join(".");
                if RUN_PATHS.contains(&joined.as_str()) {
                    Ok(())
                } else {
                    Err(Problem::Error(format!(
                        "`{path}`: `run` has {}{}",
                        RUN_PATHS.join(", "),
                        did_you_mean(&joined, RUN_PATHS.into_iter())
                    )))
                }
            }
            _ => {
                if let Some((_, measurements)) = self.phases.iter().find(|(k, _)| *k == root) {
                    return match rest {
                        ["measurements", tail @ ..] => {
                            self.measurement(path, tail, measurements, &format!("phase `{root}`"))
                        }
                        _ => Err(Problem::Error(format!(
                            "`{path}`: a phase reads as `{root}.measurements.<key>`"
                        ))),
                    };
                }
                if self.plugs.contains(&root) {
                    return match rest {
                        ["status"] => Ok(()),
                        _ => Err(Problem::Error(format!(
                            "`{path}`: a plug reads as `{root}.status`"
                        ))),
                    };
                }
                let known = BUILTINS
                    .into_iter()
                    .chain(self.phases.iter().map(|(k, _)| *k))
                    .chain(self.plugs.iter().copied());
                Err(Problem::Error(format!(
                    "`{path}`: unknown name `{root}`: a binding reads ui, measurements, unit, run, phase, a phase key or a plug key{}",
                    did_you_mean(root, known)
                )))
            }
        }
    }

    fn measurement(
        &self,
        path: &str,
        rest: &[&str],
        declared: &[&str],
        owner: &str,
    ) -> Result<(), Problem> {
        let (key, field) = match rest {
            [key] => (*key, None),
            [key, field] => (*key, Some(*field)),
            _ => {
                return Err(Problem::Error(format!(
                    "`{path}`: write `measurements.<key>` or `measurements.<key>.{}`",
                    MEASUREMENT_FIELDS.join("|")
                )))
            }
        };
        if let Some(field) = field.filter(|f| !MEASUREMENT_FIELDS.contains(f)) {
            return Err(Problem::Error(format!(
                "`{path}`: a measurement has {}{}",
                MEASUREMENT_FIELDS.join(", "),
                did_you_mean(field, MEASUREMENT_FIELDS.into_iter())
            )));
        }
        if declared.contains(&key) {
            return Ok(());
        }
        // Phase code may record a measurement the YAML does not declare.
        Err(Problem::Warning(format!(
            "`{path}`: {owner} declares no measurement `{key}`; the binding stays empty until one by that name is recorded{}",
            did_you_mean(key, declared.iter().copied())
        )))
    }
}

fn fields(path: &str, rest: &[&str], allowed: &[&str]) -> Result<(), Problem> {
    match rest {
        [field] if allowed.contains(field) => Ok(()),
        _ => {
            let root = path.split('.').next().unwrap_or_default();
            let written = rest.join(".");
            Err(Problem::Error(format!(
                "`{path}`: `{root}` has {}{}",
                allowed.join(", "),
                did_you_mean(&written, allowed.iter().copied())
            )))
        }
    }
}

/// Whether `source` assigns `ui.<key>` somewhere (`ui.key = v` or
/// `ui.key += v`, not `ui.key == v`). A text search: enough to tell a typo in the panel from
/// a value the phase writes.
fn writes_ui(source: &str, key: &str) -> bool {
    let needle = format!("ui.{key}");
    source.match_indices(&needle).any(|(at, _)| {
        let before = source[..at].chars().next_back();
        let after = source[at + needle.len()..].trim_start_matches([' ', '\t']);
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && (after.starts_with('=') && !after.starts_with("==")
                || ["+=", "-=", "*=", "/=", "//=", "%=", "**=", "|=", "&="]
                    .iter()
                    .any(|op| after.starts_with(op)))
    })
}
