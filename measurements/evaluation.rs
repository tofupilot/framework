use crate::measurements::types::{Measurement, MeasurementValue};
use crate::procedure::schema::{
    AggregationValue, AxisSpec, MeasurementSpec, PhaseDefinition, ValidatorExpectedValue,
    ValidatorOutcome, ValidatorSpec,
};
use serde_json::Value;

/// Convert MeasurementValue to JSON Value for evaluation
fn measurement_value_to_json(value: &MeasurementValue) -> Value {
    match value {
        MeasurementValue::Numeric(n) => Value::from(*n),
        MeasurementValue::String(s) => Value::String(s.clone()),
        MeasurementValue::Boolean(b) => Value::Bool(*b),
        MeasurementValue::Array(arr) => Value::Array(arr.clone()),
        MeasurementValue::MultiDimensional(_) => Value::Null,
        MeasurementValue::Object(obj) => {
            // Handle tagged enum serialization from Python: {"Numeric": 3.3} -> 3.3
            if obj.len() == 1 {
                if let Some(value) = obj.get("Numeric") {
                    return value.clone();
                } else if let Some(value) = obj.get("String") {
                    return value.clone();
                } else if let Some(value) = obj.get("Boolean") {
                    return value.clone();
                } else if let Some(value) = obj.get("Array") {
                    return value.clone();
                } else if obj.get("Null").is_some() {
                    return Value::Null;
                }
            }
            Value::Object(obj.clone())
        }
        MeasurementValue::Null => Value::Null,
    }
}

/// Convert ValidatorExpectedValue to JSON Value for evaluation
fn expected_value_to_json(value: &ValidatorExpectedValue) -> Value {
    match value {
        ValidatorExpectedValue::Null => Value::Null,
        ValidatorExpectedValue::Boolean(b) => Value::Bool(*b),
        ValidatorExpectedValue::Number(n) => Value::from(*n),
        ValidatorExpectedValue::String(s) => Value::String(s.clone()),
        ValidatorExpectedValue::NumberArray(arr) => {
            Value::Array(arr.iter().map(|n| Value::from(*n)).collect())
        }
        ValidatorExpectedValue::StringArray(arr) => {
            Value::Array(arr.iter().map(|s| Value::String(s.clone())).collect())
        }
        ValidatorExpectedValue::MixedArray(arr) => Value::Array(arr.clone()),
        ValidatorExpectedValue::Object(obj) => Value::Object(obj.clone()),
    }
}

/// Convert AggregationValue to JSON Value for evaluation
fn aggregation_value_to_json(value: &AggregationValue) -> Value {
    match value {
        AggregationValue::Number(n) => Value::from(*n),
        AggregationValue::String(s) => Value::String(s.clone()),
        AggregationValue::Boolean(b) => Value::Bool(*b),
        AggregationValue::Object(obj) => Value::Object(obj.clone()),
    }
}

/// Convert JSON Value to AggregationValue
fn json_to_aggregation_value(value: Value) -> AggregationValue {
    match value {
        Value::Number(n) => AggregationValue::Number(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => AggregationValue::String(s),
        Value::Bool(b) => AggregationValue::Boolean(b),
        Value::Object(obj) => AggregationValue::Object(obj),
        _ => AggregationValue::Number(0.0), // Default for null or array
    }
}

/// Auto-evaluate measurements by:
/// 1. Merging YAML-defined validators with Python-provided validators
/// 2. Auto-evaluating validators that have UNSET outcome
/// 3. Computing aggregation values when needed
/// 4. Auto-evaluating aggregation validators
pub fn auto_evaluate_measurements(
    mut measurements: Vec<Measurement>,
    phase_config: &PhaseDefinition,
) -> Vec<Measurement> {
    for measurement in &mut measurements {
        // Convert tagged enum from Python: {"MultiDimensional": {...}} -> MultiDimensional(...)
        if let MeasurementValue::Object(obj) = &measurement.value {
            if obj.len() == 1 && obj.contains_key("MultiDimensional") {
                if let Some(multidim_value) = obj.get("MultiDimensional") {
                    if let Ok(multidim_spec) = serde_json::from_value::<
                        crate::procedure::schema::MultiDimensionalSpec,
                    >(multidim_value.clone())
                    {
                        measurement.value = MeasurementValue::MultiDimensional(multidim_spec);
                    }
                }
            }
        }

        // Get YAML config for this measurement if it exists
        if let Some(yaml_config) = phase_config
            .measurements
            .iter()
            .find(|m| m.key == measurement.name)
        {
            // Merge unit from YAML if Python didn't provide it
            if measurement.unit.is_none() && yaml_config.unit.is_some() {
                measurement.unit = yaml_config.unit.clone();
            }

            // Merge description from YAML if Python didn't provide it
            if measurement.description.is_none() && yaml_config.description.is_some() {
                measurement.description = yaml_config.description.clone();
            }

            // Merge and evaluate validators
            merge_and_evaluate_validators(measurement, yaml_config);

            // Merge and evaluate aggregations
            merge_and_evaluate_aggregations(measurement, yaml_config);

            // Handle MultiDimensional measurements - evaluate axis validators/aggregations
            if matches!(measurement.value, MeasurementValue::MultiDimensional(_)) {
                merge_and_evaluate_multidim_axes(measurement, yaml_config);
            }
        } else {
            // No YAML config, but still evaluate any Python-provided validators/aggregations
            evaluate_measurement_validators(measurement);
            evaluate_aggregations_only(measurement);
        }
    }
    measurements
}

/// Check if all measurements passed validation
/// Any validator failure causes measurements to fail
pub fn check_all_measurements_pass(measurements: &[Measurement]) -> bool {
    measurements.iter().all(check_measurement_pass)
}

fn check_measurement_pass(measurement: &Measurement) -> bool {
    if let Some(validators) = &measurement.validators {
        let has_fail = validators
            .iter()
            .any(|v| v.outcome == Some(ValidatorOutcome::Fail));
        if has_fail {
            return false;
        }
    }

    if let Some(aggregations) = &measurement.aggregations {
        for agg in aggregations {
            if let Some(validators) = &agg.validators {
                let has_fail = validators
                    .iter()
                    .any(|v| v.outcome == Some(ValidatorOutcome::Fail));
                if has_fail {
                    return false;
                }
            }
        }
    }

    if let MeasurementValue::MultiDimensional(multidim) = &measurement.value {
        if let Some(validators) = &multidim.x_axis.validators {
            let has_fail = validators
                .iter()
                .any(|v| v.outcome == Some(ValidatorOutcome::Fail));
            if has_fail {
                return false;
            }
        }

        for y_axis in &multidim.y_axis {
            if let Some(validators) = &y_axis.validators {
                let has_fail = validators
                    .iter()
                    .any(|v| v.outcome == Some(ValidatorOutcome::Fail));
                if has_fail {
                    return false;
                }
            }

            if let Some(aggregations) = &y_axis.aggregations {
                for agg in aggregations {
                    if let Some(validators) = &agg.validators {
                        let has_fail = validators
                            .iter()
                            .any(|v| v.outcome == Some(ValidatorOutcome::Fail));
                        if has_fail {
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

/// Merge YAML validators with Python validators and evaluate them
fn merge_and_evaluate_validators(measurement: &mut Measurement, yaml_config: &MeasurementSpec) {
    // Start with YAML validators if they exist
    let mut all_validators = if let Some(yaml_validators) = &yaml_config.validators {
        yaml_validators.clone()
    } else {
        Vec::new()
    };

    // Merge Python validator outcomes with YAML validators
    if let Some(python_validators) = &measurement.validators {
        for py_val in python_validators {
            // Find matching YAML validator by operator
            let matching_idx = all_validators
                .iter()
                .position(|yaml_val| yaml_val.operator == py_val.operator);

            if let Some(idx) = matching_idx {
                if py_val.outcome.is_some() {
                    all_validators[idx].outcome = py_val.outcome.clone();
                }
            } else {
                all_validators.push(py_val.clone());
            }
        }
    }

    // Auto-evaluate validators with UNSET outcome
    for validator in &mut all_validators {
        if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset) {
            let json_value = measurement_value_to_json(&measurement.value);
            validator.outcome = Some(evaluate_single_validator(validator, &json_value));
        }
    }

    // Update measurement with merged and evaluated validators
    measurement.validators = if all_validators.is_empty() {
        None
    } else {
        Some(all_validators)
    };
}

/// Evaluate validators that don't have YAML config
fn evaluate_measurement_validators(measurement: &mut Measurement) {
    if let Some(validators) = &mut measurement.validators {
        let json_value = measurement_value_to_json(&measurement.value);
        for validator in validators {
            if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset) {
                validator.outcome = Some(evaluate_single_validator(validator, &json_value));
            }
        }
    }
}

/// Auto-evaluate a single validator based on operator and expected value
fn evaluate_single_validator(validator: &ValidatorSpec, actual_value: &Value) -> ValidatorOutcome {
    // If outcome is already set (from Python), use it
    if let Some(outcome) = &validator.outcome {
        if *outcome != ValidatorOutcome::Unset {
            return outcome.clone();
        }
    }

    // Require operator + expected_value for auto-evaluation
    let (operator, expected) = match (&validator.operator, &validator.expected_value) {
        (Some(op), Some(exp)) => (op, expected_value_to_json(exp)),
        _ => return ValidatorOutcome::Unset,
    };

    // Evaluate based on operator
    let pass = match operator.as_str() {
        "==" => compare_values(actual_value, &expected, |a, e| a == e),
        "!=" => compare_values(actual_value, &expected, |a, e| a != e),
        ">" => compare_numeric(actual_value, &expected, |a, e| a > e),
        ">=" => compare_numeric(actual_value, &expected, |a, e| a >= e),
        "<" => compare_numeric(actual_value, &expected, |a, e| a < e),
        "<=" => compare_numeric(actual_value, &expected, |a, e| a <= e),
        "in" => check_membership(actual_value, &expected, true),
        "not in" => check_membership(actual_value, &expected, false),
        "matches" => check_regex_match(actual_value, &expected),
        _ => false,
    };

    if pass {
        ValidatorOutcome::Pass
    } else {
        ValidatorOutcome::Fail
    }
}

/// Compare two JSON values for equality/inequality
fn compare_values<F>(actual: &Value, expected: &Value, op: F) -> bool
where
    F: Fn(&Value, &Value) -> bool,
{
    // Handle array with single element as scalar
    let expected_scalar = if let Some(arr) = expected.as_array() {
        if arr.len() == 1 {
            &arr[0]
        } else {
            expected
        }
    } else {
        expected
    };

    op(actual, expected_scalar)
}

/// Compare numeric values
fn compare_numeric<F>(actual: &Value, expected: &Value, op: F) -> bool
where
    F: Fn(f64, f64) -> bool,
{
    // Extract numeric value from actual
    let actual_num = match actual {
        Value::Number(n) => n.as_f64(),
        _ => None,
    };

    // Extract numeric value from expected (handle single-element array)
    let expected_num = if let Some(n) = expected.as_f64() {
        Some(n)
    } else if let Some(arr) = expected.as_array() {
        if arr.len() == 1 {
            arr[0].as_f64()
        } else {
            None
        }
    } else {
        None
    };

    match (actual_num, expected_num) {
        (Some(a), Some(e)) => op(a, e),
        _ => false,
    }
}

/// Check if value is in/not in array
fn check_membership(actual: &Value, expected: &Value, should_be_in: bool) -> bool {
    let arr = match expected.as_array() {
        Some(a) => a,
        None => return false,
    };

    let is_member = arr.iter().any(|item| actual == item);
    if should_be_in {
        is_member
    } else {
        !is_member
    }
}

/// Check if string matches regex pattern
fn check_regex_match(actual: &Value, pattern: &Value) -> bool {
    let actual_str = match actual.as_str() {
        Some(s) => s,
        None => return false,
    };

    // Extract pattern string (handle single-element array)
    let pattern_str = if let Some(s) = pattern.as_str() {
        s
    } else if let Some(arr) = pattern.as_array() {
        if arr.len() == 1 {
            match arr[0].as_str() {
                Some(s) => s,
                None => return false,
            }
        } else {
            return false;
        }
    } else {
        return false;
    };

    // Compile and match regex
    match regex::Regex::new(pattern_str) {
        Ok(re) => re.is_match(actual_str),
        Err(_) => false,
    }
}

/// Merge YAML aggregations with Python aggregations and evaluate them
fn merge_and_evaluate_aggregations(measurement: &mut Measurement, yaml_config: &MeasurementSpec) {
    // Start with YAML aggregations if they exist
    let mut all_aggregations = if let Some(yaml_aggregations) = &yaml_config.aggregations {
        yaml_aggregations.clone()
    } else {
        Vec::new()
    };

    // Add/merge Python aggregations if they exist
    if let Some(python_aggregations) = &measurement.aggregations {
        for py_agg in python_aggregations {
            // Check if Python overrides a YAML aggregation (same type)
            let should_override = all_aggregations
                .iter()
                .position(|yaml_agg| yaml_agg.aggregation_type == py_agg.aggregation_type);

            if let Some(idx) = should_override {
                // Python aggregation overrides YAML aggregation
                // But preserve YAML validators and unit if Python doesn't provide them
                let yaml_agg = &all_aggregations[idx];
                let mut merged_agg = py_agg.clone();

                // Preserve unit from YAML if Python doesn't provide it
                if merged_agg.unit.is_none() && yaml_agg.unit.is_some() {
                    merged_agg.unit = yaml_agg.unit.clone();
                }

                // If Python doesn't provide validators, use YAML validators
                if merged_agg.validators.is_none() && yaml_agg.validators.is_some() {
                    merged_agg.validators = yaml_agg.validators.clone();
                } else if let (Some(py_vals), Some(yaml_vals)) =
                    (&merged_agg.validators, &yaml_agg.validators)
                {
                    // Merge validators similar to measurement validators
                    let mut merged_validators = yaml_vals.clone();
                    for py_val in py_vals {
                        // Check if Python validator overrides YAML validator
                        let override_idx = merged_validators
                            .iter()
                            .position(|yaml_val| yaml_val.operator == py_val.operator);

                        if let Some(idx) = override_idx {
                            // Python validator matches YAML validator
                            // Merge fields: prefer Python if provided, otherwise use YAML
                            if py_val.outcome.is_some() {
                                merged_validators[idx].outcome = py_val.outcome.clone();
                            }
                            // Only override expected_value if Python provides a non-null value
                            if let Some(ref exp_val) = py_val.expected_value {
                                if !matches!(exp_val, ValidatorExpectedValue::Null) {
                                    merged_validators[idx].expected_value =
                                        py_val.expected_value.clone();
                                }
                            }
                            if py_val.expression.is_some() {
                                merged_validators[idx].expression = py_val.expression.clone();
                            }
                            if py_val.operator.is_some() {
                                merged_validators[idx].operator = py_val.operator.clone();
                            }
                        } else {
                            merged_validators.push(py_val.clone());
                        }
                    }
                    merged_agg.validators = Some(merged_validators);
                }

                all_aggregations[idx] = merged_agg;
            } else {
                // Python aggregation is additional
                all_aggregations.push(py_agg.clone());
            }
        }
    }

    // Now evaluate all aggregations
    let json_value = measurement_value_to_json(&measurement.value);
    for aggregation in &mut all_aggregations {
        // Compute aggregation value if not already set
        if aggregation.value.is_none() {
            let computed_value =
                compute_aggregation_value(&json_value, &aggregation.aggregation_type);
            aggregation.value = Some(json_to_aggregation_value(computed_value.clone()));
        }

        // Evaluate aggregation validators
        if let Some(validators) = &mut aggregation.validators {
            for validator in validators {
                if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset)
                {
                    if let Some(agg_value) = &aggregation.value {
                        let json_agg_value = aggregation_value_to_json(agg_value);
                        validator.outcome =
                            Some(evaluate_single_validator(validator, &json_agg_value));
                    }
                }
            }
        }

        // Set aggregation outcome based on validators
        aggregation.outcome = Some(determine_aggregation_outcome(&aggregation.validators));
    }

    // Update measurement with merged and evaluated aggregations
    measurement.aggregations = if all_aggregations.is_empty() {
        None
    } else {
        Some(all_aggregations)
    };
}

/// Evaluate aggregations only (when no YAML config exists)
fn evaluate_aggregations_only(measurement: &mut Measurement) {
    if let Some(aggregations) = &mut measurement.aggregations {
        let json_value = measurement_value_to_json(&measurement.value);
        for aggregation in aggregations {
            // Compute aggregation value if not already set
            if aggregation.value.is_none() {
                let computed_value =
                    compute_aggregation_value(&json_value, &aggregation.aggregation_type);
                aggregation.value = Some(json_to_aggregation_value(computed_value.clone()));
            }

            // Evaluate aggregation validators
            if let Some(validators) = &mut aggregation.validators {
                for validator in validators {
                    if validator.outcome.is_none()
                        || validator.outcome == Some(ValidatorOutcome::Unset)
                    {
                        if let Some(agg_value) = &aggregation.value {
                            let json_agg_value = aggregation_value_to_json(agg_value);
                            validator.outcome =
                                Some(evaluate_single_validator(validator, &json_agg_value));
                        }
                    }
                }
            }

            // Set aggregation outcome based on validators
            aggregation.outcome = Some(determine_aggregation_outcome(&aggregation.validators));
        }
    }
}

/// Compute aggregation value based on type
fn compute_aggregation_value(value: &Value, aggregation_type: &str) -> Value {
    // Handle array values
    if let Value::Array(arr) = value {
        let numbers: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();

        if numbers.is_empty() {
            return Value::Null;
        }

        match aggregation_type {
            "mean" => {
                let sum: f64 = numbers.iter().sum();
                Value::from(sum / numbers.len() as f64)
            }
            "min" => numbers
                .iter()
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|&v| Value::from(v))
                .unwrap_or(Value::Null),
            "max" => numbers
                .iter()
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|&v| Value::from(v))
                .unwrap_or(Value::Null),
            "sum" => {
                let sum: f64 = numbers.iter().sum();
                Value::from(sum)
            }
            "count" => Value::from(numbers.len() as i64),
            "std" => {
                if numbers.len() < 2 {
                    return Value::from(0.0);
                }
                let mean = numbers.iter().sum::<f64>() / numbers.len() as f64;
                let variance =
                    numbers.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / numbers.len() as f64;
                Value::from(variance.sqrt())
            }
            _ => Value::Null,
        }
    } else {
        // For non-array values, some aggregations still make sense
        match aggregation_type {
            "count" => Value::from(1),
            _ => value.clone(), // Return the value itself for other aggregations
        }
    }
}

/// Determine aggregation outcome based on its validators
fn determine_aggregation_outcome(validators: &Option<Vec<ValidatorSpec>>) -> ValidatorOutcome {
    match validators {
        Some(vals) if !vals.is_empty() => {
            // Check if any validator failed
            let has_fail = vals
                .iter()
                .any(|v| v.outcome == Some(ValidatorOutcome::Fail));

            if has_fail {
                ValidatorOutcome::Fail
            } else {
                // Check if all validators passed
                let all_pass = vals.iter().all(|v| {
                    v.outcome == Some(ValidatorOutcome::Pass)
                        || v.outcome == Some(ValidatorOutcome::Unset)
                });

                if all_pass {
                    ValidatorOutcome::Pass
                } else {
                    ValidatorOutcome::Fail
                }
            }
        }
        _ => ValidatorOutcome::Unset,
    }
}

/// Merge and evaluate validators and aggregations for MultiDimensional measurement axes
fn merge_and_evaluate_multidim_axes(measurement: &mut Measurement, yaml_config: &MeasurementSpec) {
    if let MeasurementValue::MultiDimensional(ref mut multidim_spec) = measurement.value {
        // Try new style first (multi_dimensional object)
        if let Some(yaml_multidim) = &yaml_config.multi_dimensional {
            // Merge title from YAML if Python didn't provide it
            if multidim_spec.title.is_none() && yaml_multidim.title.is_some() {
                multidim_spec.title = yaml_multidim.title.clone();
            }

            // Evaluate x_axis
            evaluate_axis(&mut multidim_spec.x_axis, &yaml_multidim.x_axis);

            // Evaluate each y_axis
            for (idx, python_y_axis) in multidim_spec.y_axis.iter_mut().enumerate() {
                if let Some(yaml_y_axis) = yaml_multidim.y_axis.get(idx) {
                    evaluate_axis(python_y_axis, yaml_y_axis);
                }
            }
        }
        // Fall back to old style (x_axis and y_axis directly in MeasurementSpec)
        else if yaml_config.x_axis.is_some() || yaml_config.y_axis.is_some() {
            // Merge title from YAML if Python didn't provide it
            if multidim_spec.title.is_none() && yaml_config.title.is_some() {
                multidim_spec.title = yaml_config.title.clone();
            }

            // Evaluate x_axis if YAML provides it
            if let Some(yaml_x_axis) = &yaml_config.x_axis {
                evaluate_axis(&mut multidim_spec.x_axis, yaml_x_axis);
            }

            // Evaluate y_axis if YAML provides it
            if let Some(yaml_y_axes) = &yaml_config.y_axis {
                for (idx, python_y_axis) in multidim_spec.y_axis.iter_mut().enumerate() {
                    if let Some(yaml_y_axis) = yaml_y_axes.get(idx) {
                        evaluate_axis(python_y_axis, yaml_y_axis);
                    }
                }
            }
        }
    }
}

/// Evaluate validators and aggregations for a single axis
fn evaluate_axis(python_axis: &mut AxisSpec, yaml_axis: &AxisSpec) {
    // Merge unit from YAML if Python didn't provide it
    if python_axis.unit.is_none() && yaml_axis.unit.is_some() {
        python_axis.unit = yaml_axis.unit.clone();
    }

    // Merge legend from YAML if Python didn't provide it
    if python_axis.legend.is_none() && yaml_axis.legend.is_some() {
        python_axis.legend = yaml_axis.legend.clone();
    }

    // Merge and evaluate axis validators
    let mut all_validators = if let Some(yaml_validators) = &yaml_axis.validators {
        yaml_validators.clone()
    } else {
        Vec::new()
    };

    // Merge Python validator outcomes with YAML validators
    if let Some(python_validators) = &python_axis.validators {
        for py_val in python_validators {
            let matching_idx = all_validators
                .iter()
                .position(|yaml_val| yaml_val.operator == py_val.operator);

            if let Some(idx) = matching_idx {
                if py_val.outcome.is_some() {
                    all_validators[idx].outcome = py_val.outcome.clone();
                }
            } else {
                all_validators.push(py_val.clone());
            }
        }
    }

    // Auto-evaluate axis validators with UNSET outcome
    // Convert axis data to JSON for validation (only if data exists)
    if let Some(data) = &python_axis.data {
        let axis_json_value = match data {
            crate::procedure::schema::AxisData::Numeric(nums) => {
                Value::Array(nums.iter().map(|n| Value::from(*n)).collect())
            }
            crate::procedure::schema::AxisData::String(strs) => {
                Value::Array(strs.iter().map(|s| Value::String(s.clone())).collect())
            }
        };

        for validator in &mut all_validators {
            if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset) {
                validator.outcome = Some(evaluate_single_validator(validator, &axis_json_value));
            }
        }
    }

    python_axis.validators = if all_validators.is_empty() {
        None
    } else {
        Some(all_validators)
    };

    // Merge and evaluate axis aggregations
    let mut all_aggregations = if let Some(yaml_aggregations) = &yaml_axis.aggregations {
        yaml_aggregations.clone()
    } else {
        Vec::new()
    };

    // Add/merge Python aggregations
    if let Some(python_aggregations) = &python_axis.aggregations {
        for py_agg in python_aggregations {
            let should_override = all_aggregations
                .iter()
                .position(|yaml_agg| yaml_agg.aggregation_type == py_agg.aggregation_type);

            if let Some(idx) = should_override {
                let yaml_agg = &all_aggregations[idx];
                let mut merged_agg = py_agg.clone();

                // Preserve unit from YAML if Python doesn't provide it
                if merged_agg.unit.is_none() && yaml_agg.unit.is_some() {
                    merged_agg.unit = yaml_agg.unit.clone();
                }

                // Merge validators: if Python provides validators, merge with YAML validators
                if merged_agg.validators.is_none() && yaml_agg.validators.is_some() {
                    // Python provides no validators, use YAML validators
                    merged_agg.validators = yaml_agg.validators.clone();
                } else if let (Some(py_vals), Some(yaml_vals)) =
                    (&merged_agg.validators, &yaml_agg.validators)
                {
                    // Both Python and YAML provide validators, merge them
                    let mut merged_validators = yaml_vals.clone();
                    for py_val in py_vals {
                        let override_idx = merged_validators
                            .iter()
                            .position(|yaml_val| yaml_val.operator == py_val.operator);

                        if let Some(idx) = override_idx {
                            // Python validator matches YAML validator
                            // Merge fields: prefer Python if provided, otherwise use YAML
                            if py_val.outcome.is_some() {
                                merged_validators[idx].outcome = py_val.outcome.clone();
                            }
                            // Only override expected_value if Python provides a non-null value
                            if let Some(ref exp_val) = py_val.expected_value {
                                if !matches!(exp_val, ValidatorExpectedValue::Null) {
                                    merged_validators[idx].expected_value =
                                        py_val.expected_value.clone();
                                }
                            }
                            if py_val.expression.is_some() {
                                merged_validators[idx].expression = py_val.expression.clone();
                            }
                            if py_val.operator.is_some() {
                                merged_validators[idx].operator = py_val.operator.clone();
                            }
                        } else {
                            // Python provides a new validator
                            merged_validators.push(py_val.clone());
                        }
                    }
                    merged_agg.validators = Some(merged_validators);
                }

                all_aggregations[idx] = merged_agg;
            } else {
                all_aggregations.push(py_agg.clone());
            }
        }
    }

    // Evaluate all axis aggregations
    for aggregation in &mut all_aggregations {
        // Axis aggregations are already computed by Python, just evaluate validators
        if let Some(validators) = &mut aggregation.validators {
            for validator in validators {
                if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset)
                {
                    if let Some(agg_value) = &aggregation.value {
                        let json_agg_value = aggregation_value_to_json(agg_value);
                        validator.outcome =
                            Some(evaluate_single_validator(validator, &json_agg_value));
                    }
                }
            }
        }

        // Set aggregation outcome based on validators
        aggregation.outcome = Some(determine_aggregation_outcome(&aggregation.validators));
    }

    python_axis.aggregations = if all_aggregations.is_empty() {
        None
    } else {
        Some(all_aggregations)
    };
}
