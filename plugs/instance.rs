use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum PlugInstanceId {
    #[serde(rename = "all")]
    All { plug_name: String },
    #[serde(rename = "each")]
    Each { plug_name: String, slot_id: String },
}

impl PlugInstanceId {
    pub fn all(plug_name: impl Into<String>) -> Self {
        Self::All {
            plug_name: plug_name.into(),
        }
    }

    pub fn each(plug_name: impl Into<String>, slot_id: impl Into<String>) -> Self {
        Self::Each {
            plug_name: plug_name.into(),
            slot_id: slot_id.into(),
        }
    }

    pub fn plug_name(&self) -> &str {
        match self {
            Self::All { plug_name } => plug_name,
            Self::Each { plug_name, .. } => plug_name,
        }
    }

    pub fn slot_id(&self) -> Option<&str> {
        match self {
            Self::All { .. } => None,
            Self::Each { slot_id, .. } => Some(slot_id),
        }
    }

    pub fn to_key(&self) -> String {
        match self {
            Self::All { plug_name } => format!("all:{}", plug_name),
            Self::Each { plug_name, slot_id } => format!("each:{}:{}", plug_name, slot_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_id_all() {
        let id = PlugInstanceId::all("Multimeter");
        assert_eq!(id.plug_name(), "Multimeter");
        assert_eq!(id.slot_id(), None);
        assert_eq!(id.to_key(), "all:Multimeter");
    }

    #[test]
    fn test_instance_id_each() {
        let id = PlugInstanceId::each("PowerSupply", "slot1");
        assert_eq!(id.plug_name(), "PowerSupply");
        assert_eq!(id.slot_id(), Some("slot1"));
        assert_eq!(id.to_key(), "each:PowerSupply:slot1");
    }
}
