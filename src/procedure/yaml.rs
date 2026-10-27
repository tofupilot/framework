//! The one place `procedure.yaml` text is parsed, so the loader and every
//! test read YAML with the same rules.
//!
//! The options pin what procedures relied on under `serde_yaml` (replaced
//! for RUSTSEC-2024-0449, TP-1269); `tests/procedure_yaml_parsing.rs`
//! holds each one in place.

use serde::de::DeserializeOwned;
use serde_saphyr::granit_parser::{Event, Parser, ScalarStyle};
use serde_saphyr::options::{DuplicateKeyPolicy, MergeKeyPolicy};
use serde_saphyr::MessageFormatter;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// A parse failure, positioned in the text the caller passed (1-based),
/// with the YAML path of the node it is about when the parser got that far.
#[derive(Debug, Clone)]
pub struct Error {
    message: String,
    /// `codes::MISSING_FIELD`, `UNKNOWN_FIELD`, `UNKNOWN_VALUE` or
    /// `PARSE_ERROR`, read off the parser's own wording.
    code: &'static str,
    path: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
}

impl Error {
    pub fn line(&self) -> Option<u32> {
        self.line
    }

    pub fn column(&self) -> Option<u32> {
        self.column
    }

    /// `main[0].measurements[1].validators[0].operator`; `None` at the root.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// What is wrong, without path or position.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{path}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Where each node of the parsed text starts (1-based line and column in
/// the text the caller passed), by YAML path: `main[2].depends_on` is the
/// `depends_on` key of the third main phase, `plugs[0]` the entry itself.
/// What turns a post-parse refusal into a `file:line:col` position.
#[derive(Debug, Clone, Default)]
pub struct Positions {
    by_path: HashMap<String, (u32, u32)>,
    by_position: BTreeMap<(u32, u32), String>,
}

impl Positions {
    /// The position of `path`, or of its closest written ancestor: a
    /// derived `key` that the file never wrote points at its entry.
    pub fn locate(&self, path: &str) -> Option<(u32, u32)> {
        let mut path = path;
        loop {
            if let Some(at) = self.by_path.get(path) {
                return Some(*at);
            }
            path = parent_path(path)?;
        }
    }

    /// The position of `path` itself, `None` when the text does not write
    /// it.
    pub fn exact(&self, path: &str) -> Option<(u32, u32)> {
        self.by_path.get(path).copied()
    }

    /// The path of the node at, or closest before, `(line, column)`.
    fn path_at(&self, line: u32, column: u32) -> Option<&str> {
        self.by_position
            .range(..=(line, column))
            .next_back()
            .map(|(_, path)| path.as_str())
    }
}

/// Where each node of `text` starts, without parsing it into anything:
/// for positioning findings on a procedure that already loaded.
pub fn positions(text: &str) -> Positions {
    let repaired = repair(strip_bom(text));
    let prepared = prepare(&repaired.text);
    positions_of(&repaired, &prepared)
}

fn positions_of(repaired: &Repaired<'_>, prepared: &Prepared<'_>) -> Positions {
    let mut positions = Positions::default();
    for (line, column, path) in &prepared.nodes {
        let at = (
            *line as u32,
            repaired.original_column(*line, *column) as u32,
        );
        positions.by_path.entry(path.clone()).or_insert(at);
        // A collection and its first key start at the same place: the
        // collection is what a whole-node refusal is about.
        positions
            .by_position
            .entry(at)
            .or_insert_with(|| path.clone());
    }
    positions
}

/// `a.b[2].c` → `a.b[2]` → `a.b` → `a` → `None`.
fn parent_path(path: &str) -> Option<&str> {
    let cut = path.rfind(['.', '['])?;
    Some(&path[..cut])
}

impl std::error::Error for Error {}

/// Something that loads today but will be refused by a future version,
/// positioned in the text the caller passed (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub line: u32,
    pub column: u32,
    /// `detail` and `DEPRECATION_NOTICE`: what Studio shows and the
    /// uploaded run logs carry, where the notice is searched for.
    pub message: String,
    /// What is wrong and the fix, without position or notice.
    pub detail: String,
    /// `codes::DEPRECATED`, or the kind of likely mistake a notice is.
    pub code: &'static str,
}

impl Warning {
    fn new(line: u32, column: u32, detail: String) -> Self {
        Self {
            line,
            column,
            code: super::codes::DEPRECATED,
            message: format!("{detail}. {DEPRECATION_NOTICE}"),
            detail,
        }
    }

    /// A deprecation found after parsing (a spelling the loader still
    /// reads), positioned like the parser's own.
    pub(crate) fn deprecation(line: u32, column: u32, detail: String) -> Self {
        Self::new(line, column, detail)
    }

    /// A likely mistake that is no deprecation: the message is the detail
    /// alone, without `DEPRECATION_NOTICE`.
    pub(crate) fn notice(line: u32, column: u32, code: &'static str, detail: String) -> Self {
        Self {
            line,
            column,
            code,
            message: detail.clone(),
            detail,
        }
    }

    /// Compiler-style console line: `procedure.yaml:4:10: {detail}`.
    pub fn render(&self, file: &str) -> String {
        format!("{file}:{}:{}: {}", self.line, self.column, self.detail)
    }
}

/// The sentence every deprecation warning carries, so the runs that
/// uploaded one can be found later in the run logs.
pub const DEPRECATION_NOTICE: &str = "A future version of TofuPilot will refuse it.";

fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        // `yes`/`no`/`on`/`off` stay text (YAML 1.2), as they always were.
        strict_booleans: true,
        // `<<:` is not part of the procedure format.
        merge_keys: MergeKeyPolicy::Error,
        // `LastWins` would buffer every struct and lose every error
        // position to the start of the enclosing phase.
        duplicate_keys: DuplicateKeyPolicy::Error,
        // `.inf`/`.nan` keep loading during the deprecation (see `prepare`).
        reject_non_finite_typeless_float: false,
        // Default budget except the alias/anchor ratio check, which refuses
        // one block reused 100 times (a shared validator list) that
        // serde_yaml loaded. Alias bombs stay refused by the replay limits.
        budget: serde_saphyr::budget! { enforce_alias_anchor_ratio: false },
        // Comments are never read. Emitting them buffers the ones before a
        // block's first entry, and 33 of them (a commented-out first phase)
        // fail a load that serde_yaml accepted.
        emit_comments: false,
        with_snippet: false,
    }
}

pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    from_str_with_warnings(text).map(|(value, _)| value)
}

pub fn from_str_with_warnings<T: DeserializeOwned>(text: &str) -> Result<(T, Vec<Warning>), Error> {
    from_str_located(text).map(|(value, warnings, _)| (value, warnings))
}

/// `from_str_with_warnings`, plus where every node of the text starts.
pub fn from_str_located<T: DeserializeOwned>(
    text: &str,
) -> Result<(T, Vec<Warning>, Positions), Error> {
    let repaired = repair(strip_bom(text));
    let prepared = prepare(&repaired.text);
    let original = |(line, column): (usize, usize)| {
        (line as u32, repaired.original_column(line, column) as u32)
    };
    let positions = positions_of(&repaired, &prepared);
    // A syntax error is reported before anything the deserializer would
    // say: it stops reading a mapping at the line that broke it, and would
    // name a field "missing" that sits just below a mis-indented line.
    // Reading the whole text as `IgnoredAny` reaches the error through
    // the same formatting as any other.
    let read = if prepared.syntax_error {
        serde_saphyr::from_str_with_options::<serde::de::IgnoredAny>(&prepared.text, options())
            .and_then(|_| serde_saphyr::from_str_with_options(&prepared.text, options()))
    } else {
        serde_saphyr::from_str_with_options(&prepared.text, options())
    };
    match read {
        Ok(value) => Ok((value, all_warnings(&repaired, &prepared), positions)),
        Err(err) => {
            // The user formatter: the default one advises the reader to change
            // parser options or Rust types, which a procedure author (and the
            // Studio assistant, which receives these messages) cannot do.
            let message = serde_saphyr::UserMessageFormatter
                .format_message(&err)
                .into_owned();
            // In the repaired text, where `prepare` recorded its keys.
            let at = err.location().map(|l| {
                let line = l.line() as usize;
                (line, prepared.original_column(line, l.column() as usize))
            });
            // serde-saphyr places a missing field on the last key read in its
            // mapping, which can be far down a long phase: point at the
            // mapping and name its path instead, as serde_yaml did.
            let owner = match (&err, at) {
                (serde_saphyr::Error::SerdeMissingField { .. }, Some(at)) => {
                    prepared.key_owner.get(&at)
                }
                _ => None,
            };
            Err(match owner {
                Some((line, column, path)) => {
                    let (line, column) = original((*line, *column));
                    Error {
                        code: parse_code(&message),
                        message: user_message(&message),
                        path: (!path.is_empty()).then(|| path.clone()),
                        line: Some(line),
                        column: Some(column),
                    }
                }
                None => {
                    let at = at.map(original);
                    let path = at
                        .and_then(|(line, column)| positions.path_at(line, column))
                        .map(str::to_string);
                    let raised_on = path.clone();
                    let code = parse_code(&message);
                    let (path, message) = refine_path(path, user_message(&message));
                    // A child named by the refusal has a line of its own.
                    let at = match &path {
                        Some(p) if path != raised_on => positions.locate(p).or(at),
                        _ => at,
                    };
                    Error {
                        code,
                        message,
                        path,
                        line: at.map(|at| at.0),
                        column: at.map(|at| at.1),
                    }
                }
            })
        }
    }
}

/// The node a refusal is about, from the node it was raised on: an unknown
/// field is raised on its mapping, and our own refusals name the child
/// they are about (`validators[0]: ...`).
fn refine_path(path: Option<String>, message: String) -> (Option<String>, String) {
    let join = |path: Option<String>, child: &str| match path {
        Some(path) if path.is_empty() => child.to_string(),
        Some(path) => format!("{path}.{child}"),
        None => child.to_string(),
    };
    if let Some(field) = message
        .strip_prefix("unknown field `")
        .and_then(|rest| rest.split('`').next())
    {
        let named = path
            .as_deref()
            .is_some_and(|p| p == field || p.ends_with(&format!(".{field}")));
        let path = if named { path } else { Some(join(path, field)) };
        return (path, message);
    }
    if let Some((child, rest)) = split_path(&message) {
        let (child, rest) = (child.to_string(), rest.to_string());
        return (Some(join(path, &child)), rest);
    }
    (path, message)
}

/// `("unit.serial_number", "...")` for `"unit.serial_number: ..."`: the
/// leading YAML path a message names, when it starts with one.
pub(crate) fn split_path(text: &str) -> Option<(&str, &str)> {
    let (head, rest) = text.split_once(": ")?;
    let is_path = head.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'));
    is_path.then_some((head, rest))
}

/// The code of a parser refusal, from its wording before `user_message`
/// rewrites it.
fn parse_code(message: &str) -> &'static str {
    use super::codes;
    if message.starts_with("missing field `") {
        codes::MISSING_FIELD
    } else if message.starts_with("unknown field `") {
        codes::UNKNOWN_FIELD
    } else if message.starts_with("unknown variant `") {
        codes::UNKNOWN_VALUE
    } else {
        codes::PARSE_ERROR
    }
}

/// Words other test tools use for a field this one has.
const SYNONYMS: &[(&[&str], &str)] = &[
    (&["run", "command", "cmd", "exec", "shell"], "executable"),
    (&["script", "function", "module", "callable"], "python"),
    (&["limits", "checks", "limit"], "validators"),
    (&["depends", "needs", "after", "requires"], "depends_on"),
];

/// The parser's wording, shortened where it lists or restates what the
/// path already says: `unknown field `nmae`, expected one of key, name,
/// …` becomes "unknown field `nmae`; did you mean `name`?".
fn user_message(message: &str) -> String {
    if let Some(field) = message
        .strip_prefix("missing field `")
        .and_then(|rest| rest.strip_suffix('`'))
    {
        return format!("missing `{field}`");
    }
    match message {
        "expected mapping start" => return "expected a mapping".to_string(),
        "expected sequence start" => return "expected a list".to_string(),
        _ => {}
    }
    // The scanner's account of a line indented to no level above it.
    if message.contains("did not find expected '-' indicator")
        || message.contains("did not find expected key")
    {
        return "indentation does not line up with the lines above".to_string();
    }
    // Reported where the scanner gives up, often one line after the
    // mistake: a value indented under a scalar, or a `key: value` inside
    // a value.
    if message.contains("mapping values are not allowed in this context") {
        return "unexpected `key: value`: check the indentation of this line and the one above (or quote a value holding `: `)".to_string();
    }
    if message.contains("could not find expected ':'") {
        return "a key without its `:`, on this line or the one above".to_string();
    }
    for (prefix, what) in [("unknown field `", "field"), ("unknown variant `", "value")] {
        let Some(rest) = message.strip_prefix(prefix) else {
            continue;
        };
        let Some((got, expected)) = rest.split_once('`') else {
            continue;
        };
        let expected = expected
            .trim_start_matches(", ")
            .trim_start_matches("expected ")
            .trim_start_matches("one of ");
        let candidates: Vec<&str> = expected
            .split(", ")
            .map(|c| c.trim_matches('`'))
            .filter(|c| !c.is_empty())
            .collect();
        // A swapped pair (`=>` for `>=`) before any edit-distance guess.
        let swapped = candidates.iter().copied().find(|c| {
            let mut a: Vec<char> = c.chars().collect();
            let mut b: Vec<char> = got.chars().collect();
            a.sort_unstable();
            b.sort_unstable();
            a == b && *c != got
        });
        // Another tool's word for a field this one has (`run:` for
        // `executable:`), which no edit distance finds.
        let synonym = SYNONYMS
            .iter()
            .find(|(words, _)| words.contains(&got.to_lowercase().as_str()))
            .map(|(_, field)| *field)
            .filter(|field| candidates.contains(field));
        let near = swapped.or(synonym).or_else(|| {
            super::pysource::closest(got, candidates.iter().copied())
                // One edit away from a one-letter value is no clue, nor
                // are two edits on a three-letter word (`run`, `ui`).
                .filter(|near| {
                    let longest = near.chars().count().max(got.chars().count());
                    longest > 2
                        && super::pysource::edit_distance(&got.to_lowercase(), &near.to_lowercase())
                            .is_none_or(|d| d * 2 <= longest)
                })
        });
        return match near {
            Some(near) => format!("unknown {what} `{got}`; did you mean `{near}`?"),
            None if candidates.is_empty() => format!("unknown {what} `{got}`"),
            None => format!(
                "unknown {what} `{got}`; expected one of {}",
                candidates.join(", ")
            ),
        };
    }
    message.to_string()
}

/// A scalar in value position, as `rewrite_value_scalars` hands it to its
/// caller.
pub struct ValueScalar<'a> {
    /// The scalar's value, quotes and escapes resolved.
    pub value: &'a str,
    /// Its dotted path (`main[1].retry.limit`), spelled as the warnings do.
    pub path: String,
    /// 1-based position of the scalar in the text (its opening quote for a
    /// quoted one), the same convention as `Warning` and `Error`.
    pub line: usize,
    pub column: usize,
}

/// Why `rewrite_value_scalars` could not rewrite a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteError {
    /// The raw text does not parse: the typed read reports the error. The
    /// raw parser also refuses the two layouts `repair` accepts with a
    /// deprecation warning, so a caller that must not read such a text
    /// checks `warnings` first.
    Parse,
    /// The scalar at this 1-based line has no span the rewriter can stand
    /// in for, or its replacement holds a line break.
    Scalar { line: usize },
}

/// `text` with every value scalar `rewrite` returns a replacement for
/// rewritten in place, so everything else keeps its spelling and its line:
/// a replacement never holds a line break, and one that stands for a
/// scalar spanning several lines (a `|` or `>` block, a folded plain or
/// quoted scalar) is followed by as many blank lines as the scalar held.
/// Keys, anchors and the structure are untouched; a replacement must be a
/// valid flow scalar or flow collection.
pub fn rewrite_value_scalars(
    text: &str,
    mut rewrite: impl FnMut(&ValueScalar<'_>) -> Option<String>,
) -> Result<String, RewriteError> {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let parser_options = serde_saphyr::granit_parser::options! { emit_comments: false };
    for item in Parser::new_from_str_with_options(text, parser_options) {
        let (event, span) = item.map_err(|_| RewriteError::Parse)?;
        match event {
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                starts_value(&mut stack, "?");
                stack.push(match event {
                    Event::MappingStart(..) => Frame::Map {
                        key: None,
                        expect_key: true,
                    },
                    _ => Frame::Seq { next: 0 },
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                stack.pop();
                value_done(&mut stack);
            }
            Event::Alias(_) => {
                if starts_value(&mut stack, "*") {
                    value_done(&mut stack);
                }
            }
            Event::Scalar(value, style, _, _) if starts_value(&mut stack, &value) => {
                let scalar = ValueScalar {
                    value: &value,
                    path: display(&path_of(&stack)),
                    line: span.start.line(),
                    column: span.start.col() + 1,
                };
                if let Some(replacement) = rewrite(&scalar) {
                    let failed = RewriteError::Scalar { line: scalar.line };
                    if replacement.contains('\n') {
                        return Err(failed);
                    }
                    let range = span.byte_range().ok_or(failed)?;
                    let source = text.get(range.clone()).ok_or(failed)?;
                    // The span of a quoted scalar may stop at its content:
                    // take the quotes along, so the replacement stands
                    // on its own. A block scalar's span covers its
                    // content lines: the rewrite starts at the `|` or `>`
                    // indicator and stops before the next line's
                    // indentation.
                    let quoted_around = |open: u8| {
                        range.start > 0
                            && text.as_bytes()[range.start - 1] == open
                            && text.as_bytes().get(range.end) == Some(&open)
                    };
                    let folded_plain = source.split_whitespace().collect::<Vec<_>>().join(" ")
                        == value.split_whitespace().collect::<Vec<_>>().join(" ");
                    let block = matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
                    let (start, end) = if block {
                        // The header line holds the indicator, maybe a
                        // comment after it (`| # note`): the comment goes
                        // with the replacement.
                        let before = text[..range.start].trim_end_matches([' ', '\t', '\n', '\r']);
                        let header_start = before.rfind(['\n', '\r']).map_or(0, |at| at + 1);
                        let header = &before[header_start..];
                        // YAML wants any whitespace before `#`.
                        let header_end = header
                            .match_indices('#')
                            .find(|(at, _)| {
                                *at > 0 && matches!(header.as_bytes()[at - 1], b' ' | b'\t')
                            })
                            .map_or(before.len(), |(at, _)| header_start + at);
                        let head = text[..header_end].trim_end_matches([' ', '\t']);
                        let indicator = head
                            .trim_end_matches(|c: char| {
                                matches!(c, '|' | '>' | '-' | '+') || c.is_ascii_digit()
                            })
                            .len();
                        if indicator == head.len() {
                            return Err(failed);
                        }
                        let trailing = source.len() - source.trim_end_matches([' ', '\t']).len();
                        (indicator, range.end - trailing)
                    } else if source == value.as_ref()
                        || source.starts_with(['"', '\''])
                        || folded_plain
                    {
                        (range.start, range.end)
                    } else if quoted_around(b'"') || quoted_around(b'\'') {
                        (range.start - 1, range.end + 1)
                    } else {
                        return Err(failed);
                    };
                    // A multi-line scalar keeps its line count: the lines
                    // after it keep their numbers for every diagnostic.
                    let breaks = text[start..end].matches('\n').count();
                    let mut padded = replacement;
                    padded.extend(std::iter::repeat_n('\n', breaks));
                    edits.push((start, end, padded));
                }
                value_done(&mut stack);
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return Ok(text.to_string());
    }
    edits.sort_by_key(|edit| edit.0);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, end, replacement) in edits {
        if start < copied {
            // Two scalars sharing text: nothing the parser emits, kept as a
            // guard rather than a corrupted text.
            return Err(RewriteError::Parse);
        }
        out.push_str(&text[copied..start]);
        out.push_str(&replacement);
        copied = end;
    }
    out.push_str(&text[copied..]);
    Ok(out)
}

/// The deprecation warnings of `text`, without parsing it into anything.
pub fn warnings(text: &str) -> Vec<Warning> {
    let mut warnings = parser_warnings(text);
    // The old spellings of `with:` and of the slot markers, and a secret
    // put in `env:`: found on the parsed text, reported after the parser's.
    warnings.extend(super::value_checks::text_warnings(
        text,
        super::value_checks::FileKind::Procedure,
    ));
    warnings
}

/// The parser's own deprecation warnings of `text` (the repairs and
/// `prepare`), without the procedure-level ones `warnings` adds: what a
/// sequence file is held to.
pub(crate) fn parser_warnings(text: &str) -> Vec<Warning> {
    let repaired = repair(strip_bom(text));
    let prepared = prepare(&repaired.text);
    all_warnings(&repaired, &prepared)
}

/// The repairs' warnings and `prepare`'s, in the original text's columns,
/// in reading order.
fn all_warnings(repaired: &Repaired<'_>, prepared: &Prepared<'_>) -> Vec<Warning> {
    let mut warnings: Vec<Warning> = repaired
        .warnings
        .iter()
        .cloned()
        .chain(prepared.warnings.iter().map(|w| Warning {
            column: repaired.original_column(w.line as usize, w.column as usize) as u32,
            ..w.clone()
        }))
        .collect();
    warnings.sort_by_key(|w| (w.line, w.column));
    warnings
}

/// `text` with the two forms serde_yaml (libyaml) accepted and serde-saphyr's
/// scanner refuses repaired, each with a deprecation warning:
///
/// - a quoted text continued on a line indented no deeper than its key:
///   the line is re-indented with spaces, first to the previous line's
///   indentation plus two, then one column at a time, until the scanner
///   accepts it.
///   Folding drops a quoted continuation's leading whitespace, so the value
///   does not change.
/// - a comment touching the closing quote, bracket or block header before
///   it (`"V"# volts`): a space is inserted before the `#`.
///
/// Lines never change. `original_column` undoes the inserted characters.
struct Repaired<'a> {
    text: Cow<'a, str>,
    /// (line, 1-based column in `text`) of each inserted character.
    added: Vec<(usize, usize)>,
    warnings: Vec<Warning>,
}

impl Repaired<'_> {
    fn original_column(&self, line: usize, column: usize) -> usize {
        column
            - self
                .added
                .iter()
                .filter(|&&(l, c)| l == line && c < column)
                .count()
    }
}

/// Each repair re-reads the whole (small) file, so the work is bounded: by
/// repaired lines (an unclosed quote would otherwise "repair" every line
/// after it) and by attempts on one line.
const MAX_REPAIRED_LINES: usize = 1_000;
const MAX_ATTEMPTS_PER_LINE: usize = 16;

fn repair(text: &str) -> Repaired<'_> {
    use serde_saphyr::granit_parser::ErrorKind;

    let mut current = Cow::Borrowed(text);
    let mut added: Vec<(usize, usize)> = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();
    // (line, attempts on it so far)
    let mut last: Option<(usize, usize)> = None;
    // (line, is a re-indent) already warned: retries on one line warn once.
    let mut warned: Vec<(usize, bool)> = Vec::new();
    let mut repaired_lines = 0;
    loop {
        let options = serde_saphyr::granit_parser::options! { emit_comments: false };
        let Some(err) = Parser::new_from_str_with_options(&current, options).find_map(Result::err)
        else {
            break;
        };
        let line = err.marker().line();
        let attempts = match last {
            Some((l, n)) if l == line => n + 1,
            _ => 1,
        };
        if attempts == 1 {
            repaired_lines += 1;
        }
        if repaired_lines > MAX_REPAIRED_LINES || attempts > MAX_ATTEMPTS_PER_LINE {
            break;
        }
        last = Some((line, attempts));
        let Some(offset) = err.marker().byte_offset() else {
            break;
        };
        // (inserted characters, their 1-based column, warning column, message)
        let reindent = matches!(
            err.kind(),
            ErrorKind::InvalidQuotedScalarIndent | ErrorKind::TabInIndentation
        );
        let (inserted, insert_at, column, message) = match err.kind() {
            ErrorKind::InvalidQuotedScalarIndent | ErrorKind::TabInIndentation => {
                let Some(start) = indent_start(&current, offset) else {
                    break;
                };
                let indent_text: String = current[start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let indent = indent_text.chars().count();
                // First attempt: straight to the previous line's indentation
                // plus two, which clears a continuation under its key. Then
                // one column at a time. Tabs become spaces.
                let target = if attempts == 1 {
                    (previous_indent(&current, start) + 2).max(indent + 1)
                } else {
                    indent + 1
                };
                let mut text = current.into_owned();
                text.replace_range(start..start + indent_text.len(), &" ".repeat(target));
                current = Cow::Owned(text);
                let message = if indent_text.contains('\t') {
                    "tab in quoted continuation deprecated; indent with spaces".to_string()
                } else {
                    "quoted continuation not indented past its key, deprecated; indent further"
                        .to_string()
                };
                (target - indent, 1, 1, message)
            }
            ErrorKind::CommentNotSeparated => {
                let column = err.marker().col() + 1;
                let mut text = current.into_owned();
                text.insert(offset, ' ');
                current = Cow::Owned(text);
                let message =
                    "comment touches value, deprecated; add a space before `#`".to_string();
                (1, column, column, message)
            }
            // Anything else is a real error: the parse reports it.
            _ => break,
        };
        let column = column
            - added
                .iter()
                .filter(|&&(l, c)| l == line && c < column)
                .count();
        // Characters after the insertion on this line move right.
        for entry in added.iter_mut() {
            if entry.0 == line && entry.1 >= insert_at {
                entry.1 += inserted;
            }
        }
        added.extend((0..inserted).map(|i| (line, insert_at + i)));
        if !warned.contains(&(line, reindent)) {
            warned.push((line, reindent));
            warnings.push(Warning::new(line as u32, column.max(1) as u32, message));
        }
    }
    Repaired {
        text: current,
        added,
        warnings,
    }
}

/// Byte offset where the indentation holding `offset` starts: back over
/// spaces and tabs to the line start. `None` when `offset` is not in a
/// line's indentation. Any line break counts, `\r` alone included.
fn indent_start(text: &str, offset: usize) -> Option<usize> {
    let head = text.get(..offset)?;
    let start = head.trim_end_matches([' ', '\t']).len();
    (start == 0 || head[..start].ends_with(['\n', '\r'])).then_some(start)
}

/// Indentation, in characters, of the last non-blank line before the line
/// starting at `start` (0 when there is none).
fn previous_indent(text: &str, start: usize) -> usize {
    text[..start]
        .split(['\n', '\r'])
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or(0, |line| {
            line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
        })
}

#[cfg(test)]
pub fn to_string<T: serde::Serialize>(value: &T) -> String {
    serde_saphyr::to_string(value).expect("test value serializes")
}

/// Windows editors prefix UTF-8 files with a byte order mark.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// The text actually parsed, after the rewrites that keep serde_yaml's
/// reading of a procedure:
///
/// - an unquoted integer that serde_yaml read as text is quoted, so it
///   stays text: `05` → `"05"`. serde-saphyr refuses `05` as an integer
///   and then accepts it as the float 5.0, so a plug config
///   `gpib_address: 05` or a text default `0042` would silently change.
/// - a non-finite spelling serde_yaml read as text (`-.nan`, `.iNf`,
///   `1e400`) is quoted too.
/// - `.inf`/`.nan` inside a plug's `config:` or a UI component's
///   `default_value` becomes `null`, which is what the plug and the
///   operator form received under serde_yaml, and every `.inf`/`.nan` gets
///   a deprecation warning. Elsewhere they parse as infinity in a number
///   field, and as the text ".inf" in a validator, which judges as the old
///   number did.
///
/// Lines never change. A quote shifts the columns after it on its line,
/// and `original_column` undoes that; `null` is padded to the length of
/// what it replaces.
struct Prepared<'a> {
    text: Cow<'a, str>,
    /// (line, 1-based column in `text`) of each added quote.
    added: Vec<(usize, usize)>,
    warnings: Vec<Warning>,
    /// Each key's (line, 1-based column) to the start and path of the
    /// mapping it belongs to, in the original text.
    key_owner: HashMap<(usize, usize), (usize, usize, String)>,
    /// (line, 1-based column, path) of every key, value and collection
    /// start, in reading order.
    nodes: Vec<(usize, usize, String)>,
    /// The text is not valid YAML: nothing was prepared.
    syntax_error: bool,
}

impl Prepared<'_> {
    fn original_column(&self, line: usize, column: usize) -> usize {
        column
            - self
                .added
                .iter()
                .filter(|&&(l, c)| l == line && c < column)
                .count()
    }
}

#[derive(Clone, PartialEq)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Where the parser is in the document: one frame per open mapping or
/// sequence, holding the key or index of the value being read.
enum Frame {
    Map {
        key: Option<Segment>,
        expect_key: bool,
    },
    Seq {
        next: usize,
    },
}

fn path_of(stack: &[Frame]) -> Vec<Segment> {
    stack
        .iter()
        .filter_map(|frame| match frame {
            Frame::Map { key, .. } => key.clone(),
            Frame::Seq { next } => Some(Segment::Index(*next)),
        })
        .collect()
}

fn display(path: &[Segment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) if out.is_empty() => out.push_str(key),
            Segment::Key(key) => {
                out.push('.');
                out.push_str(key);
            }
            Segment::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    out
}

/// A value has been read: the enclosing mapping expects a key again, the
/// enclosing sequence moves to its next index.
fn value_done(stack: &mut [Frame]) {
    match stack.last_mut() {
        Some(Frame::Map { key, expect_key }) => {
            *key = None;
            *expect_key = true;
        }
        Some(Frame::Seq { next }) => *next += 1,
        None => {}
    }
}

/// The event that starts a value: if it sits where a key is expected, it
/// is a key (a complex one when it is a container), and returns false.
fn starts_value(stack: &mut [Frame], key_text: &str) -> bool {
    if let Some(Frame::Map { key, expect_key }) = stack.last_mut() {
        if *expect_key {
            *key = Some(Segment::Key(key_text.to_string()));
            *expect_key = false;
            return false;
        }
    }
    true
}

enum Edit {
    Quote,
    Null,
}

fn prepare(text: &str) -> Prepared<'_> {
    let unchanged = |syntax_error: bool| Prepared {
        text: Cow::Borrowed(text),
        added: Vec::new(),
        warnings: Vec::new(),
        key_owner: HashMap::new(),
        nodes: Vec::new(),
        syntax_error,
    };
    let mut key_owner = HashMap::new();
    let mut nodes = Vec::new();
    // (line, 1-based column, path) of each open mapping.
    let mut mappings: Vec<(usize, usize, String)> = Vec::new();
    // (byte start, byte end, line, 0-based column, edit), in document order.
    let mut edits = Vec::new();
    let mut warnings = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    // Same comment handling as `options()`: this pass must accept at least
    // what the real parse accepts, or it falls back to the original text
    // and its rewrites are silently lost.
    let parser_options = serde_saphyr::granit_parser::options! { emit_comments: false };
    for item in Parser::new_from_str_with_options(text, parser_options) {
        // The real parse reports the error, at untouched positions.
        let Ok((event, span)) = item else {
            return unchanged(true);
        };
        match event {
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                // In key position this is a complex key: its contents are
                // not a procedure path, `?` stands for it.
                if starts_value(&mut stack, "?") {
                    nodes.push((
                        span.start.line(),
                        span.start.col() + 1,
                        display(&path_of(&stack)),
                    ));
                }
                if matches!(event, Event::MappingStart(..)) {
                    mappings.push((
                        span.start.line(),
                        span.start.col() + 1,
                        display(&path_of(&stack)),
                    ));
                }
                stack.push(match event {
                    Event::MappingStart(..) => Frame::Map {
                        key: None,
                        expect_key: true,
                    },
                    _ => Frame::Seq { next: 0 },
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                if matches!(event, Event::MappingEnd) {
                    mappings.pop();
                }
                stack.pop();
                value_done(&mut stack);
            }
            Event::Alias(_) => {
                if starts_value(&mut stack, "*") {
                    value_done(&mut stack);
                }
            }
            Event::Scalar(value, style, _, tag) => {
                let plain = matches!(style, ScalarStyle::Plain) && tag.is_none();
                // `!!float .inf` changes too (to the text ".inf" in a free-form
                // field) and cannot be nulled in place (`!!float null` does not
                // parse), so it is only warned.
                let float_tagged = matches!(style, ScalarStyle::Plain)
                    && tag
                        .as_ref()
                        .is_some_and(|t| t.is_yaml_core_schema_tag("float"));
                let is_value = starts_value(&mut stack, &value);
                if !is_value {
                    if let Some(owner) = mappings.last() {
                        key_owner.insert((span.start.line(), span.start.col() + 1), owner.clone());
                    }
                }
                let path = path_of(&stack);
                nodes.push((span.start.line(), span.start.col() + 1, display(&path)));
                let mut nulled = false;
                let edit = if !plain {
                    None
                } else if read_as_text_by_serde_yaml(&value) {
                    Some(Edit::Quote)
                } else if is_value && is_non_finite(&value) {
                    // Deliberately not a validator's `expected_value`: there
                    // it stays the text ".inf", whose verdict matches the old
                    // f64 for every operator, where `null` would turn `==`
                    // and `!=` into unset. The dashboard shows it as the
                    // expression `x <= ".inf"`, accepted.
                    (in_plug_config(&path) || is_component_default(&path)).then_some(Edit::Null)
                } else {
                    None
                };
                if let Some(edit) = edit {
                    let Some(range) = span.byte_range() else {
                        return unchanged(false);
                    };
                    // Only a span that is exactly the scalar (not its anchor)
                    // is safe to rewrite.
                    if text.get(range.clone()) == Some(value.as_ref()) {
                        nulled = matches!(edit, Edit::Null);
                        edits.push((
                            range.start,
                            range.end,
                            span.start.line(),
                            span.start.col(),
                            edit,
                        ));
                    }
                }
                // Every spelling of infinity or not-a-number is warned, the
                // unusual ones too: quoted above, they stay text in a
                // free-form field but still load as a float in a number one.
                // A quoted `".inf"` is deliberately not warned: it is legitimate
                // text in a text field, and only a number field reads it as one.
                if (plain || float_tagged) && is_value && is_any_non_finite(&value) {
                    let line = span.start.line();
                    warnings.push(Warning::new(
                        line as u32,
                        (span.start.col() + 1) as u32,
                        non_finite_message(&value, &path, nulled),
                    ));
                }
                if is_value {
                    value_done(&mut stack);
                }
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return Prepared {
            warnings,
            key_owner,
            nodes,
            ..unchanged(false)
        };
    }

    let mut out = String::with_capacity(text.len() + 2 * edits.len());
    let mut added = Vec::new();
    let mut copied = 0;
    let mut shift = (0, 0); // (line, quotes already added on it)
    for (start, end, line, col, edit) in edits {
        out.push_str(&text[copied..start]);
        match edit {
            Edit::Quote => {
                if shift.0 != line {
                    shift = (line, 0);
                }
                let open = col + 1 + shift.1;
                added.push((line, open));
                added.push((line, open + (end - start) + 1));
                shift.1 += 2;
                out.push('"');
                out.push_str(&text[start..end]);
                out.push('"');
            }
            Edit::Null => out.push_str(&format!("{:<width$}", "null", width = end - start)),
        }
        copied = end;
    }
    out.push_str(&text[copied..]);
    Prepared {
        text: Cow::Owned(out),
        added,
        warnings,
        key_owner,
        nodes,
        syntax_error: false,
    }
}

fn in_plug_config(path: &[Segment]) -> bool {
    matches!(
        path,
        [Segment::Key(plugs), Segment::Index(_), Segment::Key(config), ..]
            if plugs == "plugs" && config == "config"
    )
}

/// A UI component's `default_value`: a number or a text, so serde_yaml read
/// `.inf` there as a number the operator form received as `null`.
fn is_component_default(path: &[Segment]) -> bool {
    matches!(
        path,
        [.., Segment::Key(components), Segment::Index(_), Segment::Key(field)]
            if components == "components" && field == "default_value"
    )
}

/// Any case, any sign: the spellings `is_non_finite` misses included.
pub(crate) fn is_any_non_finite(scalar: &str) -> bool {
    let unsigned = scalar.strip_prefix(['+', '-']).unwrap_or(scalar);
    unsigned.eq_ignore_ascii_case(".inf") || unsigned.eq_ignore_ascii_case(".nan")
}

/// YAML's spellings of infinity and not-a-number, as both parsers read them.
fn is_non_finite(scalar: &str) -> bool {
    let unsigned = scalar.strip_prefix(['+', '-']).unwrap_or(scalar);
    matches!(unsigned, ".inf" | ".Inf" | ".INF") || matches!(scalar, ".nan" | ".NaN" | ".NAN")
}

/// `nulled`: the value was rewritten to `null` (plug config, UI default), so
/// the plug or the form gets nothing. Otherwise it parses as text or as a
/// float, depending on the field.
fn non_finite_message(raw: &str, path: &[Segment], nulled: bool) -> String {
    let what = match path.last() {
        _ if nulled && in_plug_config(path) => "plug gets None; write a number or remove the key",
        _ if nulled => "field gets no default; write a number or remove the default",
        Some(Segment::Key(key)) if key == "expected_value" => {
            "limit not a number, validator cannot judge; write a number or remove the validator"
        }
        // A text field keeps the text: quoting it is the whole fix there.
        _ => "write a number, or quote it if it is text",
    };
    format!("{}: `{raw}` deprecated, {what}", display(path))
}

/// A number spelled in a way `serde_yaml` did not read as a number, and
/// serde-saphyr does:
/// - an integer with a redundant leading zero (`05`, `-017`, `0001`), an
///   underscore (`1_000`, `0x4000_0000`), or an uppercase radix prefix
///   (`0X1F`);
/// - a not-a-number with a sign or an unusual case (`-.nan`, `.Nan`) or
///   an infinity in an unusual case (`.iNf`);
/// - a decimal too large for a float (`1e400`), which serde-saphyr reads
///   as infinity.
fn read_as_text_by_serde_yaml(scalar: &str) -> bool {
    if is_any_non_finite(scalar) {
        return !is_non_finite(scalar);
    }
    let body = scalar.strip_prefix(['+', '-']).unwrap_or(scalar);
    let mut chars = body.chars();
    match (chars.next(), chars.next()) {
        (Some('0'), Some(prefix @ ('x' | 'X' | 'o' | 'O' | 'b' | 'B'))) => {
            prefix.is_ascii_uppercase() || body.contains('_')
        }
        (Some(first), _) if first.is_ascii_digit() => {
            let integer = body.chars().all(|c| c.is_ascii_digit() || c == '_');
            (integer && (body.contains('_') || (body.len() > 1 && first == '0')))
                || scalar.parse::<f64>().is_ok_and(f64::is_infinite)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_field_suggests_a_near_spelling_or_this_tool_word_only() {
        let m = |got: &str| {
            user_message(&format!(
                "unknown field `{got}`, expected one of `key`, `name`, `python`, `executable`, `ui`"
            ))
        };
        assert_eq!(m("nmae"), "unknown field `nmae`; did you mean `name`?");
        assert_eq!(m("run"), "unknown field `run`; did you mean `executable`?");
        assert_eq!(
            m("script"),
            "unknown field `script`; did you mean `python`?"
        );
        // Two edits on three letters is no clue.
        assert_eq!(
            m("abc"),
            "unknown field `abc`; expected one of key, name, python, executable, ui"
        );
    }

    #[test]
    fn rewrites_value_scalars_in_place_and_leaves_the_rest() {
        let text = "name: Calib\ninputs:\n  n:\n    default: 3\nmain:\n  - name: Check {inputs.ch}\n    python: phases.ok\n    retry:\n      limit: \"{inputs.n}\"\n    description: '{inputs.ch}'\n    note: 3.10\n    doc: |\n      Channel {inputs.ch}\n      second line\n    folded: Check\n      {inputs.ch} here\n    noted: >- # a comment on the header\n      {inputs.ch}\n    tabbed: >-\t# a tab before it\n      {inputs.ch}\n    after: 1\n";
        let mut seen = Vec::new();
        let out = rewrite_value_scalars(text, |scalar| {
            seen.push((
                scalar.path.clone(),
                scalar.value.to_string(),
                scalar.line,
                scalar.column,
            ));
            if scalar.path.starts_with("inputs") || !scalar.value.contains("{inputs.") {
                return None;
            }
            Some(match scalar.value {
                "{inputs.n}" => "2".to_string(),
                "{inputs.ch}" => "7".to_string(),
                other => serde_json::to_string(&other.replace("{inputs.ch}", "7")).unwrap(),
            })
        })
        .expect("parses");
        // A block or folded scalar is replaced in one line, the lines it held
        // staying blank, so `after:` keeps its line number (17).
        assert_eq!(
            out,
            "name: Calib\ninputs:\n  n:\n    default: 3\nmain:\n  - name: \"Check 7\"\n    python: phases.ok\n    retry:\n      limit: 2\n    description: 7\n    note: 3.10\n    doc: \"Channel 7\\nsecond line\\n\"\n\n\n    folded: \"Check 7 here\"\n\n    noted: 7\n\n    tabbed: 7\n\n    after: 1\n"
        );
        assert_eq!(out.lines().count(), text.lines().count());
        // Keys are never offered; values come with their path and position.
        assert!(seen.iter().any(|s| s
            == &(
                "main[0].retry.limit".to_string(),
                "{inputs.n}".to_string(),
                9,
                14
            )));
        assert!(seen.iter().any(|s| s.0 == "inputs.n.default" && s.1 == "3"));
        assert!(!seen.iter().any(|s| s.1 == "retry" || s.1 == "main"));
        // Nothing to rewrite: the text comes back as it is.
        assert_eq!(
            rewrite_value_scalars("a: 1\n", |_| None).ok().as_deref(),
            Some("a: 1\n")
        );
        assert_eq!(
            rewrite_value_scalars("a: [1\n", |_| None),
            Err(RewriteError::Parse)
        );
        // A replacement that holds a line break names the scalar's line.
        assert_eq!(
            rewrite_value_scalars("a: 1\nb: x\n", |s| (s.value == "x")
                .then(|| "1\n2".to_string())),
            Err(RewriteError::Scalar { line: 2 })
        );
    }

    #[test]
    fn numbers_serde_yaml_read_as_text() {
        for text in [
            "05",
            "00",
            "0123",
            "-017",
            "+05",
            "0001",
            "1_000",
            "-1_000",
            "0X1F",
            "0O17",
            "0B101",
            "+.nan",
            "-.nan",
            ".Nan",
            ".iNf",
            "1e400",
            "-1e400",
            "0x4000_0000",
            "0b1111_0000",
            "0o1_7",
            "-0b1_0",
            "0x_1F",
        ] {
            assert!(read_as_text_by_serde_yaml(text), "{text}");
        }
        // Numbers both parsers agree on, and the canonical non-finite
        // spellings, which `prepare` handles as a deprecation instead.
        for number in [
            "0", "-0", "+0", "5", "-17", "0x1F", "0o17", "0b101", "0.5", "007.5", "05e2", "1e3",
            ".5", "COM3", "", "-", ".inf", "+.inf", "-.Inf", ".INF", ".nan", ".NaN", ".NAN",
        ] {
            assert!(!read_as_text_by_serde_yaml(number), "{number}");
        }
    }

    #[test]
    fn quoting_maps_columns_back() {
        let q = prepare("a: {x: 05, y: 007, z: [1]}\n");
        assert_eq!(q.text, "a: {x: \"05\", y: \"007\", z: [1]}\n");
        // The `[` is column 23 in the original and 27 after four quotes.
        assert_eq!(q.original_column(1, 27), 23);
        // The first value keeps its own column.
        assert_eq!(q.original_column(1, 8), 8);
        assert_eq!(q.original_column(2, 27), 27);
    }

    #[test]
    fn non_finite_values_are_nulled_in_plug_config_only_and_all_warned() {
        let text = "plugs:\n  - name: P\n    config:\n      a: .inf\n      b: [-.inf, .nan]\n      c: '.inf'\nmain:\n  - name: M\n    max: .inf\n";
        let p = prepare(text);
        assert_eq!(
            p.text,
            "plugs:\n  - name: P\n    config:\n      a: null\n      b: [null , null]\n      c: '.inf'\nmain:\n  - name: M\n    max: .inf\n"
        );
        let found: Vec<(u32, u32, bool)> = p
            .warnings
            .iter()
            .map(|w| (w.line, w.column, w.message.contains(DEPRECATION_NOTICE)))
            .collect();
        // The quoted one is text, not a number: no warning.
        assert_eq!(
            found,
            [(4, 10, true), (5, 11, true), (5, 18, true), (9, 10, true)]
        );
        assert!(
            p.warnings[0]
                .message
                .starts_with("plugs[0].config.a: `.inf` deprecated, plug gets None"),
            "{:?}",
            p.warnings[0]
        );
        assert!(
            p.warnings[1].message.contains("plugs[0].config.b[0]"),
            "{:?}",
            p.warnings[1]
        );
        assert!(
            p.warnings[3].message.contains("main[0].max"),
            "{:?}",
            p.warnings[3]
        );
    }

    #[test]
    fn keys_and_anchors_do_not_confuse_the_path() {
        let text = "plugs:\n  - &p\n    name: P\n    config: {.inf: 1, x: &v .inf, y: *v}\n";
        let p = prepare(text);
        // `.inf` as a key is not a value; an anchored `.inf` is warned but
        // left alone (its span includes the anchor).
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(
            p.warnings[0].message.contains("plugs[0].config.x"),
            "{:?}",
            p.warnings[0]
        );
    }
}
