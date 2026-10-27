//! A tag scanner for operator panel files: enough HTML to find every
//! element, its attributes and its line, nothing more. The station reads
//! a panel only to judge it (forbidden elements, bindings, size); the
//! browser parses and sanitizes it for real before anything renders, so
//! this does not have to agree with HTML5 on malformed markup. It must
//! only never miss an element a browser would build: anything that looks
//! like a tag is reported as one.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Lowercase tag name.
    pub name: String,
    /// `(lowercase name, decoded value)` in source order; `None` value
    /// for a bare attribute (`<select multiple>`).
    pub attrs: Vec<(String, Option<String>)>,
    /// 1-based line of the `<`.
    pub line: u32,
}

/// Elements whose content is raw text, never markup.
const RAW_TEXT: [&str; 6] = ["script", "style", "textarea", "title", "xmp", "noscript"];

pub struct Scan {
    pub elements: Vec<Element>,
    /// `(line, text)` of every `<style>` element's content.
    pub styles: Vec<(u32, String)>,
}

pub fn scan(src: &str) -> Scan {
    let bytes = src.as_bytes();
    let mut elements = Vec::new();
    let mut styles = Vec::new();
    let mut i = 0;
    let mut line: u32 = 1;
    let advance = |from: usize, to: usize, line: &mut u32| {
        *line += bytes[from..to].iter().filter(|b| **b == b'\n').count() as u32;
    };

    while i < bytes.len() {
        let Some(off) = bytes[i..].iter().position(|b| *b == b'<') else {
            break;
        };
        advance(i, i + off, &mut line);
        i += off;
        let rest = &src[i..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            let end = comment
                .find("-->")
                .map(|e| i + 4 + e + 3)
                .unwrap_or(bytes.len());
            advance(i, end, &mut line);
            i = end;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") || rest.starts_with("</") {
            let end = rest.find('>').map(|e| i + e + 1).unwrap_or(bytes.len());
            advance(i, end, &mut line);
            i = end;
            continue;
        }
        let name_len = rest[1..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
            .count();
        if name_len == 0 || !rest.as_bytes()[1].is_ascii_alphabetic() {
            // `a < b` in text: not a tag.
            i += 1;
            continue;
        }
        let tag_line = line;
        let name = rest[1..1 + name_len].to_ascii_lowercase();
        let (attrs, consumed) = attributes(&rest[1 + name_len..]);
        let end = i + 1 + name_len + consumed;
        advance(i, end, &mut line);
        i = end;
        elements.push(Element {
            name: name.clone(),
            attrs,
            line: tag_line,
        });
        if RAW_TEXT.contains(&name.as_str()) {
            let close = format!("</{name}");
            let lower = src[i..].to_ascii_lowercase();
            let content_end = lower.find(&close).map(|e| i + e).unwrap_or(bytes.len());
            if name == "style" {
                styles.push((line, src[i..content_end].to_string()));
            }
            advance(i, content_end, &mut line);
            i = content_end;
        }
    }
    Scan { elements, styles }
}

/// Attributes up to and including the tag's `>`. Returns them and the
/// bytes consumed.
fn attributes(s: &str) -> (Vec<(String, Option<String>)>, usize) {
    let b = s.as_bytes();
    let mut attrs = Vec::new();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            return (attrs, i);
        }
        if b[i] == b'>' {
            return (attrs, i + 1);
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == start {
            // A lone `=`: skip it.
            i += 1;
            continue;
        }
        let name = s[start..i].to_ascii_lowercase();
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < b.len() && b[j] == b'=' {
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let (raw, next) = if j < b.len() && (b[j] == b'"' || b[j] == b'\'') {
                let q = b[j];
                let close = b[j + 1..].iter().position(|c| *c == q).map(|p| j + 1 + p);
                match close {
                    Some(c) => (&s[j + 1..c], c + 1),
                    None => (&s[j + 1..], b.len()),
                }
            } else {
                let e = b[j..]
                    .iter()
                    .position(|c| c.is_ascii_whitespace() || *c == b'>')
                    .map(|p| j + p)
                    .unwrap_or(b.len());
                (&s[j..e], e)
            };
            attrs.push((name, Some(decode_entities(raw))));
            i = next;
        } else {
            attrs.push((name, None));
        }
    }
}

/// The entities an author writes inside an attribute (`&quot;`, `&#60;`),
/// so the station reads the value the browser will.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        // Byte search: `;` is ASCII, so slicing at it is always a char
        // boundary, whatever precedes it.
        let window = &rest.as_bytes()[..rest.len().min(12)];
        let Some(semi) = window.iter().position(|b| *b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_elements_attributes_and_lines() {
        let s = scan("<div class=\"a\">\n  <button data-on\n    title='Volts &amp; amps'>Go</button>\n</div>");
        assert_eq!(s.elements.len(), 2);
        assert_eq!(s.elements[1].name, "button");
        assert_eq!(s.elements[1].line, 2);
        assert_eq!(
            s.elements[1].attrs,
            vec![
                ("data-on".to_string(), None),
                ("title".to_string(), Some("Volts & amps".to_string())),
            ]
        );
    }

    #[test]
    fn style_and_script_content_is_not_markup() {
        let s = scan("<style>a > b { }</style><script>if (a<b) {}</script><p>x</p>");
        let names: Vec<&str> = s.elements.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["style", "script", "p"]);
        assert_eq!(s.styles[0].1, "a > b { }");
    }

    #[test]
    fn comments_and_text_comparisons_are_skipped() {
        let s = scan("<!-- <script> -->1 < 2 <svg viewBox='0 0 1 1'/>");
        assert_eq!(s.elements.len(), 1);
        assert_eq!(s.elements[0].name, "svg");
    }

    #[test]
    fn mixed_case_is_lowered() {
        let s = scan("<SCRIPT SRC=x></SCRIPT><foreignObject/>");
        assert_eq!(s.elements[0].name, "script");
        assert_eq!(s.elements[0].attrs[0].0, "src");
        assert_eq!(s.elements[1].name, "foreignobject");
    }

    #[test]
    fn decodes_numeric_entities() {
        assert_eq!(decode_entities("&#60;&#x3e;&amp;x&bogus;"), "<>&x&bogus;");
    }
}
