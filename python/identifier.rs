pub fn to_python_identifier(s: &str) -> String {
    let transliterate = |c: char| -> char {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            'ñ' => 'n',
            'ç' => 'c',
            'æ' => 'a',
            'œ' => 'o',
            _ => c,
        }
    };

    let mut result: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(transliterate)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else if c.is_whitespace() || c == '-' {
                '_'
            } else {
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if result.is_empty() {
        return result;
    }

    if result.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        result.insert(0, '_');
    }

    result
}

pub fn is_valid_python_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();

    if let Some(first) = chars.next() {
        if !first.is_ascii_alphabetic() && first != '_' {
            return false;
        }
    }

    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
