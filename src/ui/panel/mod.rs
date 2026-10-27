//! Operator panels: a phase's `ui.panel`, one plain HTML file (inline SVG
//! for instruments) that replaces the default top-to-bottom component
//! list, and the procedure's root `ui.panel` around every screen.
//!
//! - `data-bind="PATH"` shows a value: the element gets `data-value`, a
//!   numeric `--value`, and its text when it has no child element.
//! - `<input|select|textarea name="KEY">` edits the input component KEY,
//!   validated like the component list; `<slot name="KEY">` places it.
//! - `<button name="X">` is a press the phase reads with
//!   `ui.pressed("X")`; `<button type="submit">` answers the screen.
//!
//! Undeclared `ui.<key>` values are the phase's own display values: never
//! in an answer, never recorded.
//!
//! The station's part is to load the file into the `UiRequest` (the
//! dashboard has no procedure directory to fetch it from) and to refuse,
//! before the run, a panel the browser would have to strip or could not
//! bind. The browser sanitizes again before rendering
//! (`packages/operator-ui/src/panel/sanitize.ts`): this lint is the
//! author's feedback, that sanitizer is the security boundary.

pub mod names;
pub mod scan;

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use station_protocol::UiPanel;

use crate::procedure::markers::did_you_mean;
use crate::procedure::schema::UIComponentType;
use names::{Names, Problem};

/// Largest panel file. The panel rides inside every `UiRequest`, and a
/// station event must stay under `station_protocol::MAX_EVENT_BYTES`
/// (60 kB) to reach the dashboard; this leaves room for the components.
pub const MAX_PANEL_BYTES: usize = 40 * 1024;
/// Past this many elements a panel is likely a designer export that
/// will cost frames on a kiosk: warned.
pub const WARN_ELEMENTS: usize = 2_000;
/// Past this many, refused.
pub const MAX_ELEMENTS: usize = 10_000;

/// Elements the browser's sanitizer drops (`FORBID_TAGS` in
/// `packages/operator-ui/src/panel/sanitize.ts`, and `animate` / `set`,
/// which DOMPurify never keeps): they run code, load another document,
/// navigate, or animate an attribute into a URL.
const FORBIDDEN_ELEMENTS: [&str; 17] = [
    "script",
    "iframe",
    "frame",
    "frameset",
    "object",
    "embed",
    "applet",
    "foreignobject",
    "link",
    "meta",
    "base",
    "form",
    "portal",
    "template",
    "noscript",
    "animate",
    "set",
];

/// Attributes the browser drops whatever their value (`FORBID_ATTR`).
const FORBIDDEN_ATTRS: [&str; 5] = ["srcdoc", "formaction", "action", "ping", "srcset"];

/// Attributes whose value is a URL (`URL_ATTRS` in the sanitizer).
const URL_ATTRS: [&str; 6] = ["href", "xlink:href", "src", "poster", "background", "data"];

/// Raster images a `data:` URL may hold (`RASTER_DATA` in the sanitizer).
const RASTER_DATA: [&str; 7] = ["png", "jpeg", "jpg", "gif", "webp", "bmp", "avif"];

/// `<input type>`s that would submit or reset the prompt's form: the
/// sanitizer turns them into plain buttons.
const SUBMITTING_INPUTS: [&str; 3] = ["submit", "image", "reset"];

/// The run panel's buttons: the control bar's commands.
const ACTIONS: [&str; 2] = ["stop", "kill"];

/// `id` / `name` values the browser's sanitizer drops (DOMPurify's
/// `SANITIZE_DOM`): properties of the page or of the prompt's form, which
/// an element by that name would shadow. The lowercase words a key could
/// be; camelCase ones never match a Python name.
const CLOBBERING_NAMES: &[&str] = &[
    "action",
    "after",
    "all",
    "anchors",
    "animate",
    "append",
    "applets",
    "attributes",
    "autocomplete",
    "autofocus",
    "before",
    "blur",
    "body",
    "children",
    "clear",
    "click",
    "close",
    "closest",
    "contains",
    "cookie",
    "dataset",
    "dir",
    "doctype",
    "domain",
    "draggable",
    "elements",
    "embeds",
    "encoding",
    "enctype",
    "evaluate",
    "focus",
    "fonts",
    "forms",
    "head",
    "hidden",
    "id",
    "images",
    "implementation",
    "inert",
    "lang",
    "length",
    "links",
    "location",
    "matches",
    "method",
    "name",
    "nonce",
    "normalize",
    "open",
    "part",
    "plugins",
    "popover",
    "prefix",
    "prepend",
    "referrer",
    "rel",
    "remove",
    "reset",
    "role",
    "scripts",
    "scroll",
    "slot",
    "spellcheck",
    "style",
    "submit",
    "target",
    "timeline",
    "title",
    "translate",
    "write",
    "writeln",
    "charset",
    "fullscreen",
    "prerendering",
    "autocapitalize",
];

/// Event handler properties (`onload`...) are page properties too.
const HANDLER_EVENTS: &[&str] = &[
    "abort",
    "blur",
    "change",
    "click",
    "close",
    "contextmenu",
    "copy",
    "cut",
    "dblclick",
    "error",
    "focus",
    "input",
    "invalid",
    "keydown",
    "keypress",
    "keyup",
    "load",
    "mousedown",
    "mousemove",
    "mouseout",
    "mouseover",
    "mouseup",
    "paste",
    "reset",
    "resize",
    "scroll",
    "select",
    "submit",
    "toggle",
    "wheel",
];

fn clobbers(name: &str) -> bool {
    let n = name.trim();
    CLOBBERING_NAMES.contains(&n)
        || n.strip_prefix("on")
            .is_some_and(|e| HANDLER_EVENTS.contains(&e))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub error: bool,
    pub message: String,
}

impl Finding {
    fn error(message: String) -> Self {
        Self {
            error: true,
            message,
        }
    }
    fn warning(message: String) -> Self {
        Self {
            error: false,
            message,
        }
    }
}

/// The panel file, resolved inside the procedure directory. Refuses an
/// absolute path, a `..` and a symlink out of the directory: the file is
/// shipped to every viewer of the run.
fn resolve_path(procedure_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel.trim());
    if rel.trim().is_empty() {
        return Err(
            "`panel` is empty: write the path of an .html file in the procedure directory".into(),
        );
    }
    if rel_path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "`panel: {rel}` must be a relative path inside the procedure directory (no `/` at the start, no `..`)"
        ));
    }
    let ext = rel_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if !matches!(ext.as_deref(), Some("html" | "htm")) {
        return Err(format!("`panel: {rel}` must be an .html file"));
    }
    let full = procedure_dir.join(rel_path);
    let canonical = full
        .canonicalize()
        .map_err(|_| format!("`panel: {rel}`: file not found at {}", full.display()))?;
    let root = procedure_dir
        .canonicalize()
        .unwrap_or_else(|_| procedure_dir.to_path_buf());
    if !canonical.starts_with(&root) {
        return Err(format!(
            "`panel: {rel}` points outside the procedure directory"
        ));
    }
    Ok(canonical)
}

/// Read the panel for a `UiRequest`. Errors are the same ones `lint`
/// reports, so a run that passed the lint does not meet them.
pub fn load(procedure_dir: &Path, rel: &str) -> Result<UiPanel, String> {
    let path = resolve_path(procedure_dir, rel)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("`panel: {rel}`: {e}"))?;
    if bytes.len() > MAX_PANEL_BYTES {
        return Err(format!(
            "`panel: {rel}` is {} kB; the limit is {} kB (the panel travels with every operator prompt)",
            bytes.len().div_ceil(1024),
            MAX_PANEL_BYTES / 1024
        ));
    }
    let html = String::from_utf8(bytes).map_err(|_| format!("`panel: {rel}` is not UTF-8 text"))?;
    let sha256 = format!("{:x}", Sha256::digest(html.as_bytes()));
    let events = events(&scan::scan(&html));
    Ok(UiPanel {
        html,
        sha256,
        events,
    })
}

/// What kind of button `el` is: `<button type="submit">` submits the
/// screen; any other `<button>` is a plain button (the sanitizer makes it
/// `type="button"`, the HTML default would submit).
fn is_submit(el: &scan::Element) -> bool {
    attr(el, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("submit"))
}

/// The panel's press buttons: `<button name>` that neither submits nor
/// is the run's `stop` / `kill`, each name once, in document order.
fn events(scanned: &scan::Scan) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for el in scanned
        .elements
        .iter()
        .filter(|e| e.name == "button" && !is_submit(e))
    {
        let Some(name) = attr(el, "name").map(str::trim) else {
            continue;
        };
        if !name.is_empty() && !ACTIONS.contains(&name) && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Whether the panel brings a `<button type="submit">`: a screen with
/// one waits for it, as one with input components waits for Continue.
pub fn has_submit(html: &str) -> bool {
    scan::scan(html)
        .elements
        .iter()
        .any(|e| e.name == "button" && is_submit(e))
}

/// Everything wrong with the panel of a phase: the file itself, then
/// its content against the phase's components and measurements.
pub fn lint(procedure_dir: &Path, rel: &str, names: &Names) -> Vec<Finding> {
    match load(procedure_dir, rel) {
        Err(e) => vec![Finding::error(e)],
        Ok(panel) => lint_source(rel, &panel.html, names),
    }
}

/// `lint` on panel text already read.
pub fn lint_source(rel: &str, html: &str, names: &Names) -> Vec<Finding> {
    let scanned = scan::scan(html);
    let mut out = Vec::new();
    let at = |line: u32| format!("`{rel}` line {line}");

    let count = scanned.elements.len();
    if count > MAX_ELEMENTS {
        out.push(Finding::error(format!(
            "`{rel}` has {count} elements; the limit is {MAX_ELEMENTS}. Flatten exported artwork (merge paths, drop hidden layers)"
        )));
    } else if count > WARN_ELEMENTS {
        out.push(Finding::warning(format!(
            "`{rel}` has {count} elements; past {WARN_ELEMENTS} a kiosk may drop frames. Flatten exported artwork (merge paths, drop hidden layers)"
        )));
    }

    for (line, css) in &scanned.styles {
        if let Some(problem) = css_problem(css, false) {
            out.push(Finding::error(format!(
                "{} (<style>): {problem}",
                at(*line)
            )));
        }
    }

    // Every path read, checked once against what the procedure declares.
    let mut reads: Vec<(u32, String)> = Vec::new();
    let mut screens = 0;
    let mut actions = BTreeSet::new();

    for el in &scanned.elements {
        let here = at(el.line);
        let mut error = |message: String| out.push(Finding::error(format!("{here}: {message}")));
        if FORBIDDEN_ELEMENTS.contains(&el.name.as_str()) {
            error(format!(
                "<{}> is not allowed in a panel (no scripts, embedded documents, forms or links)",
                el.name
            ));
            continue;
        }
        if el.name.contains('-') {
            error(format!(
                "<{}> is a custom element; a panel is plain HTML and SVG (bind with `data-bind`, place a component with `<slot name>`)",
                el.name
            ));
            continue;
        }
        let name = attr(el, "name").map(str::trim);

        for (attr_name, value) in &el.attrs {
            let value = value.as_deref().unwrap_or("");
            if attr_name.starts_with("on") {
                error(format!(
                    "`{attr_name}` is not allowed: panels run no JavaScript. A `<button name>` is read in Python with `ui.pressed(name)`"
                ));
            } else if FORBIDDEN_ATTRS.contains(&attr_name.as_str()) {
                let why = if attr_name == "srcset" {
                    "a list of URLs; use `src`"
                } else {
                    "it loads or sends to a URL"
                };
                error(format!("`{attr_name}` is not allowed: {why}"));
            } else if URL_ATTRS.contains(&attr_name.as_str()) {
                if let Some(problem) = url_problem(&el.name, value) {
                    error(format!("`{attr_name}`: {problem}"));
                }
            } else if attr_name == "style" {
                if let Some(problem) = css_problem(value, true) {
                    error(format!("`style`: {problem}"));
                }
            } else if attr_name.starts_with("tp-") {
                error(format!(
                    "`{attr_name}` is not a panel attribute: show a value with `data-bind=\"PATH\"`, bind a component with `name=\"KEY\"`"
                ));
            } else if (attr_name == "id" || attr_name == "name") && clobbers(value) {
                error(format!(
                    "`{attr_name}=\"{value}\"` names a property of the page, which the browser keeps no element under; rename it"
                ));
            } else if attr_name == "data-bind" {
                reads.push((el.line, value.trim().to_string()));
            } else if attr_name == "data-format" {
                if attr(el, "data-bind").is_none() {
                    error("`data-format` formats the `data-bind` of the same element".into());
                } else if !valid_format(value.trim()) {
                    error(format!(
                        "`data-format=\"{value}\"`: write the decimals to show as zeros: `0`, `0.0`, `0.00`"
                    ));
                }
            }
        }

        match el.name.as_str() {
            "slot" => match name.filter(|n| !n.is_empty()) {
                None if names.procedure => screens += 1,
                None => error(
                    "a <slot> without `name` is where the procedure's panel shows each phase's screen; in a phase's panel write `<slot name=\"KEY\">` to place a component".into(),
                ),
                Some(_) if names.procedure => error(
                    "the procedure's panel has no components: its one <slot> (no `name`) shows each phase's screen".into(),
                ),
                Some(key) if names.component(key).is_some() => {}
                Some(key) => error(format!(
                    "<slot name=\"{key}\">: no component `{key}` in this phase{}",
                    did_you_mean(key, names.components.iter().map(|c| c.key.as_str()))
                )),
            },
            "input" | "select" | "textarea" => {
                let input_type = attr(el, "type").map(|t| t.trim().to_ascii_lowercase());
                if el.name == "input"
                    && input_type.as_deref().is_some_and(|t| SUBMITTING_INPUTS.contains(&t))
                {
                    error("an <input> that submits or resets the form becomes a plain button; use <button type=\"submit\">".into());
                    continue;
                }
                let Some(key) = name.filter(|n| !n.is_empty()) else {
                    continue;
                };
                if names.procedure {
                    error(format!(
                        "<{} name=\"{key}\">: the procedure's panel has no components; a field belongs in a phase's panel",
                        el.name
                    ));
                } else if let Some(message) = field_problem(el, input_type.as_deref(), key, names) {
                    error(message);
                }
            }
            "button" => {
                if names.procedure && is_submit(el) {
                    error("the procedure's panel answers no screen: a `<button type=\"submit\">` belongs in a phase's panel".into());
                    continue;
                }
                let Some(key) = name.filter(|n| !n.is_empty()) else {
                    continue;
                };
                if names.procedure {
                    if !ACTIONS.contains(&key) {
                        error(format!(
                            "the procedure's panel has two buttons, `<button name=\"stop\">` and `<button name=\"kill\">`; `{key}` belongs in a phase's panel"
                        ));
                    } else {
                        actions.insert(key.to_string());
                    }
                } else if ACTIONS.contains(&key) {
                    error(format!(
                        "`<button name=\"{key}\">` stops the run: it goes in the procedure's root `ui: panel:`"
                    ));
                } else if is_submit(el) {
                    if let Some(message) = submit_problem(el, key, names) {
                        error(message);
                    }
                } else if names::split(key).is_none_or(|s| s.len() != 1) {
                    error(format!(
                        "`<button name=\"{key}\">`: a button's name is what Python reads, `ui.pressed(\"name\")`: letters, digits and `_`"
                    ));
                }
            }
            _ => {}
        }
    }

    // Kill escalates the panel's own Stop, as on the control bar.
    if actions.contains("kill") && !actions.contains("stop") {
        out.push(Finding::error(format!(
            "`{rel}`: `<button name=\"kill\">` arms only after the panel's Stop; add a `<button name=\"stop\">`"
        )));
    }
    if names.procedure && screens != 1 {
        out.push(Finding::error(format!(
            "`{rel}` has {screens} <slot> without `name`; the procedure's panel needs exactly one: where each phase's screen shows"
        )));
    }

    let mut reported = BTreeSet::new();
    for (line, path) in reads {
        if !reported.insert(path.clone()) {
            continue;
        }
        match names.check(&path) {
            Ok(()) => {}
            Err(Problem::Error(e)) => out.push(Finding::error(format!("{}: {e}", at(line)))),
            Err(Problem::Warning(w)) => out.push(Finding::warning(format!("{}: {w}", at(line)))),
        }
    }
    out
}

/// What is wrong with a field `name="KEY"`: it must name an input
/// component of the phase, of a type the control can hold.
fn field_problem(
    el: &scan::Element,
    input_type: Option<&str>,
    key: &str,
    names: &Names,
) -> Option<String> {
    let tag = &el.name;
    let Some(component) = names.component(key) else {
        return Some(format!(
            "<{tag} name=\"{key}\">: no component `{key}` in this phase; a field writes the input component of that key{}",
            did_you_mean(key, names.components.iter().map(|c| c.key.as_str()))
        ));
    };
    let kind = component.component_type;
    if !kind.is_input() {
        return Some(format!(
            "<{tag} name=\"{key}\">: `{key}` is a `{}`, a display component; show it with `data-bind=\"ui.{key}\"`",
            kind.as_str()
        ));
    }
    use UIComponentType as T;
    let fits = match (tag.as_str(), input_type) {
        ("input", Some("checkbox")) if attr(el, "value").is_some() => {
            matches!(kind, T::Multiselect | T::Checklist)
        }
        ("input", Some("checkbox")) => kind == T::Switch,
        ("input", Some("radio")) if attr(el, "value").is_none() => {
            return Some(format!(
                "<input type=\"radio\" name=\"{key}\"> needs `value=\"...\"`: the option it picks"
            ));
        }
        ("input", Some("radio")) => matches!(kind, T::Select | T::Radio),
        ("input", Some("range")) => matches!(kind, T::Slider | T::NumberInput),
        ("input", Some("number")) => matches!(kind, T::NumberInput | T::Slider),
        ("select", _) if el.attrs.iter().any(|(n, _)| n == "multiple") => {
            matches!(kind, T::Multiselect | T::Checklist)
        }
        ("select", _) => matches!(kind, T::Select | T::Radio),
        _ => !matches!(kind, T::Switch | T::Multiselect | T::Checklist),
    };
    (!fits).then(|| {
        let control = match kind {
            T::Switch => "<input type=\"checkbox\">",
            T::Multiselect | T::Checklist => {
                "<input type=\"checkbox\" value=\"...\"> or <select multiple>"
            }
            T::Select | T::Radio => "<select> or <input type=\"radio\" value=\"...\">",
            T::Slider | T::NumberInput => "<input type=\"range\"> or <input type=\"number\">",
            _ => "<input> or <textarea>",
        };
        format!(
            "<{tag} name=\"{key}\">: `{key}` is a `{}`: bind it with {control}",
            kind.as_str()
        )
    })
}

/// What is wrong with `<button type="submit" name="KEY" value="V">`: it
/// writes V into the input component KEY, then submits.
fn submit_problem(el: &scan::Element, key: &str, names: &Names) -> Option<String> {
    match names.component(key) {
        None => Some(format!(
            "<button type=\"submit\" name=\"{key}\">: no component `{key}` in this phase; a named submit writes its `value` into that input component{}",
            did_you_mean(key, names.components.iter().map(|c| c.key.as_str()))
        )),
        Some(c) if !c.component_type.is_input() => Some(format!(
            "<button type=\"submit\" name=\"{key}\">: `{key}` is a display component; a named submit writes an input component"
        )),
        Some(_) if attr(el, "value").is_none() => Some(format!(
            "<button type=\"submit\" name=\"{key}\"> needs `value=\"...\"`: what it writes into `{key}`"
        )),
        Some(_) => None,
    }
}

/// `0`, `0.0`, `0.00`...: the number of decimals, shown as zeros.
fn valid_format(f: &str) -> bool {
    match f.split_once('.') {
        None => f == "0",
        Some((int, dec)) => {
            int == "0" && !dec.is_empty() && dec.len() <= 12 && dec.chars().all(|c| c == '0')
        }
    }
}

fn attr<'a>(el: &'a scan::Element, name: &str) -> Option<&'a str> {
    el.attrs
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, v)| v.as_deref())
}

/// A URL the panel may carry: a fragment (`#gradient`), an inline raster
/// image, or a relative image path resolved like component images. Never
/// a scheme (`javascript:`, `https:`) or a protocol-relative `//host`:
/// a panel loads nothing from outside and runs nothing.
fn url_problem(element: &str, value: &str) -> Option<String> {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    if v.is_empty() || v.starts_with('#') {
        return None;
    }
    if element == "a" || element == "area" {
        return Some(
            "a link would navigate the operator screen away; only `#fragment` links are allowed"
                .into(),
        );
    }
    if element == "use" {
        return Some("<use> may only point at an element of the panel: `href=\"#id\"`".into());
    }
    if is_raster_data(&lower) {
        return None;
    }
    if lower.starts_with("data:") {
        return Some(
            "only raster `data:image/...` URLs are allowed (png, jpeg, gif, webp, bmp, avif)"
                .into(),
        );
    }
    if lower.starts_with("//")
        || has_scheme(&lower)
        || lower.starts_with('/')
        || lower.starts_with('\\')
    {
        return Some(format!(
            "`{v}`: panels load nothing from outside the procedure; use a path relative to the procedure directory"
        ));
    }
    let path = lower.split(['?', '#']).next().unwrap_or_default();
    if path.ends_with(".svg") {
        return Some("SVG files are not served (scriptable); inline the SVG in the panel".into());
    }
    None
}

/// `data:image/png;...` and the other rasters `RASTER_DATA` names.
fn is_raster_data(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix("data:image/") else {
        return false;
    };
    RASTER_DATA.iter().any(|t| {
        rest.strip_prefix(t)
            .is_some_and(|r| r.starts_with(';') || r.starts_with(','))
    })
}

fn has_scheme(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    let head = &s[..colon];
    !head.is_empty()
        && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// What the browser's sanitizer (`cleanCss` in
/// `packages/operator-ui/src/panel/sanitize.ts`) would drop from a
/// stylesheet or a `style=` (`inline`), said for the author. Braces are
/// counted the way the browser's tokenizer counts them (strings and
/// comments skipped): a stray `}` or a brace inside a string could close
/// the panel's scoping block and restyle the host page. Then anything
/// that loads a URL or runs something.
fn css_problem(css: &str, inline: bool) -> Option<String> {
    let mut out = String::with_capacity(css.len());
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            return Some("a backslash (a CSS escape) is not allowed: write the characters".into());
        }
        if let Some(q) = quote {
            if matches!(c, '\n' | '\r' | '\x0c' | '{' | '}') {
                return Some("a quoted string may not hold `{`, `}` or a line break".into());
            }
            if c == q {
                quote = None;
            }
            out.push(c);
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut last = ' ';
            loop {
                match chars.next() {
                    None => return Some("a comment is not closed (`*/`)".into()),
                    Some('/') if last == '*' => break,
                    Some(n) => last = n,
                }
            }
            out.push(' ');
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '}' if inline => {
                return Some("`style=` holds declarations only, no `{` or `}`".into())
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return Some("a `}` closes no `{` of the panel's CSS".into());
                }
            }
            _ => {}
        }
        out.push(c);
    }
    if quote.is_some() {
        return Some("a quoted string is not closed".into());
    }
    if depth != 0 {
        return Some("a `{` is not closed".into());
    }

    let lower = out.to_ascii_lowercase();
    if lower.contains("@import") {
        return Some("`@import` is not allowed: put the styles in the panel".into());
    }
    if lower.contains("expression(") || lower.contains("javascript:") {
        return Some("scripted CSS is not allowed".into());
    }
    // The sanitizer's `GLOBAL_AT_RULE`: they apply to the whole page.
    for rule in [
        "@font-face",
        "@charset",
        "@namespace",
        "@layer",
        "@property",
        "@page",
        "@scope",
        "@counter-style",
        "@font-feature-values",
        "@font-palette-values",
        "@view-transition",
    ] {
        if lower.contains(rule) {
            return Some(format!(
                "`{rule}` applies to the whole page: a panel's CSS cannot keep it"
            ));
        }
    }
    // Functions that load what a plain string names, without `url(`;
    // `-image(` inside a longer name (`-webkit-image(`) is its own entry.
    const LOADING: [&str; 7] = [
        "image-set(",
        "-webkit-image-set(",
        "image(",
        "-webkit-image(",
        "cross-fade(",
        "-webkit-cross-fade(",
        "src(",
    ];
    for function in LOADING {
        let loads = lower.match_indices(function).any(|(at, _)| {
            !lower[..at]
                .chars()
                .next_back()
                .is_some_and(|p| p.is_ascii_alphanumeric() || p == '_' || p == '-')
        });
        if loads {
            return Some(format!(
                "`{function})` loads a URL: use `url(#id)` or a raster `data:image/...`"
            ));
        }
    }
    let mut rest = lower.as_str();
    while let Some(p) = rest.find("url(") {
        let arg = rest[p + 4..].trim_start_matches([' ', '\'', '"']);
        if !(arg.starts_with('#') || is_raster_data(arg)) {
            return Some("`url()` may only point at `#id` or a raster `data:image/...`".into());
        }
        rest = &rest[p + 4..];
    }
    None
}

#[cfg(test)]
mod tests;
