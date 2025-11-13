use serde::{Deserialize, Deserializer, Serializer};

/// Parse human-readable duration strings like "5m", "30s", "1h30m", "500ms"
/// Supports: ms (milliseconds), s (seconds), m (minutes), h (hours)
/// Returns duration in milliseconds
/// Plain numbers without units are NOT supported - a unit must always be specified
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();

    let mut total_ms = 0f64;
    let mut current_num = String::new();
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        let ch = chars[i];

        if ch.is_ascii_digit() {
            current_num.push(ch);
            i += 1;
        } else if ch == 'm' && i + 1 < chars.len() && chars[i + 1] == 's' {
            // Handle "ms" (milliseconds)
            if current_num.is_empty() {
                return Err(format!("Invalid duration format: {}", s));
            }

            let num: f64 = current_num.parse()
                .map_err(|_| format!("Invalid number in duration: {}", current_num))?;

            total_ms += num;
            current_num.clear();
            i += 2; // Skip both 'm' and 's'
        } else if ch == 's' || ch == 'm' || ch == 'h' {
            if current_num.is_empty() {
                return Err(format!("Invalid duration format: {}", s));
            }

            let num: f64 = current_num.parse()
                .map_err(|_| format!("Invalid number in duration: {}", current_num))?;

            total_ms += match ch {
                's' => num * 1000.0,
                'm' => num * 60.0 * 1000.0,
                'h' => num * 3600.0 * 1000.0,
                _ => unreachable!(),
            };

            current_num.clear();
            i += 1;
        } else if !ch.is_whitespace() {
            return Err(format!(
                "Invalid character '{}' in duration '{}'. Valid units are: ms, s, m, h",
                ch, s
            ));
        } else {
            i += 1;
        }
    }

    if !current_num.is_empty() {
        return Err(format!(
            "Duration '{}' is missing a unit. Valid units are: ms (milliseconds), s (seconds), m (minutes), h (hours). Examples: 500ms, 30s, 5m, 1h30m",
            s
        ));
    }

    if total_ms == 0.0 {
        return Err("Duration cannot be zero".to_string());
    }

    Ok(total_ms.round() as u64)
}

/// Serialize duration as human-readable string (input is milliseconds)
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{}ms", ms);
    }

    let total_seconds = ms / 1000;
    let remaining_ms = ms % 1000;

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let secs = total_seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if secs > 0 {
        parts.push(format!("{}s", secs));
    }
    if remaining_ms > 0 {
        parts.push(format!("{}ms", remaining_ms));
    }

    parts.join("")
}

pub fn deserialize_duration<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let input: Option<String> = Option::deserialize(deserializer)?;
    match input {
        None => Ok(None),
        Some(s) => parse_duration(&s).map(Some).map_err(Error::custom),
    }
}

pub fn serialize_duration<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(ms) => serializer.serialize_str(&format_duration(*ms)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("500ms").unwrap(), 500);
        assert_eq!(parse_duration("1500ms").unwrap(), 1500);
        assert_eq!(parse_duration("30s").unwrap(), 30000); // 30 seconds = 30000ms
        assert_eq!(parse_duration("5m").unwrap(), 300000); // 5 minutes = 300000ms
        assert_eq!(parse_duration("1h").unwrap(), 3600000); // 1 hour = 3600000ms
        assert_eq!(parse_duration("1h30m").unwrap(), 5400000);
        assert_eq!(parse_duration("1h30m45s").unwrap(), 5445000);
        assert_eq!(parse_duration("1m500ms").unwrap(), 60500);
    }

    #[test]
    fn test_parse_duration_rejects_plain_numbers() {
        let result = parse_duration("300000");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing a unit"));

        let result = parse_duration("5");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing a unit"));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(30000), "30s");
        assert_eq!(format_duration(300000), "5m");
        assert_eq!(format_duration(3600000), "1h");
        assert_eq!(format_duration(5400000), "1h30m");
        assert_eq!(format_duration(5445000), "1h30m45s");
        assert_eq!(format_duration(60500), "1m500ms");
    }
}
