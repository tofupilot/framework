//! What an author-supplied field `pattern` means: how it matches
//! (`compile_field_pattern`, TP-1086) and how a failure reads to the
//! operator (TP-760). The two live together so the message can never
//! describe a different rule from the one that rejected the entry.
//!
//! THE single implementation for the CLI, station runs and the web —
//! there is deliberately no TypeScript twin on those surfaces. The
//! engine's unit-field validation and `validate::validate_response`
//! call it natively (TUI in-process, kiosk and web console via the
//! reject-on-submit wire path, the web sequence editor via a studio-ws
//! request). If you are tempted to re-implement any of this
//! client-side "for instant feedback", don't: that is how wording
//! drift between surfaces happened the first time.
//!
//! Known exception: Studio desktop still runs a frozen copy in
//! `apps/studio/app/lib/operator-ui-legacy/pattern-messages.ts` until
//! its runner is ported. It already diverges from this module (substring
//! match, literal anchors required); treat it as pending deletion, not
//! as a second source of truth.
//!
//! The boundary: a regex explains itself when it reads left to right as
//! a fixed SEQUENCE of characters and character sets — a single
//! anchored class like `^[A-Z0-9-]+$` or `^[0-9]{4}$`, or a chain of
//! literals and classes like `^SN-\d{4}$`. For those, the offending
//! character, the expected set at that position and the length rule all
//! fall out of the pattern itself. The moment the pattern branches
//! (alternation, groups, lookaheads) there is no single "character N
//! should be X" truth to recite — derivation returns `None` and the
//! caller falls back to the author's `pattern_message`, then the bare
//! verdict.

use regex::Regex;
use std::collections::HashSet;

/// The server's unit-identity charset: `runs.create` rejects serial /
/// part / revision / batch / sub-unit values outside it. Unit-identity
/// prompt components without an authored `pattern` carry it
/// (`identify_unit/components.rs`), so the shared validator rejects a
/// bad scan at submit time with the recital derived from this very
/// string — instead of the upload failing after the test has run.
/// One-or-more match — empty strings are the required check's business.
pub const UNIT_FIELD_CHARSET_PATTERN: &str = "^[a-zA-Z0-9_.:+-]+$";

/// Compile an author-supplied field `pattern` with the one matching
/// semantic every surface applies: the entry must match in FULL, never
/// merely contain a match (TP-1086).
///
/// A bare `regex::is_match` is a substring search, so `SN-[0-9]+` would
/// accept `SCRAP SN-1 XX` — a mislabeled scan passing validation the
/// author believed was a format lock. Full-match is what the field
/// author means by "the serial looks like this", and what the derived
/// messages below already assume when they name character N.
///
/// Anchors therefore become optional in authored YAML: `[A-Z]{4}` and
/// `^[A-Z]{4}$` state the same rule. Authors who wrote the anchors keep
/// working — `^(?:^X$)$` is `^X$`.
pub fn compile_field_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{pattern})$"))
}

/// The pattern body without its optional anchors, for the derivation
/// parsers below: matching is full-match either way, so `^X$` and `X`
/// must explain themselves identically. Never used to match — that
/// always goes through `compile_field_pattern`.
fn pattern_body(pattern: &str) -> &str {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    let Some(stripped) = body.strip_suffix('$') else {
        return body;
    };
    // A trailing `\$` is a literal dollar, not an anchor; `\\$` is a
    // literal backslash followed by one.
    if stripped.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1 {
        body
    } else {
        stripped
    }
}

struct CharClassPattern {
    /// Recitable inside of the class (`\d` → `0-9`). Messages only.
    body: String,
    /// Per-character membership test. See `class_matcher`.
    allowed: Regex,
    min: usize,
    /// None = unbounded
    max: Option<usize>,
}

/// Translates the shorthand classes whose allowed-set recital we can
/// state truthfully (\d, \w). Anything else escaped-by-letter (\s, \D,
/// …) and negated classes stay non-derivable — their recital would lie.
fn resolve_class_body(raw: &str) -> Option<String> {
    if raw.starts_with('^') {
        return None;
    }
    let body = raw.replace(r"\d", "0-9").replace(r"\w", "a-zA-Z0-9_");
    let leftover_escape = body
        .char_indices()
        .any(|(i, c)| c == '\\' && body[i + 1..].chars().next().is_some_and(|n| n.is_ascii_alphabetic()));
    if leftover_escape {
        None
    } else {
        Some(body)
    }
}

/// Membership test for one character of a class, built from the RAW
/// class text rather than the recital. The compiled pattern reads `\d`
/// and `\w` Unicode-wide, and so must this test — otherwise an “é” the
/// pattern accepts gets blamed while the real offender goes unnamed.
fn class_matcher(raw: &str) -> Option<Regex> {
    Regex::new(&format!("^[{raw}]$")).ok()
}

/// Accepts a single class with any quantifier — `[...]+`, `[...]*`,
/// `[...]{n}`, `[...]{n,}`, `[...]{n,m}` — with \d / \w allowed both
/// bare (`\d{4}`) and inside the brackets (`[\d-]+`). Anchors are
/// optional: patterns are full-match, so `^[A-Z]+$` and `[A-Z]+` are
/// the same rule and derive the same message.
fn parse_char_class_pattern(pattern: &str) -> Option<CharClassPattern> {
    let re = Regex::new(r"^(?:\[([^\]]+)\]|(\\[dw]))([+*]|\{\d+(?:,\d*)?\})$").ok()?;
    let m = re.captures(pattern_body(pattern))?;
    let raw = m.get(1).or_else(|| m.get(2))?.as_str();
    let body = resolve_class_body(raw)?;
    let allowed = class_matcher(raw)?;
    let quant = m.get(3)?.as_str();
    let (min, max) = match quant {
        "+" => (1, None),
        "*" => (0, None),
        _ => {
            let q = Regex::new(r"^\{(\d+)(?:,(\d*))?\}$").ok()?;
            let qm = q.captures(quant)?;
            let min: usize = qm.get(1)?.as_str().parse().ok()?;
            let max = match qm.get(2) {
                None => Some(min),
                Some(g) if g.as_str().is_empty() => None,
                Some(g) => Some(g.as_str().parse().ok()?),
            };
            (min, max)
        }
    };
    Some(CharClassPattern { body, allowed, min, max })
}

/// "A-Z0-9_.:+-" → "uppercase letters, digits, _ . : + -"
fn humanize_char_class(body: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = body.to_string();
    if rest.contains("a-z") && rest.contains("A-Z") {
        parts.push("letters".to_string());
        rest = rest.replace("a-z", "").replace("A-Z", "");
    } else {
        if rest.contains("A-Z") {
            parts.push("uppercase letters".to_string());
            rest = rest.replace("A-Z", "");
        }
        if rest.contains("a-z") {
            parts.push("lowercase letters".to_string());
            rest = rest.replace("a-z", "");
        }
    }
    if rest.contains("0-9") {
        parts.push("digits".to_string());
        rest = rest.replace("0-9", "");
    }
    let literals: Vec<String> = rest
        .chars()
        .filter(|c| *c != '\\')
        .map(|c| c.to_string())
        .collect();
    if !literals.is_empty() {
        parts.push(literals.join(" "));
    }
    parts.join(", ")
}

/// The length messages name a unit the operator recognizes: "4 digits"
/// when the class is digits-only, "characters" otherwise.
fn length_noun(body: &str, n: usize) -> String {
    let humanized = humanize_char_class(body);
    let noun = match humanized.as_str() {
        "digits" => "digit",
        "letters" => "letter",
        _ => "character",
    };
    if n == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

/// The case-flipped twin of `ch`, or None when flipping changes nothing
/// (digits, punctuation, caseless scripts).
fn case_flipped(ch: char) -> Option<(String, bool)> {
    let lower: String = ch.to_lowercase().collect();
    let upper: String = ch.to_uppercase().collect();
    let ch_str = ch.to_string();
    if lower == ch_str && upper != ch_str {
        Some((upper, true)) // flip is to uppercase
    } else if upper == ch_str && lower != ch_str {
        Some((lower, false))
    } else {
        None
    }
}

fn derive_charset_error(cls: &CharClassPattern, value: &str) -> Option<String> {
    let test = |s: &str| cls.allowed.is_match(s);

    for (i, ch) in value.chars().enumerate() {
        if test(&ch.to_string()) {
            continue;
        }
        // Wrong case is the one true special case, because the fix differs.
        if let Some((flipped, to_upper)) = case_flipped(ch) {
            if test(&flipped) {
                let case = if to_upper { "uppercase" } else { "lowercase" };
                return Some(format!("Use {case} “{flipped}” instead of “{ch}”"));
            }
        }
        // Invisible characters get a name — quoting them shows nothing.
        // The position helps everyone on long or scanned values.
        let culprit = match ch {
            ' ' => "the space".to_string(),
            '\t' => "the tab".to_string(),
            _ => format!("“{ch}”"),
        };
        return Some(format!(
            "Remove {culprit} (character {}). Allowed: {}",
            i + 1,
            humanize_char_class(&cls.body)
        ));
    }

    // Every character is legal — the only rule left is the quantifier's
    // length. Count, don't clamp: the message carries the numbers.
    let len = value.chars().count();
    if len < cls.min {
        let noun = length_noun(&cls.body, cls.min);
        return Some(format!("Too short: {len}/{} {noun}", cls.min));
    }
    if let Some(max) = cls.max {
        if len > max {
            let noun = length_noun(&cls.body, max);
            return Some(format!("Too long: {len}/{max} {noun}"));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Sequence patterns: a left-to-right chain of literals and character
// classes (^SN-\d{4}$, ^v\d+\.\d+$). Still no branching — alternation,
// groups and lookaheads stay non-derivable.
// ---------------------------------------------------------------------------

enum TermKind {
    /// Matches exactly this character.
    Literal(char),
    /// Bracket class; `body` is the recitable inside.
    Class { body: String, re: Regex },
    /// `.` wildcard.
    Any,
}

struct SeqTerm {
    kind: TermKind,
    min: usize,
    /// None = unbounded
    max: Option<usize>,
}

impl SeqTerm {
    fn test(&self, ch: char) -> bool {
        match &self.kind {
            TermKind::Literal(c) => *c == ch,
            TermKind::Class { re, .. } => re.is_match(&ch.to_string()),
            TermKind::Any => true,
        }
    }

    fn test_str(&self, s: &str) -> bool {
        match &self.kind {
            TermKind::Literal(c) => c.to_string() == s,
            TermKind::Class { re, .. } => re.is_match(s),
            TermKind::Any => s.chars().count() == 1,
        }
    }
}

/// Consumes an optional quantifier at `*i`, returning (min, max) —
/// (1, Some(1)) when none is present, None when a `{…}` is malformed.
fn parse_quantifier(chars: &[char], i: &mut usize) -> Option<(usize, Option<usize>)> {
    match chars.get(*i) {
        Some('+') => {
            *i += 1;
            Some((1, None))
        }
        Some('*') => {
            *i += 1;
            Some((0, None))
        }
        Some('?') => {
            *i += 1;
            Some((0, Some(1)))
        }
        Some('{') => {
            let end = chars[*i..].iter().position(|c| *c == '}')? + *i;
            let inner: String = chars[*i + 1..end].iter().collect();
            let q = Regex::new(r"^(\d+)(?:,(\d*))?$").ok()?;
            let m = q.captures(&inner)?;
            let min: usize = m.get(1)?.as_str().parse().ok()?;
            let max = match m.get(2) {
                None => Some(min),
                Some(g) if g.as_str().is_empty() => None,
                Some(g) => Some(g.as_str().parse().ok()?),
            };
            if let Some(max) = max {
                if max < min {
                    return None;
                }
            }
            *i = end + 1;
            Some((min, max))
        }
        _ => Some((1, Some(1))),
    }
}

/// Parses a sequence of literals / classes with optional quantifiers,
/// or None when the pattern branches or uses an escape whose recital
/// would lie (\s, \D, negated classes…). Anchors are optional — see
/// `pattern_body`; a `^` or `$` left INSIDE the body is a branch we
/// don't walk, and the term loop rejects it.
fn parse_sequence_pattern(pattern: &str) -> Option<Vec<SeqTerm>> {
    let body = pattern_body(pattern);
    if body.is_empty() {
        return None;
    }
    compile_field_pattern(pattern).ok()?;
    let chars: Vec<char> = body.chars().collect();
    let src = &chars[..];
    let mut terms: Vec<SeqTerm> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let ch = src[i];
        let kind = if ch == '[' {
            let end = src[i + 1..].iter().position(|c| *c == ']')? + i + 1;
            let raw: String = src[i + 1..end].iter().collect();
            let body = resolve_class_body(&raw)?;
            let re = class_matcher(&raw)?;
            i = end + 1;
            TermKind::Class { body, re }
        } else if ch == '\\' {
            let next = *src.get(i + 1)?;
            i += 2;
            match next {
                'd' => TermKind::Class {
                    body: "0-9".to_string(),
                    re: class_matcher(r"\d")?,
                },
                'w' => TermKind::Class {
                    body: "a-zA-Z0-9_".to_string(),
                    re: class_matcher(r"\w")?,
                },
                c if c.is_ascii_alphabetic() => return None, // \s, \D, \b… — recital would lie
                c => TermKind::Literal(c), // escaped punctuation: \. \- \+ …
            }
        } else if ch == '.' {
            i += 1;
            TermKind::Any
        } else if "|(){}*+?^$".contains(ch) {
            return None; // branching or a stray quantifier — not a sequence
        } else {
            i += 1;
            TermKind::Literal(ch)
        };
        let (min, max) = parse_quantifier(src, &mut i)?;
        terms.push(SeqTerm { kind, min, max });
        if terms.len() > 64 {
            return None;
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms)
    }
}

/// State `(i, c)`: inside term `i` having consumed `c` of it;
/// `i == terms.len()` is the accept state.
fn seq_closure(states: &mut HashSet<(usize, usize)>, terms: &[SeqTerm]) {
    let mut queue: Vec<(usize, usize)> = states.iter().copied().collect();
    while let Some((i, c)) = queue.pop() {
        if i < terms.len() && c >= terms[i].min {
            let next = (i + 1, 0);
            if states.insert(next) {
                queue.push(next);
            }
        }
    }
}

/// The terms that could consume the next character from `states`,
/// in term order so recitals read left to right.
fn seq_expected<'a>(states: &HashSet<(usize, usize)>, terms: &'a [SeqTerm]) -> Vec<&'a SeqTerm> {
    let mut indices: Vec<usize> = states
        .iter()
        .filter(|(i, c)| {
            terms
                .get(*i)
                .is_some_and(|t| t.max.is_none_or(|max| *c < max))
        })
        .map(|(i, _)| *i)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices.into_iter().map(|i| &terms[i]).collect()
}

/// "a digit", "an uppercase letter", "“-”", "one of: digits, _ -" —
/// what the operator should type at the failing position.
fn describe_expected(expected: &[&SeqTerm]) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut parts: Vec<String> = Vec::new();
    for term in expected {
        let (key, text) = match &term.kind {
            TermKind::Literal(' ') => ("literal: ".to_string(), "a space".to_string()),
            TermKind::Literal(c) => (format!("literal:{c}"), format!("“{c}”")),
            TermKind::Any => ("any".to_string(), "any character".to_string()),
            TermKind::Class { body, .. } => {
                let humanized = humanize_char_class(body);
                let text = match humanized.as_str() {
                    "digits" => "a digit".to_string(),
                    "letters" => "a letter".to_string(),
                    "uppercase letters" => "an uppercase letter".to_string(),
                    "lowercase letters" => "a lowercase letter".to_string(),
                    _ => format!("one of: {humanized}"),
                };
                (format!("class:{body}"), text)
            }
        };
        if seen.insert(key) {
            parts.push(text);
        }
    }
    parts.join(" or ")
}

/// Walks `value` through the term chain, tracking every viable
/// position at once, and names the first character where all of them
/// die — or what is still missing when the value ends early.
fn diagnose_sequence(terms: &[SeqTerm], value: &str) -> Option<String> {
    let mut states: HashSet<(usize, usize)> = HashSet::from([(0, 0)]);
    seq_closure(&mut states, terms);

    // The fixed opening literals ("SN-"): a failure inside them gets the
    // strongest possible message, since there is exactly one right start.
    let leading_literal: String = terms
        .iter()
        .take_while(|t| t.min == 1 && t.max == Some(1) && matches!(t.kind, TermKind::Literal(_)))
        .map(|t| match t.kind {
            TermKind::Literal(c) => c,
            _ => unreachable!(),
        })
        .collect();
    let leading_len = leading_literal.chars().count();

    for (p, ch) in value.chars().enumerate() {
        let mut next: HashSet<(usize, usize)> = HashSet::new();
        for &(i, c) in &states {
            if let Some(term) = terms.get(i) {
                if term.max.is_none_or(|max| c < max) && term.test(ch) {
                    next.insert((i, c + 1));
                }
            }
        }
        seq_closure(&mut next, terms);
        if !next.is_empty() {
            states = next;
            continue;
        }

        let expected = seq_expected(&states, terms);
        let name = |ch: char| match ch {
            ' ' => "the space".to_string(),
            '\t' => "the tab".to_string(),
            _ => format!("“{ch}”"),
        };
        if expected.is_empty() {
            return Some(format!(
                "Too long: remove {} (character {})",
                name(ch),
                p + 1
            ));
        }
        // Wrong case is the one true special case, because the fix differs.
        if let Some((flipped, to_upper)) = case_flipped(ch) {
            if expected.iter().any(|t| t.test_str(&flipped)) {
                let case = if to_upper { "uppercase" } else { "lowercase" };
                return Some(format!(
                    "Use {case} “{flipped}” instead of “{ch}” (character {})",
                    p + 1
                ));
            }
        }
        if p < leading_len {
            return Some(format!("Should start with “{leading_literal}”"));
        }
        // Position first, then the culprit, then what belongs there —
        // invisible characters get a name instead of an empty quote.
        let culprit = match ch {
            ' ' => "(space)".to_string(),
            '\t' => "(tab)".to_string(),
            _ => format!("“{ch}”"),
        };
        return Some(format!(
            "Character {} {culprit} should be {}",
            p + 1,
            describe_expected(&expected)
        ));
    }

    if !states.contains(&(terms.len(), 0)) {
        let expected = seq_expected(&states, terms);
        if expected.is_empty() {
            return None; // can't happen: not accepting ⇒ something is consumable
        }
        return Some(format!(
            "Too short: needs {} at character {}",
            describe_expected(&expected),
            value.chars().count() + 1
        ));
    }
    None
}

/// True when `derive_pattern_error` can explain a failure against
/// `pattern` by itself. The Studio editors' authoring nudge fires only
/// for patterns where it can't — that's when the author's
/// `pattern_message` IS the operator's error message.
pub fn is_derivable_pattern(pattern: &str) -> bool {
    parse_char_class_pattern(pattern).is_some() || parse_sequence_pattern(pattern).is_some()
}

/// Derives the error for `value` against a derivable `pattern` — a
/// charset-only class or a literal/class sequence — or None: either the
/// pattern branches (not derivable) or the value matches. Every message
/// leads with the fix, not the diagnosis; recitals are reference
/// material and go at the tail.
pub fn derive_pattern_error(pattern: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    if let Some(cls) = parse_char_class_pattern(pattern) {
        return derive_charset_error(&cls, value);
    }
    // Not a single class — try the sequence chain. The real regex is
    // the referee: a value it accepts never gets an error, whatever
    // the walker thinks.
    let terms = parse_sequence_pattern(pattern)?;
    let re = compile_field_pattern(pattern).ok()?;
    if re.is_match(value) {
        return None;
    }
    diagnose_sequence(&terms, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The reference suite for operator-facing pattern wording — every
    // surface (kiosk rejection, TUI, Studio) renders these exact
    // strings, so a wording change here IS the product change.

    #[test]
    fn charset_names_the_offender_and_leads_with_the_fix() {
        let charset = "^[A-Z0-9_.:+-]+$";
        assert_eq!(
            derive_pattern_error(charset, "SN#0042").as_deref(),
            Some("Remove “#” (character 3). Allowed: uppercase letters, digits, _ . : + -")
        );
        assert_eq!(
            derive_pattern_error(charset, "SN 0042").as_deref(),
            Some("Remove the space (character 3). Allowed: uppercase letters, digits, _ . : + -")
        );
        assert_eq!(
            derive_pattern_error(charset, "sN-0042").as_deref(),
            Some("Use uppercase “S” instead of “s”")
        );
        assert_eq!(derive_pattern_error(charset, "SN-0042"), None);
    }

    #[test]
    fn shorthand_classes_blame_what_the_pattern_rejects() {
        // `\w` and `\d` are Unicode-wide in the compiled pattern. The
        // derivation must not name an “é” the pattern accepts; the real
        // offender is the “#”, and a value of legal letters fails on
        // length, not on a character.
        assert_eq!(
            derive_pattern_error(r"^[\w-]+$", "Ré#123").as_deref(),
            Some("Remove “#” (character 3). Allowed: letters, digits, _ -")
        );
        assert_eq!(
            derive_pattern_error(r"^\w{4}$", "éé").as_deref(),
            Some("Too short: 2/4 characters")
        );
        let seq = derive_pattern_error(r"^SN-\w{2}$", "SN-é#").unwrap();
        assert!(!seq.contains('é'), "{seq}");
        assert!(seq.contains('#'), "{seq}");
    }

    #[test]
    fn charset_derives_counted_length_errors() {
        assert_eq!(
            derive_pattern_error("^[0-9]{4}$", "42").as_deref(),
            Some("Too short: 2/4 digits")
        );
        assert_eq!(
            derive_pattern_error("^[0-9]{4}$", "004217").as_deref(),
            Some("Too long: 6/4 digits")
        );
        assert_eq!(
            derive_pattern_error("^[A-Z0-9]{8,20}$", "LOT7").as_deref(),
            Some("Too short: 4/8 characters")
        );
    }

    #[test]
    fn sequence_names_position_offender_and_expectation() {
        let sn = r"^SN-\d{4}$";
        assert_eq!(
            derive_pattern_error(sn, "XN-1234").as_deref(),
            Some("Should start with “SN-”")
        );
        assert_eq!(
            derive_pattern_error(sn, "sn-1234").as_deref(),
            Some("Use uppercase “S” instead of “s” (character 1)")
        );
        assert_eq!(
            derive_pattern_error(sn, "SN-12X4").as_deref(),
            Some("Character 6 “X” should be a digit")
        );
        assert_eq!(
            derive_pattern_error(sn, "SN- 234").as_deref(),
            Some("Character 4 (space) should be a digit")
        );
        assert_eq!(
            derive_pattern_error(sn, "SN-12").as_deref(),
            Some("Too short: needs a digit at character 6")
        );
        assert_eq!(
            derive_pattern_error(sn, "SN-12345").as_deref(),
            Some("Too long: remove “5” (character 8)")
        );
        assert_eq!(derive_pattern_error(sn, "SN-1234"), None);
    }

    #[test]
    fn sequence_recites_every_viable_expectation() {
        // After "v1" the walker can still be inside \d+ or at the dot.
        assert_eq!(
            derive_pattern_error(r"^v\d+\.\d+$", "v1x2").as_deref(),
            Some("Character 3 “x” should be a digit or “.”")
        );
    }

    #[test]
    fn branching_and_lying_recitals_stay_non_derivable() {
        for pattern in [
            r"^(\d{3}-\d{4}|\d{10})$", // alternation
            r"^([0-9A-F]{2}:){5}[0-9A-F]{2}$", // group
            r"^SN\s\d+$",              // \s
            r"^[^A-Z]\d+$",            // negated class
        ] {
            assert_eq!(derive_pattern_error(pattern, "whatever"), None, "{pattern}");
        }
    }

    /// TP-1086: patterns are full-match, so writing the anchors is the
    /// author's option and never changes the rule — or the wording.
    #[test]
    fn anchors_are_optional_and_change_nothing() {
        for (bare, anchored) in [
            (r"SN-\d{4}", r"^SN-\d{4}$"),
            ("[A-Z0-9-]+", "^[A-Z0-9-]+$"),
            (r"\d{4}", r"^\d{4}$"),
        ] {
            for value in ["SN-12X4", "ab 1", "12345", ""] {
                assert_eq!(
                    derive_pattern_error(bare, value),
                    derive_pattern_error(anchored, value),
                    "{bare} vs {anchored} on {value:?}"
                );
            }
        }
        assert_eq!(
            derive_pattern_error(r"SN-\d{4}", "SN-12X4").as_deref(),
            Some("Character 6 “X” should be a digit")
        );
    }

    /// The referee inside `derive_pattern_error` is the same compile the
    /// validators use: a value the rule accepts never yields an error,
    /// and one it accepts only as a substring is not spared.
    #[test]
    fn derivation_agrees_with_full_match_semantics() {
        assert_eq!(derive_pattern_error(r"SN-\d{4}", "SN-0042"), None);
        // Accepted as a substring under the old semantic; now explained.
        assert!(derive_pattern_error(r"SN-\d{4}", "SCRAP SN-0042").is_some());
    }
}
