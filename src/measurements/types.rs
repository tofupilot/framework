use crate::procedure::schema::{
    AggregationSpec, MultiDimensionalSpec, ValidatorOutcome, ValidatorSpec,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MeasurementValue {
    Null,
    Boolean(bool),
    Numeric(f64),
    String(String),
    Array(Vec<serde_json::Value>),
    MultiDimensional(MultiDimensionalSpec),
    Object(serde_json::Map<String, serde_json::Value>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Roll-up of the measurement's pass/fail state, computed by
    /// `compute_measurement_outcome`: any FAIL ⇒ FAIL; every check
    /// passed ⇒ PASS, a recorded value with no validators included (as in
    /// OpenHTF, where `all([])` is True); UNSET when no value was recorded
    /// yet or a check couldn't run. The live broadcast emits UNSET while a phase is
    /// running and `evaluate_measurements` finalizes it at phase end.
    #[serde(default = "default_outcome")]
    pub outcome: ValidatorOutcome,
}

fn default_outcome() -> ValidatorOutcome {
    ValidatorOutcome::Unset
}

impl MeasurementValue {
    /// The value as plain JSON, without the variant tag. An `Object` is
    /// always a dict the phase recorded: the worker tags each value once
    /// (`{"Object": {...}}`), so a one-key dict such as `{"Numeric": 1}`
    /// is the user's data, not a tag.
    pub fn to_raw_json(&self) -> serde_json::Value {
        match self {
            MeasurementValue::Numeric(v) => serde_json::json!(v),
            MeasurementValue::Boolean(v) => serde_json::json!(v),
            MeasurementValue::String(v) => serde_json::json!(v),
            MeasurementValue::Array(v) => serde_json::json!(v),
            MeasurementValue::Null => serde_json::Value::Null,
            MeasurementValue::Object(map) => serde_json::Value::Object(map.clone()),
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
