use crate::procedure::schema::{AggregationSpec, MultiDimensionalSpec, ValidatorSpec};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub enum MeasurementValue {
    Null,
    Boolean(bool),
    Numeric(f64),
    String(String),
    #[specta(skip)]
    Array(Vec<serde_json::Value>),
    MultiDimensional(MultiDimensionalSpec),
    #[specta(skip)]
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct Measurement {
    pub name: String,
    pub value: MeasurementValue,
    #[serde(default)]
    pub unit: Option<String>,
    pub timestamp: String,
    #[serde(default)]
    pub validators: Option<Vec<ValidatorSpec>>,
    #[serde(default)]
    pub aggregations: Option<Vec<AggregationSpec>>,
    #[serde(default)]
    pub description: Option<String>,
}

impl MeasurementValue {
    /// Extract the raw JSON value, unwrapping Python's tagged format (e.g. {"Numeric": 3.3} → 3.3)
    pub fn to_raw_json(&self) -> serde_json::Value {
        match self {
            MeasurementValue::Numeric(v) => serde_json::json!(v),
            MeasurementValue::Boolean(v) => serde_json::json!(v),
            MeasurementValue::String(v) => serde_json::json!(v),
            MeasurementValue::Array(v) => serde_json::json!(v),
            MeasurementValue::Null => serde_json::Value::Null,
            MeasurementValue::Object(map) => {
                // Unwrap Python's tagged enum format: {"Numeric": 3.3} → 3.3
                if map.len() == 1 {
                    for (key, val) in map {
                        match key.as_str() {
                            "Numeric" | "Boolean" | "String" | "Array" => return val.clone(),
                            "Null" => return serde_json::Value::Null,
                            _ => {}
                        }
                    }
                }
                serde_json::Value::Object(map.clone())
            }
            MeasurementValue::MultiDimensional(spec) => {
                serde_json::to_value(spec).unwrap_or(serde_json::Value::Null)
            }
        }
    }
}

impl Measurement {
    pub fn get_key(&self) -> &str {
        &self.name
    }
}
