//! YAML location tracking for precise error reporting.

use std::collections::HashMap;

pub struct YamlLocationMap {
    locations: HashMap<String, (usize, usize, usize)>,
    content: String,
}

impl YamlLocationMap {
    pub fn new(yaml_content: &str) -> Self {
        let mut locations = HashMap::new();

        for (line_num, line) in yaml_content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();

            if trimmed.starts_with("- name:") {
                if let Some(name_start) = line.find("name:") {
                    let value_start = name_start + 5;
                    if let Some(colon_pos) = line[name_start..].find(':') {
                        let value = line[name_start + colon_pos + 1..].trim();
                        let name = value.trim_matches('"').trim_matches('\'');
                        if !name.is_empty() {
                            let col = value_start + line[value_start..].find(name).unwrap_or(0) + 1;
                            locations
                                .insert(format!("phase:{}", name), (line_num, col, name.len()));
                        }
                    }
                }
            }

            if trimmed.starts_with("- key:") {
                if let Some(key_start) = line.find("key:") {
                    let value_start = key_start + 4;
                    if let Some(colon_pos) = line[key_start..].find(':') {
                        let value = line[key_start + colon_pos + 1..].trim();
                        let key = value.trim_matches('"').trim_matches('\'');
                        if !key.is_empty() {
                            let col = value_start + line[value_start..].find(key).unwrap_or(0) + 1;
                            locations.insert(format!("plug:{}", key), (line_num, col, key.len()));
                        }
                    }
                }
            }

            if trimmed == "plugs:" {
                locations.insert("section:plugs".to_string(), (line_num, indent + 1, 5));
            }

            if trimmed == "main:" {
                locations.insert("section:main".to_string(), (line_num, indent + 1, 4));
            }

            if let Some(colon_pos) = trimmed.find(':') {
                let key = trimmed[..colon_pos].trim();
                if !key.is_empty() && !key.starts_with('-') {
                    let col = indent + 1;
                    locations.insert(
                        format!("field:{}:{}", line_num, key),
                        (line_num, col, key.len()),
                    );
                }
            }

            locations.insert(
                format!("line:{}", line_num),
                (line_num, indent + 1, trimmed.len().max(1)),
            );
        }

        Self {
            locations,
            content: yaml_content.to_string(),
        }
    }

    pub fn get_phase_location(&self, phase_name: &str) -> Option<(usize, usize, usize)> {
        self.locations
            .get(&format!("phase:{}", phase_name))
            .copied()
    }

    pub fn get_plug_location(&self, plug_key: &str) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("plug:{}", plug_key)).copied()
    }

    pub fn get_section_location(&self, section: &str) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("section:{}", section)).copied()
    }

    pub fn get_line_location(&self, line: usize) -> Option<(usize, usize, usize)> {
        self.locations.get(&format!("line:{}", line)).copied()
    }

    pub fn find_python_field_for_phase(&self, phase_name: &str) -> Option<(usize, usize, usize)> {
        if let Some((phase_line, _, _)) = self.get_phase_location(phase_name) {
            for (line_num, line) in self.content.lines().enumerate().skip(phase_line) {
                let line_num = line_num + 1;
                let trimmed = line.trim();

                if trimmed.starts_with("- name:") && line_num != phase_line {
                    break;
                }

                if trimmed.starts_with("python:") {
                    let col = line.find("python:").unwrap_or(0) + 1;
                    return Some((line_num, col, 6));
                }
            }
        }
        None
    }
}
