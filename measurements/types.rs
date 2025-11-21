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

impl Measurement {
    pub fn get_key(&self) -> &str {
        &self.name
    }
}
