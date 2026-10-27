use crate::measurements::issue::{DeclaredPart, MatchIssue};
use crate::measurements::types::{Measurement, MeasurementValue};
use crate::procedure::schema::{
    AggregationSpec, AggregationValue, AxisSpec, MeasurementSpec, PhaseDefinition,
    ValidatorExpectedValue, ValidatorOperator, ValidatorOutcome, ValidatorSpec,
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
        MeasurementValue::Object(obj) => Value::Object(obj.clone()),
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

/// Auto-evaluate measurements by:
/// 1. Matching each recorded measurement, y axis and aggregation to its
///    declaration (`key` for measurements and axes, `type` for
///    aggregations) and merging the declared fields in
/// 2. Auto-evaluating validators that have UNSET outcome
/// 3. Evaluating aggregation validators (values must be provided by the caller)
///
/// Declared parts that were never recorded are returned as issues. Their
/// validators fail, since there is no value to meet them; a part without
/// validators leaves its measurement UNSET unless something in it failed.
/// Whole measurements that were never recorded are the caller's to add,
/// through `complete_declared_measurements`, once every source of values
/// (Python, operator UI) has been merged.
pub fn auto_evaluate_measurements(
    mut measurements: Vec<Measurement>,
    phase_config: &PhaseDefinition,
) -> (Vec<Measurement>, Vec<MatchIssue>) {
    let mut issues = Vec::new();
    for measurement in &mut measurements {
        let first_issue = issues.len();

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

            evaluate_declared_validators(measurement, yaml_config, &mut issues);

            let missing = merge_and_evaluate_aggregations(measurement, yaml_config, &mut issues);
            issues.extend(
                missing
                    .into_iter()
                    .map(|aggregation_type| MatchIssue::NotRecorded {
                        measurement: measurement.name.clone(),
                        part: DeclaredPart::Aggregation {
                            axis: None,
                            aggregation_type,
                        },
                    }),
            );

            // Handle MultiDimensional measurements - evaluate axis validators/aggregations
            if matches!(measurement.value, MeasurementValue::MultiDimensional(_)) {
                issues.extend(merge_and_evaluate_multidim_axes(measurement, yaml_config));
            }
        } else {
            // No YAML config, but still evaluate any Python-provided validators/aggregations
            issues.extend(evaluate_measurement_validators(measurement));
            evaluate_aggregations_only(measurement, &mut issues);
        }

        measurement.outcome = compute_measurement_outcome(measurement);
        // A declared part that was never recorded leaves the measurement
        // not fully checked; a FAIL, its own validators' included, wins.
        if issues.len() > first_issue && measurement.outcome != ValidatorOutcome::Fail {
            measurement.outcome = ValidatorOutcome::Unset;
        }
    }
    (measurements, issues)
}

/// Add every declared measurement the phase never recorded as an empty
/// measurement, so it shows up next to the ones that were recorded instead
/// of vanishing from the run. With no value, its validators fail; one
/// without validators is UNSET. Call once, after all sources of values for
/// the phase have been merged.
pub fn complete_declared_measurements(
    measurements: &mut Vec<Measurement>,
    declared: &[MeasurementSpec],
) -> Vec<MatchIssue> {
    let mut issues = Vec::new();
    for spec in declared {
        if measurements.iter().any(|m| m.name == spec.key) {
            continue;
        }
        // Kept so the dashboard shows which limits had no value to meet.
        // The axes of a multi-dimensional declaration can't be carried
        // without data, so their limits are folded onto the measurement.
        let axes = spec.x_axis.iter().chain(spec.y_axis.iter().flatten());
        let mut validators: Vec<ValidatorSpec> = spec
            .validators
            .iter()
            .flatten()
            .chain(
                axes.clone()
                    .flat_map(|axis| axis.validators.iter().flatten()),
            )
            .cloned()
            .collect();
        fail_unjudged(&mut validators);
        let mut aggregations: Vec<AggregationSpec> = spec
            .aggregations
            .iter()
            .flatten()
            .chain(axes.flat_map(|axis| axis.aggregations.iter().flatten()))
            .cloned()
            .collect();
        // No value: nothing is judged, so nothing to report.
        evaluate_aggregations(&mut aggregations, &spec.key, None, &mut Vec::new());
        let mut empty = Measurement {
            name: spec.key.clone(),
            value: MeasurementValue::Null,
            unit: spec.unit.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            validators: (!validators.is_empty()).then_some(validators),
            aggregations: (!aggregations.is_empty()).then_some(aggregations),
            description: spec.description.clone(),
            outcome: ValidatorOutcome::Unset,
        };
        empty.outcome = compute_measurement_outcome(&empty);
        measurements.push(empty);
        issues.push(MatchIssue::NotRecorded {
            measurement: spec.key.clone(),
            part: DeclaredPart::Measurement,
        });
    }
    if !issues.is_empty() {
        let undeclared: Vec<String> = measurements
            .iter()
            .filter(|m| !declared.iter().any(|d| d.key == m.name))
            .map(|m| m.name.clone())
            .collect();
        if !undeclared.is_empty() {
            issues.push(MatchIssue::Undeclared {
                measurements: undeclared,
            });
        }
    }
    issues
}

/// The verdict rule, shared by measurements, axes and aggregations: a
/// verdict comes from the checks (validators) wherever they are attached.
/// Any FAIL, or a check with no value to judge ⇒ FAIL. Otherwise no value
/// recorded, or a check that couldn't run ⇒ UNSET. Otherwise PASS — a
/// recorded value with no checks included, as in OpenHTF (`all([])`) and
/// TestStand's LOG comparison.
#[derive(Default)]
struct Verdict {
    checked: bool,
    failed: bool,
    incomplete: bool,
    /// An axis or aggregation with no value and no checks: UNSET itself,
    /// but with nothing to judge it leaves the measurement's verdict alone.
    empty: bool,
}

impl Verdict {
    fn validators(&mut self, validators: Option<&Vec<ValidatorSpec>>) {
        for v in validators.into_iter().flatten() {
            self.checked = true;
            match v.outcome {
                Some(ValidatorOutcome::Fail) => self.failed = true,
                Some(ValidatorOutcome::Pass) => {}
                Some(ValidatorOutcome::Unset) | None => self.incomplete = true,
            }
        }
    }

    fn aggregations(&mut self, aggregations: Option<&Vec<AggregationSpec>>) {
        for aggregation in aggregations.into_iter().flatten() {
            self.merge(aggregation_verdict(aggregation));
        }
    }

    fn axis(&mut self, axis: &AxisSpec) {
        let mut verdict = Verdict::default();
        verdict.validators(axis.validators.as_ref());
        if verdict.checked && axis.data.is_none() {
            verdict.failed = true;
        }
        self.merge(verdict);
        self.aggregations(axis.aggregations.as_ref());
    }

    fn merge(&mut self, other: Verdict) {
        self.checked |= other.checked;
        self.failed |= other.failed;
        self.incomplete |= other.incomplete;
    }

    fn outcome(&self) -> ValidatorOutcome {
        if self.failed {
            ValidatorOutcome::Fail
        } else if self.incomplete || self.empty {
            ValidatorOutcome::Unset
        } else {
            ValidatorOutcome::Pass
        }
    }
}

fn aggregation_verdict(aggregation: &AggregationSpec) -> Verdict {
    let mut verdict = Verdict::default();
    verdict.validators(aggregation.validators.as_ref());
    // Limits with no value to meet fail.
    match (verdict.checked, aggregation.value.is_none()) {
        (true, true) => verdict.failed = true,
        (false, true) => verdict.empty = true,
        _ => {}
    }
    verdict
}

/// Compute the rolled-up outcome for a single measurement, from every
/// check on it, its aggregations and its axes. Public so the CLI can
/// compute it for live broadcasts and the worker can validate invariants
/// in tests.
pub fn compute_measurement_outcome(measurement: &Measurement) -> ValidatorOutcome {
    let mut verdict = Verdict::default();
    verdict.validators(measurement.validators.as_ref());
    match &measurement.value {
        MeasurementValue::MultiDimensional(multidim) => {
            verdict.axis(&multidim.x_axis);
            multidim.y_axis.iter().for_each(|y| verdict.axis(y));
            // No data on any axis and no limits: nothing was recorded.
            let no_data =
                multidim.x_axis.data.is_none() && multidim.y_axis.iter().all(|y| y.data.is_none());
            if no_data && !verdict.checked {
                verdict.incomplete = true;
            }
        }
        // Nothing was measured: its limits had no value to meet, and
        // without limits there is nothing to report yet.
        MeasurementValue::Null if verdict.checked => verdict.failed = true,
        MeasurementValue::Null => verdict.incomplete = true,
        _ => {}
    }
    verdict.aggregations(measurement.aggregations.as_ref());
    verdict.outcome()
}

/// The verdict a measurement has the moment it is recorded: its declared
/// limits judged against the value alone. What an operator screen shows
/// while the phase still runs; phase end judges again with everything
/// (aggregations, axes, values merged from the operator UI). A series or
/// an empty value stays UNSET until then.
pub fn judge_recorded(measurement: &mut Measurement, specs: &[MeasurementSpec]) {
    if matches!(
        measurement.value,
        MeasurementValue::MultiDimensional(_) | MeasurementValue::Null
    ) {
        return;
    }
    if let Some(spec) = specs.iter().find(|s| s.key == measurement.name) {
        if measurement.unit.is_none() {
            measurement.unit = spec.unit.clone();
        }
        // A limit that cannot judge the value reads FAIL here as at phase
        // end; the issue itself is reported then, once.
        evaluate_declared_validators(measurement, spec, &mut Vec::new());
    }
    measurement.outcome = compute_measurement_outcome(measurement);
}

/// The numeric range a measurement's limits draw: the tightest `>` / `>=`
/// as the low end, the tightest `<` / `<=` as the high end. For displays
/// (a gauge's band); the verdict stays the validators'.
pub fn numeric_limits(validators: &[ValidatorSpec]) -> (Option<f64>, Option<f64>) {
    let (mut low, mut high): (Option<f64>, Option<f64>) = (None, None);
    for v in validators {
        let (Some(operator), Some(expected)) = (v.operator, &v.expected_value) else {
            continue;
        };
        let Some(limit) =
            numeric_limit(&expected_value_to_json(expected)).filter(|l| l.is_finite())
        else {
            continue;
        };
        match operator {
            ValidatorOperator::Gt | ValidatorOperator::Ge => {
                low = Some(low.map_or(limit, |l| l.max(limit)))
            }
            ValidatorOperator::Lt | ValidatorOperator::Le => {
                high = Some(high.map_or(limit, |h| h.min(limit)))
            }
            _ => {}
        }
    }
    (low, high)
}

/// Whether the ends `numeric_limits` returns exclude their value (`>`,
/// `<`) rather than include it (`>=`, `<=`): `(low, high)`. On a tie the
/// strict one wins, as it is the tighter.
pub fn exclusive_limits(validators: &[ValidatorSpec]) -> (bool, bool) {
    let (low, high) = numeric_limits(validators);
    let strict_at = |want: ValidatorOperator, end: Option<f64>| {
        end.is_some_and(|end| {
            validators.iter().any(|v| {
                v.operator == Some(want)
                    && v.expected_value
                        .as_ref()
                        .and_then(|e| numeric_limit(&expected_value_to_json(e)))
                        == Some(end)
            })
        })
    };
    (
        strict_at(ValidatorOperator::Gt, low),
        strict_at(ValidatorOperator::Lt, high),
    )
}

/// Whether no measurement failed. An UNSET measurement doesn't fail its
/// phase. Reads the verdict already rolled up on each measurement rather
/// than recomputing it: a declared multi-dimensional measurement never
/// recorded carries its FAIL on `outcome` alone, since the limits that
/// failed sit on axes an empty measurement can't carry.
pub fn check_all_measurements_pass(measurements: &[Measurement]) -> bool {
    measurements
        .iter()
        .all(|m| m.outcome != ValidatorOutcome::Fail)
}

/// A validator left to the engine, with no value to judge, fails.
fn fail_unjudged(validators: &mut [ValidatorSpec]) {
    for validator in validators {
        if validator.outcome.is_none() || validator.outcome == Some(ValidatorOutcome::Unset) {
            validator.outcome = Some(ValidatorOutcome::Fail);
        }
    }
}

/// Where a validator sits, for the issue raised when it can't judge.
struct Site<'a> {
    measurement: &'a str,
    part: Option<String>,
}

/// Judge every validator not judged yet against `value`. One that can't
/// judge it reads FAIL and is reported on `issues`.
fn judge_validators(
    validators: &mut [ValidatorSpec],
    value: &Value,
    site: &Site,
    issues: &mut Vec<MatchIssue>,
) {
    for validator in validators {
        if validator.outcome.is_some() && validator.outcome != Some(ValidatorOutcome::Unset) {
            continue;
        }
        validator.outcome = Some(match judge(validator, value) {
            Ok(outcome) => outcome,
            Err(reason) => {
                issues.push(MatchIssue::UnjudgedLimit {
                    measurement: site.measurement.to_string(),
                    part: site.part.clone(),
                    operator: validator.operator.map(|o| o.as_str().to_string()),
                    reason,
                });
                ValidatorOutcome::Fail
            }
        });
    }
}

/// Evaluate the validators the YAML declares on a recorded measurement.
/// Recorded measurements never carry validators of their own (the worker
/// sends none), so the declared ones are the measurement's limits.
fn evaluate_declared_validators(
    measurement: &mut Measurement,
    yaml_config: &MeasurementSpec,
    issues: &mut Vec<MatchIssue>,
) {
    let mut validators = yaml_config.validators.clone().unwrap_or_default();
    match measurement.value {
        // A measurement recorded as `None` has no value to meet its
        // limits, so they fail.
        MeasurementValue::Null => fail_unjudged(&mut validators),
        MeasurementValue::MultiDimensional(_) => {
            unset_series_limits(&mut validators, &measurement.name, issues)
        }
        _ => {}
    }
    let site = Site {
        measurement: &measurement.name,
        part: None,
    };
    if !matches!(measurement.value, MeasurementValue::MultiDimensional(_)) {
        judge_validators(
            &mut validators,
            &measurement_value_to_json(&measurement.value),
            &site,
            issues,
        );
    }
    measurement.validators = (!validators.is_empty()).then_some(validators);
}

/// A multi-dimensional reading is a set of series, not a value a
/// measurement-level limit can judge: `judge` would see it as null and
/// fail it as "not a number". The loader refuses such a limit in the
/// YAML; one that gets here anyway is left UNSET and reported.
fn unset_series_limits(
    validators: &mut [ValidatorSpec],
    measurement: &str,
    issues: &mut Vec<MatchIssue>,
) {
    for validator in validators {
        if validator.outcome.is_some() && validator.outcome != Some(ValidatorOutcome::Unset) {
            continue;
        }
        validator.outcome = Some(ValidatorOutcome::Unset);
        issues.push(MatchIssue::LimitOnSeries {
            measurement: measurement.to_string(),
            operator: validator.operator.map(|o| o.as_str().to_string()),
        });
    }
}

/// Evaluate validators that don't have YAML config
fn evaluate_measurement_validators(measurement: &mut Measurement) -> Vec<MatchIssue> {
    let mut issues = Vec::new();
    let value = measurement_value_to_json(&measurement.value);
    let is_null = matches!(measurement.value, MeasurementValue::Null);
    let site = Site {
        measurement: &measurement.name,
        part: None,
    };
    if let Some(validators) = &mut measurement.validators {
        if is_null {
            fail_unjudged(validators);
        } else if matches!(measurement.value, MeasurementValue::MultiDimensional(_)) {
            unset_series_limits(validators, &measurement.name, &mut issues);
        } else {
            judge_validators(validators, &value, &site, &mut issues);
        }
    }
    issues
}

/// Why a validator can never judge a value, whatever gets recorded: no
/// operator or no limit, a numeric comparison against a limit that is not
/// a number (`expected_value: "3.3"`), `in` / `not in` without a list, a
/// `matches` pattern that is not a regular expression. The loader refuses
/// a procedure that declares one; at run time it reads FAIL.
pub fn limit_problem(validator: &ValidatorSpec) -> Option<String> {
    let Some(operator) = validator.operator else {
        return Some("missing `operator`".to_string());
    };
    let expected = match &validator.expected_value {
        None | Some(ValidatorExpectedValue::Null) => {
            return Some(format!("missing `expected_value` for `{operator}`"))
        }
        Some(expected) => expected,
    };
    let limit = expected_value_to_json(expected);
    match operator {
        ValidatorOperator::Eq | ValidatorOperator::Ne => None,
        ValidatorOperator::Gt
        | ValidatorOperator::Ge
        | ValidatorOperator::Lt
        | ValidatorOperator::Le => {
            if is_non_finite_limit(expected) || numeric_limit(&limit).is_some() {
                return None;
            }
            Some(match expected {
                ValidatorExpectedValue::String(s) if s.trim().parse::<f64>().is_ok() => format!(
                    "`{operator}` needs a number as `expected_value`, got text `{}`; remove the quotes",
                    show(&limit)
                ),
                _ => format!(
                    "`{operator}` needs a number as `expected_value`, got `{}`",
                    show(&limit)
                ),
            })
        }
        ValidatorOperator::In | ValidatorOperator::NotIn => (!limit.is_array()).then(|| {
            format!(
                "`{operator}` needs a list as `expected_value`, got `{}`",
                show(&limit)
            )
        }),
        ValidatorOperator::Matches => match regex_pattern(&limit) {
            None => Some(format!(
                "`matches` needs text as `expected_value`, got `{}`",
                show(&limit)
            )),
            Some(pattern) => regex::Regex::new(pattern).err().map(|e| {
                // The regex crate's message repeats the pattern over several
                // lines with a caret; its last line is the cause.
                let text = e.to_string();
                let cause = text.lines().last().unwrap_or(&text);
                let cause = cause.strip_prefix("error: ").unwrap_or(cause);
                format!("invalid regex `{}`: {cause}", show(&limit))
            }),
        },
    }
}

/// `limit_problem` for a validator on an axis. An operator other than
/// `==` / `!=` with a limit to compare is never judged on a series: it is
/// left UNSET at run time and `resolve_runtime_refs` warns about it
/// (`MatchIssue::UnsupportedAxisOperator`), so it is not a problem here.
/// Everything else reaches `judge` and reads FAIL when it can't judge.
pub fn axis_limit_problem(validator: &ValidatorSpec) -> Option<String> {
    let problem = limit_problem(validator)?;
    match validator.operator {
        Some(operator)
            if has_limit(validator)
                && !matches!(operator, ValidatorOperator::Eq | ValidatorOperator::Ne) =>
        {
            None
        }
        _ => Some(problem),
    }
}

/// An `expected_value` that is a value: `~` and `null` are none.
fn has_limit(validator: &ValidatorSpec) -> bool {
    !matches!(
        validator.expected_value,
        None | Some(ValidatorExpectedValue::Null)
    )
}

/// `.inf` / `.nan` as a numeric limit: deprecated with a warning at load
/// (see `procedure::yaml`), and judged as before during the deprecation,
/// which is UNSET.
fn is_non_finite_limit(expected: &ValidatorExpectedValue) -> bool {
    match expected {
        ValidatorExpectedValue::Number(n) => !n.is_finite(),
        ValidatorExpectedValue::String(s) => crate::procedure::yaml::is_any_non_finite(s.trim()),
        _ => false,
    }
}

/// A value as it reads in a message, cut short when long.
fn show(value: &Value) -> String {
    const MAX: usize = 80;
    let text = value.to_string();
    match text.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}...", &text[..cut]),
        None => text,
    }
}

/// The verdict of one validator on `actual`: its own outcome when already
/// set, otherwise the comparison's. `Err` says why the validator could
/// not judge the value.
fn judge(validator: &ValidatorSpec, actual: &Value) -> Result<ValidatorOutcome, String> {
    if let Some(outcome) = &validator.outcome {
        if *outcome != ValidatorOutcome::Unset {
            return Ok(outcome.clone());
        }
    }
    if let Some(problem) = limit_problem(validator) {
        return Err(problem);
    }
    let (Some(operator), Some(expected)) = (validator.operator, &validator.expected_value) else {
        return Err("it has no `operator` or no `expected_value`".to_string());
    };
    let is_numeric = matches!(
        operator,
        ValidatorOperator::Gt
            | ValidatorOperator::Ge
            | ValidatorOperator::Lt
            | ValidatorOperator::Le
    );
    if is_numeric && is_non_finite_limit(expected) {
        return Ok(ValidatorOutcome::Unset);
    }
    let expected = expected_value_to_json(expected);
    let not_a_number = || format!("recorded value `{}` not a number", show(actual));
    let pass = match operator {
        ValidatorOperator::Eq => compare_values(actual, &expected, json_eq),
        ValidatorOperator::Ne => compare_values(actual, &expected, |a, e| !json_eq(a, e)),
        ValidatorOperator::Gt => {
            compare_numeric(actual, &expected, |a, e| a > e).ok_or_else(not_a_number)?
        }
        ValidatorOperator::Ge => {
            compare_numeric(actual, &expected, |a, e| a >= e).ok_or_else(not_a_number)?
        }
        ValidatorOperator::Lt => {
            compare_numeric(actual, &expected, |a, e| a < e).ok_or_else(not_a_number)?
        }
        ValidatorOperator::Le => {
            compare_numeric(actual, &expected, |a, e| a <= e).ok_or_else(not_a_number)?
        }
        ValidatorOperator::In | ValidatorOperator::NotIn => {
            check_membership(actual, &expected, operator == ValidatorOperator::In)
                .ok_or_else(|| "`expected_value` not a list".to_string())?
        }
        ValidatorOperator::Matches => check_regex_match(actual, &expected)
            .ok_or_else(|| format!("recorded value `{}` not text", show(actual)))?,
    };
    Ok(if pass {
        ValidatorOutcome::Pass
    } else {
        ValidatorOutcome::Fail
    })
}

/// `judge`, with a validator that can't judge the value read as FAIL.
#[cfg(test)]
fn evaluate_single_validator(validator: &ValidatorSpec, actual_value: &Value) -> ValidatorOutcome {
    judge(validator, actual_value).unwrap_or(ValidatorOutcome::Fail)
}

/// Structural equality where numbers compare by value: Python records
/// `3` (an integer), the YAML limit parses as `3.0` (a float), and
/// `serde_json` would call them different. Arrays compare element by
/// element, objects key by key; everything else as `serde_json` does.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => x == y,
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| json_eq(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| json_eq(x, y)))
        }
        _ => a == b,
    }
}

/// Compare two JSON values for equality/inequality
fn compare_values<F>(actual: &Value, expected: &Value, op: F) -> bool
where
    F: Fn(&Value, &Value) -> bool,
{
    // A single-element limit is the scalar it holds (`== [3.0]` on a
    // scalar reading), unless the reading is itself an array: then both
    // sides are arrays and compare as such (`== [3]` on `[3]`).
    let expected_scalar = match expected.as_array() {
        Some(arr) if arr.len() == 1 && !actual.is_array() => &arr[0],
        _ => expected,
    };

    op(actual, expected_scalar)
}

/// A numeric limit: a number, or a list holding one.
fn numeric_limit(limit: &Value) -> Option<f64> {
    match limit.as_array() {
        Some(arr) if arr.len() == 1 => arr[0].as_f64(),
        _ => limit.as_f64(),
    }
}

/// A `matches` pattern: a text, or a list holding one.
fn regex_pattern(limit: &Value) -> Option<&str> {
    match limit.as_array() {
        Some(arr) if arr.len() == 1 => arr[0].as_str(),
        _ => limit.as_str(),
    }
}

/// Compare numeric values. Returns None on type mismatch.
fn compare_numeric<F>(actual: &Value, expected: &Value, op: F) -> Option<bool>
where
    F: Fn(f64, f64) -> bool,
{
    Some(op(actual.as_f64()?, numeric_limit(expected)?))
}

/// Check if value is in/not in array. Returns None if expected is not an array.
fn check_membership(actual: &Value, expected: &Value, should_be_in: bool) -> Option<bool> {
    let arr = expected.as_array()?;

    // Numbers compare by value (`json_eq`): a measurement is always a JSON
    // float, and a list mixing numbers and text keeps `2` as a JSON
    // integer, which `Value`'s own equality never matches with `2.0`. A
    // one-element recorded array is the scalar it holds, so `[2] in
    // [1, 2, 3]` reads as the author meant it.
    let needle = match actual.as_array() {
        Some(a) if a.len() == 1 && !arr.iter().any(Value::is_array) => &a[0],
        _ => actual,
    };
    let is_member = arr.iter().any(|item| json_eq(needle, item));
    if should_be_in {
        Some(is_member)
    } else {
        Some(!is_member)
    }
}

/// Check if string matches regex pattern. Returns None on type mismatch or invalid regex.
fn check_regex_match(actual: &Value, pattern: &Value) -> Option<bool> {
    let re = regex::Regex::new(regex_pattern(pattern)?).ok()?;
    Some(re.is_match(actual.as_str()?))
}

/// Merge YAML aggregations with Python aggregations and evaluate them.
/// Returns the declared aggregation types that were never supplied.
fn merge_and_evaluate_aggregations(
    measurement: &mut Measurement,
    yaml_config: &MeasurementSpec,
    issues: &mut Vec<MatchIssue>,
) -> Vec<String> {
    let (mut all_aggregations, missing) = merge_aggregations(
        yaml_config.aggregations.as_ref(),
        measurement.aggregations.as_ref(),
    );
    evaluate_aggregations(&mut all_aggregations, &measurement.name, None, issues);
    measurement.aggregations = if all_aggregations.is_empty() {
        None
    } else {
        Some(all_aggregations)
    };
    missing
}

/// Evaluate aggregations only (when no YAML config exists)
/// Values must be provided by the caller, never computed here.
fn evaluate_aggregations_only(measurement: &mut Measurement, issues: &mut Vec<MatchIssue>) {
    if let Some(aggregations) = &mut measurement.aggregations {
        evaluate_aggregations(aggregations, &measurement.name, None, issues);
    }
}

/// Merge recorded aggregations into the declared ones, matched by `type`.
/// A recorded aggregation brings its value; the limits are the declared
/// ones (the worker records an aggregation as a type and a value only).
/// A declared aggregation with no recorded counterpart and no value of
/// its own is kept empty, so the run shows which limits had no value to
/// meet; its type is returned so it can be reported.
fn merge_aggregations(
    declared: Option<&Vec<AggregationSpec>>,
    recorded: Option<&Vec<AggregationSpec>>,
) -> (Vec<AggregationSpec>, Vec<String>) {
    let mut all_aggregations = declared.cloned().unwrap_or_default();
    let mut supplied = vec![false; all_aggregations.len()];

    for py_agg in recorded.into_iter().flatten() {
        let Some(idx) = all_aggregations
            .iter()
            .position(|yaml_agg| yaml_agg.aggregation_type == py_agg.aggregation_type)
        else {
            // Python aggregation is additional
            all_aggregations.push(py_agg.clone());
            continue;
        };
        if let Some(flag) = supplied.get_mut(idx) {
            *flag = true;
        }

        let yaml_agg = &all_aggregations[idx];
        let mut merged_agg = py_agg.clone();
        if merged_agg.unit.is_none() {
            merged_agg.unit = yaml_agg.unit.clone();
        }
        if yaml_agg.validators.is_some() {
            merged_agg.validators = yaml_agg.validators.clone();
        }
        all_aggregations[idx] = merged_agg;
    }

    let missing = all_aggregations
        .iter()
        .zip(&supplied)
        .filter(|(agg, supplied)| !**supplied && agg.value.is_none())
        .map(|(agg, _)| agg.aggregation_type.clone())
        .collect();
    (all_aggregations, missing)
}

/// Evaluate aggregation validators against the supplied values and set
/// each aggregation's outcome. Values are never computed here: the limits
/// of an aggregation with no value fail. `axis` is the key of the axis
/// the aggregations belong to, for the issue of a limit that can't judge.
fn evaluate_aggregations(
    aggregations: &mut [AggregationSpec],
    measurement: &str,
    axis: Option<&str>,
    issues: &mut Vec<MatchIssue>,
) {
    for aggregation in aggregations {
        if let Some(validators) = &mut aggregation.validators {
            match &aggregation.value {
                None => fail_unjudged(validators),
                Some(value) => {
                    let part = match axis {
                        Some(axis) => {
                            format!("aggregation {} (axis {axis})", aggregation.aggregation_type)
                        }
                        None => format!("aggregation {}", aggregation.aggregation_type),
                    };
                    let site = Site {
                        measurement,
                        part: Some(part),
                    };
                    judge_validators(validators, &aggregation_value_to_json(value), &site, issues);
                }
            }
        }
        aggregation.outcome = Some(aggregation_verdict(aggregation).outcome());
    }
}

/// How the recorded y axes pair with the declared ones.
#[derive(Debug, PartialEq)]
enum AxisPairing {
    /// `(recorded index, declared index)` pairs, and the declared indices
    /// nothing was recorded for.
    Paired {
        pairs: Vec<(usize, usize)>,
        unrecorded: Vec<usize>,
    },
    /// Keyed and keyless axes are mixed: nothing is paired.
    Mixed,
}

/// Pair recorded y axes with declared ones. By `key` when every recorded
/// axis has one (the `measurements.<name>.y_axis.<key>` builder always
/// does). By position only when no recorded axis has a key: the
/// `MultiDim`/`Axis` API has no key to give, and its list order is the
/// author's. Anything in between is reported rather than guessed, since a
/// wrong guess checks one series against another's limits.
fn match_y_axes(python_y_axes: &[AxisSpec], yaml_y_axes: &[AxisSpec]) -> AxisPairing {
    let py_keys: Vec<Option<String>> = python_y_axes.iter().map(AxisSpec::get_key).collect();
    let yaml_keys: Vec<Option<String>> = yaml_y_axes.iter().map(AxisSpec::get_key).collect();

    let mut pairs = Vec::new();
    if py_keys.iter().all(Option::is_none) {
        pairs.extend((0..python_y_axes.len().min(yaml_y_axes.len())).map(|i| (i, i)));
    } else if py_keys.iter().all(Option::is_some) && yaml_keys.iter().all(Option::is_some) {
        for (py_idx, py_key) in py_keys.iter().enumerate() {
            let taken = |idx: usize| pairs.iter().any(|(_, y)| *y == idx);
            if let Some(yaml_idx) =
                (0..yaml_keys.len()).find(|&i| !taken(i) && yaml_keys[i] == *py_key)
            {
                pairs.push((py_idx, yaml_idx));
            }
        }
    } else {
        return AxisPairing::Mixed;
    }

    let unrecorded = (0..yaml_y_axes.len())
        .filter(|i| !pairs.iter().any(|(_, y)| y == i))
        .collect();
    AxisPairing::Paired { pairs, unrecorded }
}

/// Merge and evaluate validators and aggregations for MultiDimensional
/// measurement axes. Returns what was declared but never recorded.
fn merge_and_evaluate_multidim_axes(
    measurement: &mut Measurement,
    yaml_config: &MeasurementSpec,
) -> Vec<MatchIssue> {
    let mut issues = Vec::new();
    let name = measurement.name.clone();
    let MeasurementValue::MultiDimensional(ref mut multidim_spec) = measurement.value else {
        return issues;
    };
    if yaml_config.x_axis.is_none() && yaml_config.y_axis.is_none() {
        return issues;
    }

    // Merge title from YAML if Python didn't provide it
    if multidim_spec.title.is_none() && yaml_config.title.is_some() {
        multidim_spec.title = yaml_config.title.clone();
    }

    let report_axis = |issues: &mut Vec<MatchIssue>, axis: Option<String>, report: AxisReport| {
        for aggregation_type in report.missing_aggregations {
            issues.push(MatchIssue::NotRecorded {
                measurement: name.clone(),
                part: DeclaredPart::Aggregation {
                    axis: axis.clone(),
                    aggregation_type,
                },
            });
        }
        issues.extend(report.unjudged);
        for operator in report.unsupported_operators {
            issues.push(MatchIssue::UnsupportedAxisOperator {
                measurement: name.clone(),
                axis: axis.clone(),
                operator,
            });
        }
    };

    if let Some(yaml_x_axis) = &yaml_config.x_axis {
        let report = evaluate_axis(&name, &mut multidim_spec.x_axis, yaml_x_axis);
        let axis = multidim_spec.x_axis.get_key();
        report_axis(&mut issues, axis, report);
    }

    if let Some(yaml_y_axes) = &yaml_config.y_axis {
        let pairing = match_y_axes(&multidim_spec.y_axis, yaml_y_axes);
        // Recorded axes nothing was declared for are checked against
        // their own limits only.
        let undeclared = AxisSpec {
            data: None,
            unit: None,
            legend: None,
            key: None,
            aggregations: None,
            validators: None,
            description: None,
        };
        for (py_idx, axis) in multidim_spec.y_axis.iter_mut().enumerate() {
            let paired = match &pairing {
                AxisPairing::Paired { pairs, .. } => pairs.iter().any(|(p, _)| *p == py_idx),
                AxisPairing::Mixed => false,
            };
            if !paired {
                let report = evaluate_axis(&name, axis, &undeclared);
                report_axis(&mut issues, axis.get_key(), report);
            }
        }
        match pairing {
            AxisPairing::Mixed => issues.push(MatchIssue::MixedAxisKeys {
                measurement: name.clone(),
            }),
            AxisPairing::Paired { pairs, unrecorded } => {
                for &(py_idx, yaml_idx) in &pairs {
                    let axis = &mut multidim_spec.y_axis[py_idx];
                    let report = evaluate_axis(&name, axis, &yaml_y_axes[yaml_idx]);
                    let key = axis.get_key();
                    report_axis(&mut issues, key, report);
                }
                // Threads record series in whatever order they finish, so
                // restore the declared order: declared series at their YAML
                // position, undeclared ones after them in recorded order.
                let declared = yaml_y_axes.len();
                let rank = |py_idx: usize| {
                    pairs
                        .iter()
                        .find(|(p, _)| *p == py_idx)
                        .map_or(declared + py_idx, |(_, y)| *y)
                };
                let mut ranked: Vec<_> = std::mem::take(&mut multidim_spec.y_axis)
                    .into_iter()
                    .enumerate()
                    .map(|(py_idx, axis)| (rank(py_idx), axis))
                    .collect();
                // A declared series never recorded is kept as an empty axis
                // at its declared position, so the dashboard shows which
                // series and limits had no value to meet.
                for yaml_idx in unrecorded {
                    let declared_axis = &yaml_y_axes[yaml_idx];
                    let mut empty = AxisSpec {
                        key: declared_axis.get_key(),
                        ..undeclared.clone()
                    };
                    // Its aggregations are covered by the axis report below.
                    evaluate_axis(&name, &mut empty, declared_axis);
                    let axis = empty
                        .get_key()
                        .unwrap_or_else(|| format!("#{}", yaml_idx + 1));
                    issues.push(MatchIssue::NotRecorded {
                        measurement: name.clone(),
                        part: DeclaredPart::YAxis(axis),
                    });
                    ranked.push((yaml_idx, empty));
                }
                ranked.sort_by_key(|(rank, _)| *rank);
                multidim_spec.y_axis = ranked.into_iter().map(|(_, axis)| axis).collect();
            }
        }
    }
    issues
}

/// What `evaluate_axis` found that the caller reports.
#[derive(Default)]
struct AxisReport {
    /// Declared aggregation types that were never supplied.
    missing_aggregations: Vec<String>,
    /// YAML spelling of validator operators that couldn't judge the
    /// series and were left UNSET.
    unsupported_operators: Vec<String>,
    /// Limits that got the series (or an aggregation's value) and could
    /// not judge it.
    unjudged: Vec<MatchIssue>,
}

/// Merge every declared field of an axis the recording left unset, then
/// evaluate its validators and aggregations.
fn evaluate_axis(
    measurement: &str,
    python_axis: &mut AxisSpec,
    yaml_axis: &AxisSpec,
) -> AxisReport {
    let mut report = AxisReport::default();
    if python_axis.unit.is_none() && yaml_axis.unit.is_some() {
        python_axis.unit = yaml_axis.unit.clone();
    }

    // Merge legend from YAML (use get_legend to auto-generate from key if needed)
    if python_axis.legend.is_none() && yaml_axis.get_legend().is_some() {
        python_axis.legend = yaml_axis.get_legend();
    }

    // Merge key from YAML (use get_key to auto-generate from legend if needed)
    if python_axis.key.is_none() && yaml_axis.get_key().is_some() {
        python_axis.key = yaml_axis.get_key();
    }

    if python_axis.description.is_none() && yaml_axis.description.is_some() {
        python_axis.description = yaml_axis.description.clone();
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

    // Auto-evaluate axis validators with UNSET outcome. An axis with no
    // data has no value to meet its limits, so they fail.
    if python_axis.data.is_none() {
        fail_unjudged(&mut all_validators);
    }
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
            if validator.outcome.is_some() && validator.outcome != Some(ValidatorOutcome::Unset) {
                continue;
            }
            // The series is judged as one array, which only `==` and `!=`
            // compare. Any other operator with a limit to meet is left
            // UNSET without evaluating it, and that has to be said: `in` /
            // `not in` would otherwise test the whole array for membership
            // and always FAIL / PASS, and the old rollup read the others'
            // UNSET as PASS.
            match validator.operator {
                Some(operator)
                    if has_limit(validator)
                        && !matches!(operator, ValidatorOperator::Eq | ValidatorOperator::Ne) =>
                {
                    validator.outcome = Some(ValidatorOutcome::Unset);
                    report
                        .unsupported_operators
                        .push(operator.as_str().to_string());
                }
                _ => {
                    let site = Site {
                        measurement,
                        part: Some(match python_axis.get_key() {
                            Some(key) => format!("axis {key}"),
                            None => "axis".to_string(),
                        }),
                    };
                    judge_validators(
                        std::slice::from_mut(validator),
                        &axis_json_value,
                        &site,
                        &mut report.unjudged,
                    );
                }
            }
        }
    }

    python_axis.validators = if all_validators.is_empty() {
        None
    } else {
        Some(all_validators)
    };

    let (mut all_aggregations, missing) = merge_aggregations(
        yaml_axis.aggregations.as_ref(),
        python_axis.aggregations.as_ref(),
    );
    let axis_key = python_axis.get_key();
    evaluate_aggregations(
        &mut all_aggregations,
        measurement,
        Some(axis_key.as_deref().unwrap_or("?")),
        &mut report.unjudged,
    );
    python_axis.aggregations = if all_aggregations.is_empty() {
        None
    } else {
        Some(all_aggregations)
    };
    report.missing_aggregations = missing;
    report
}

#[cfg(test)]
mod tests {
    //! Tests for `compute_measurement_outcome` and the verdict rule
    //! (TP-951): a verdict comes from the checks. Any FAIL, or a check with
    //! no value to judge ⇒ FAIL; no value or a check that couldn't run ⇒
    //! UNSET; otherwise PASS. As in OpenHTF, where `all([])` is True, a
    //! measurement with a value but no validators passes.

    use super::*;
    use crate::measurements::types::{Measurement, MeasurementValue};
    use crate::procedure::schema::{
        ValidatorExpectedValue, ValidatorOperator, ValidatorOutcome, ValidatorSpec,
    };

    /// `expected_value: .inf` was the f64 infinity under serde_yaml and is
    /// the text ".inf" under serde-saphyr (TP-1269). Neither is a number
    /// JSON can carry, so the verdict must be the same for both.
    #[test]
    fn a_non_finite_limit_judges_the_same_as_number_or_text() {
        use crate::procedure::schema::{ValidatorExpectedValue, ValidatorOperator};
        let verdict = |operator, expected| {
            let validator = ValidatorSpec {
                outcome: None,
                operator: Some(operator),
                expected_value: Some(expected),
                expression: None,
            };
            evaluate_single_validator(&validator, &serde_json::json!(3.0))
        };
        for operator in [
            ValidatorOperator::Le,
            ValidatorOperator::Ge,
            ValidatorOperator::Lt,
            ValidatorOperator::Gt,
            ValidatorOperator::Eq,
            ValidatorOperator::Ne,
        ] {
            let before = verdict(operator, ValidatorExpectedValue::Number(f64::INFINITY));
            let now = verdict(operator, ValidatorExpectedValue::String(".inf".into()));
            assert_eq!(before, now, "{operator:?}");
        }
        assert_eq!(
            verdict(
                ValidatorOperator::Le,
                ValidatorExpectedValue::String(".inf".into())
            ),
            ValidatorOutcome::Unset
        );

        // In a list, `.inf` makes it a mixed list whose numbers stay JSON
        // integers: membership must still match 3 against the measured 3.0.
        for operator in [ValidatorOperator::In, ValidatorOperator::NotIn] {
            let before = verdict(
                operator,
                ValidatorExpectedValue::NumberArray(vec![1.0, 3.0, f64::INFINITY]),
            );
            let now = verdict(
                operator,
                ValidatorExpectedValue::MixedArray(vec![
                    serde_json::json!(1),
                    serde_json::json!(3),
                    serde_json::json!(".inf"),
                ]),
            );
            assert_eq!(before, now, "{operator:?}");
        }
        assert_eq!(
            verdict(
                ValidatorOperator::In,
                ValidatorExpectedValue::MixedArray(vec![
                    serde_json::json!(3),
                    serde_json::json!("a")
                ])
            ),
            ValidatorOutcome::Pass
        );
    }

    fn make(value: MeasurementValue, validators: Option<Vec<ValidatorSpec>>) -> Measurement {
        Measurement {
            name: "x".into(),
            value,
            unit: None,
            timestamp: "0".into(),
            validators,
            aggregations: None,
            description: None,
            outcome: ValidatorOutcome::Unset,
        }
    }

    fn validator(outcome: ValidatorOutcome) -> ValidatorSpec {
        ValidatorSpec {
            outcome: Some(outcome),
            operator: Some(ValidatorOperator::Eq),
            expected_value: None,
            expression: None,
        }
    }

    fn eq_validator(expected: ValidatorExpectedValue) -> ValidatorSpec {
        ValidatorSpec {
            outcome: None,
            operator: Some(ValidatorOperator::Eq),
            expected_value: Some(expected),
            expression: None,
        }
    }

    fn evaluated_outcome(value: MeasurementValue, validator: ValidatorSpec) -> ValidatorOutcome {
        let mut m = make(value, Some(vec![validator]));
        evaluate_measurement_validators(&mut m);
        m.validators.as_ref().unwrap()[0].outcome.clone().unwrap()
    }

    /// Python records `[3]` (ints); the YAML limit parses as `[3.0]`
    /// (floats). Same numbers, same verdict.
    #[test]
    fn array_equality_ignores_int_versus_float_spelling() {
        let ints = MeasurementValue::Array(vec![serde_json::json!(3), serde_json::json!(1)]);
        let outcome = evaluated_outcome(
            ints.clone(),
            eq_validator(ValidatorExpectedValue::NumberArray(vec![3.0, 1.0])),
        );
        assert_eq!(outcome, ValidatorOutcome::Pass);

        let outcome = evaluated_outcome(
            ints,
            eq_validator(ValidatorExpectedValue::NumberArray(vec![3.0, 2.0])),
        );
        assert_eq!(outcome, ValidatorOutcome::Fail);
    }

    /// `== [3]` against the recorded array `[3]`: the single-element
    /// limit stays an array when the value is one.
    #[test]
    fn single_element_array_limit_matches_a_single_element_array_value() {
        let outcome = evaluated_outcome(
            MeasurementValue::Array(vec![serde_json::json!(3)]),
            eq_validator(ValidatorExpectedValue::NumberArray(vec![3.0])),
        );
        assert_eq!(outcome, ValidatorOutcome::Pass);
        // The scalar convenience still holds: `== [3.0]` on a scalar 3.
        let outcome = evaluated_outcome(
            MeasurementValue::Numeric(3.0),
            eq_validator(ValidatorExpectedValue::NumberArray(vec![3.0])),
        );
        assert_eq!(outcome, ValidatorOutcome::Pass);
    }

    #[test]
    fn membership_ignores_int_versus_float_spelling() {
        let mut m = make(
            MeasurementValue::Array(vec![serde_json::json!(2)]),
            Some(vec![ValidatorSpec {
                outcome: None,
                operator: Some(ValidatorOperator::In),
                expected_value: Some(ValidatorExpectedValue::NumberArray(vec![1.0, 2.0, 3.0])),
                expression: None,
            }]),
        );
        // A one-element recorded array is the scalar it holds for `in`.
        evaluate_measurement_validators(&mut m);
        assert_eq!(
            m.validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Pass),
            "2 is in [1.0, 2.0, 3.0]"
        );
    }

    #[test]
    fn no_value_and_no_validators_is_unset() {
        let m = make(MeasurementValue::Null, None);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Unset);
    }

    #[test]
    fn no_value_with_validators_is_fail() {
        let m = make(
            MeasurementValue::Null,
            Some(vec![validator(ValidatorOutcome::Unset)]),
        );
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Fail);
    }

    #[test]
    fn value_with_no_validators_passes() {
        // A recorded value with no checks passes, as in OpenHTF and
        // TestStand's LOG comparison.
        let m = make(MeasurementValue::Numeric(3.3), None);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn value_with_empty_validator_list_passes() {
        // Same rule as above, but the field is `Some(vec![])` instead of
        // `None` — make sure both shapes resolve the same way.
        let m = make(MeasurementValue::Numeric(3.3), Some(vec![]));
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn string_value_with_no_validators_passes() {
        // String measurements recorded for traceability (a device family,
        // a hardware revision) have no limits and pass.
        let m = make(MeasurementValue::String("widget-x".into()), None);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn multidim_without_validators_passes_only_with_data() {
        let mut m = multidim("curve", vec![]);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
        if let MeasurementValue::MultiDimensional(md) = &mut m.value {
            md.x_axis.data = None;
        }
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Unset);
    }

    #[test]
    fn boolean_value_with_no_validators_passes() {
        let m = make(MeasurementValue::Boolean(true), None);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn all_validators_pass_is_pass() {
        let m = make(
            MeasurementValue::Numeric(3.3),
            Some(vec![
                validator(ValidatorOutcome::Pass),
                validator(ValidatorOutcome::Pass),
            ]),
        );
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn any_validator_fails_is_fail() {
        let m = make(
            MeasurementValue::Numeric(3.3),
            Some(vec![
                validator(ValidatorOutcome::Pass),
                validator(ValidatorOutcome::Fail),
            ]),
        );
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Fail);
    }

    #[test]
    fn unset_validator_does_not_force_fail() {
        // A validator that couldn't run leaves the measurement unjudged:
        // UNSET, never FAIL. Only a FAIL fails.
        let m = make(
            MeasurementValue::Numeric(3.3),
            Some(vec![validator(ValidatorOutcome::Unset)]),
        );
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Unset);
    }

    #[test]
    fn auto_evaluate_writes_outcome_back() {
        // Integration: `auto_evaluate_measurements` should populate
        // `Measurement.outcome` so the CLI doesn't have to recompute. The
        // helper starts at UNSET, so only a PASS proves the write-back.
        let mut measurement = make(MeasurementValue::Numeric(3.3), None);
        measurement.validators = Some(vec![lt(5.0)]);
        let phase_config = crate::procedure::schema::PhaseDefinition {
            key: "p".into(),
            sequence_path: Vec::new(),
            sequence_stages: Vec::new(),
            inputs: Default::default(),
            plug_aliases: Default::default(),
            source: None,
            written: None,
            name: "p".into(),
            scope: None,
            python: None,
            executable: None,
            description: None,
            measurements: vec![],
            ui: None,
            enabled: true,
            result: None,
            depends_on: vec![],
            timeout: None,
            retry: None,
            then: None,
            with: Default::default(),
            env: Default::default(),
            value_scope: None,
        };
        let (evaluated, _) = auto_evaluate_measurements(vec![measurement], &phase_config);
        assert_eq!(evaluated[0].outcome, ValidatorOutcome::Pass);
    }

    // Verifies the `Mode Check` JSON docs example: a JSON-object measurement
    // with an `==` object validator deep-equals its expected value and
    // resolves to PASS, independent of key insertion order.
    fn obj_value(pairs: &[(&str, &str)]) -> MeasurementValue {
        let mut map = serde_json::Map::new();
        for (k, v) in pairs {
            map.insert((*k).into(), Value::String((*v).into()));
        }
        MeasurementValue::Object(map)
    }

    fn eq_object_validator(pairs: &[(&str, &str)]) -> ValidatorSpec {
        let mut map = serde_json::Map::new();
        for (k, v) in pairs {
            map.insert((*k).into(), Value::String((*v).into()));
        }
        ValidatorSpec {
            outcome: None,
            operator: Some(ValidatorOperator::Eq),
            expected_value: Some(ValidatorExpectedValue::Object(map)),
            expression: None,
        }
    }

    #[test]
    fn json_object_eq_validator_matches_is_pass() {
        let m = make(
            obj_value(&[("mode", "active"), ("power", "high")]),
            Some(vec![eq_object_validator(&[
                ("mode", "active"),
                ("power", "high"),
            ])]),
        );
        let mut m = m;
        evaluate_measurement_validators(&mut m);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn json_object_eq_validator_key_order_independent() {
        // Expected value lists keys in the opposite order; deep equality
        // must still hold because serde_json::Map is sorted.
        let m = make(
            obj_value(&[("mode", "active"), ("power", "high")]),
            Some(vec![eq_object_validator(&[
                ("power", "high"),
                ("mode", "active"),
            ])]),
        );
        let mut m = m;
        evaluate_measurement_validators(&mut m);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Pass);
    }

    #[test]
    fn json_object_eq_validator_mismatch_is_fail() {
        let m = make(
            obj_value(&[("mode", "idle"), ("power", "high")]),
            Some(vec![eq_object_validator(&[
                ("mode", "active"),
                ("power", "high"),
            ])]),
        );
        let mut m = m;
        evaluate_measurement_validators(&mut m);
        assert_eq!(compute_measurement_outcome(&m), ValidatorOutcome::Fail);
    }
    // --- Matching contract (TP-951): declarations vs recorded data ---

    fn phase(yaml: &str) -> crate::procedure::schema::PhaseDefinition {
        crate::procedure::yaml::from_str(yaml).expect("phase yaml")
    }

    fn lt(limit: f64) -> ValidatorSpec {
        ValidatorSpec {
            outcome: None,
            operator: Some(ValidatorOperator::Lt),
            expected_value: Some(ValidatorExpectedValue::Number(limit)),
            expression: None,
        }
    }

    fn agg(kind: &str, value: f64) -> AggregationSpec {
        AggregationSpec {
            aggregation_type: kind.into(),
            outcome: None,
            value: Some(AggregationValue::Number(value)),
            unit: None,
            validators: None,
        }
    }

    fn axis(key: Option<&str>, max: f64) -> crate::procedure::schema::AxisSpec {
        crate::procedure::schema::AxisSpec {
            data: Some(crate::procedure::schema::AxisData::Numeric(vec![0.0, max])),
            unit: None,
            legend: None,
            key: key.map(String::from),
            aggregations: Some(vec![agg("max", max)]),
            validators: None,
            description: None,
        }
    }

    fn multidim(name: &str, y: Vec<crate::procedure::schema::AxisSpec>) -> Measurement {
        make_named(
            name,
            MeasurementValue::MultiDimensional(crate::procedure::schema::MultiDimensionalSpec {
                x_axis: crate::procedure::schema::AxisSpec {
                    data: Some(crate::procedure::schema::AxisData::Numeric(vec![0.0, 1.0])),
                    unit: None,
                    legend: None,
                    key: None,
                    aggregations: None,
                    validators: None,
                    description: None,
                },
                y_axis: y,
                title: None,
            }),
        )
    }

    fn make_named(name: &str, value: MeasurementValue) -> Measurement {
        let mut m = make(value, None);
        m.name = name.into();
        m
    }

    fn y_axes(m: &Measurement) -> &[crate::procedure::schema::AxisSpec] {
        match &m.value {
            MeasurementValue::MultiDimensional(md) => &md.y_axis,
            _ => panic!("not multidim"),
        }
    }

    fn max_outcome(axis: &crate::procedure::schema::AxisSpec) -> Option<ValidatorOutcome> {
        axis.aggregations.as_ref().unwrap()[0].outcome.clone()
    }

    fn keys(m: &Measurement) -> Vec<String> {
        y_axes(m).iter().map(|a| a.key.clone().unwrap()).collect()
    }

    const MOTORS: &str = r#"
key: arm_motion
name: Arm motion
measurements:
  - key: motor_current
    name: Motor current
    x_axis:
      key: time
      unit: s
      description: Time (s)
    y_axis:
      - key: top
        unit: A
        description: Current consumption of Top motor
        aggregations:
          - type: max
            validators: [{ operator: "<", expected_value: 1.5 }]
      - key: mid
        unit: A
        aggregations:
          - type: max
            validators: [{ operator: "<", expected_value: 1.5 }]
      - key: bot
        unit: A
        aggregations:
          - type: max
            validators: [{ operator: "<", expected_value: 1.5 }]
"#;

    #[test]
    fn declared_measurement_never_recorded_is_kept_empty_and_reported() {
        // Clothilde's mismatch: the YAML key is not the name the Python
        // writes. The declared one used to vanish from the run.
        let config = phase(
            r#"
key: arm_motion
name: Arm motion
measurements:
  - key: top_motor_current
    name: motor_current
    unit: A
    validators: [{ operator: "<", expected_value: 1.5 }]
"#,
        );
        let recorded = vec![make_named("motor_current", MeasurementValue::Numeric(4.3))];
        let (mut ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert!(
            issues.is_empty(),
            "a recorded, undeclared measurement is supported: {issues:?}"
        );

        let issues = complete_declared_measurements(&mut ms, &config.measurements);
        assert_eq!(
            issues,
            vec![
                MatchIssue::NotRecorded {
                    measurement: "top_motor_current".into(),
                    part: DeclaredPart::Measurement,
                },
                MatchIssue::Undeclared {
                    measurements: vec!["motor_current".into()],
                },
            ]
        );
        let empty = ms.iter().find(|m| m.name == "top_motor_current").unwrap();
        assert!(matches!(empty.value, MeasurementValue::Null));
        assert_eq!(empty.outcome, ValidatorOutcome::Fail);
        assert_eq!(empty.unit.as_deref(), Some("A"));
        // The limit is kept, failed: there was no value to meet it.
        assert_eq!(
            empty.validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Fail)
        );
        assert!(!check_all_measurements_pass(&ms));
        assert_eq!(
            issues[0].to_string(),
            "Measurement top_motor_current: not recorded"
        );
        assert_eq!(issues[0].to_log_entry().level, "WARNING");
    }

    #[test]
    fn declared_measurement_never_recorded_without_validators_is_unset() {
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: firmware_version
    name: Firmware version
"#,
        );
        let mut ms = vec![];
        complete_declared_measurements(&mut ms, &config.measurements);
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
        assert!(check_all_measurements_pass(&ms));
    }

    #[test]
    fn declared_measurement_never_recorded_fails_on_its_aggregation_limits() {
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: ripple
    name: Ripple
    aggregations:
      - type: max
        validators: [{ operator: "<", expected_value: 1.5 }]
      - type: std
"#,
        );
        let mut ms = vec![];
        complete_declared_measurements(&mut ms, &config.measurements);
        let aggs = ms[0].aggregations.as_ref().unwrap();
        assert_eq!(aggs[0].outcome, Some(ValidatorOutcome::Fail));
        assert_eq!(aggs[1].outcome, Some(ValidatorOutcome::Unset));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn declared_multidim_measurement_never_recorded_fails_on_its_axis_limits() {
        let config = phase(MOTORS);
        let mut ms = vec![];
        complete_declared_measurements(&mut ms, &config.measurements);
        assert!(matches!(ms[0].value, MeasurementValue::Null));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        // The empty measurement can't carry axes, so the axis aggregation
        // limits are folded onto it, failed: the dashboard shows what
        // had no value to meet.
        assert!(ms[0].validators.is_none());
        let aggs = ms[0].aggregations.as_ref().unwrap();
        assert_eq!(aggs.len(), 3);
        assert!(aggs.iter().all(|a| a.aggregation_type == "max"));
        assert!(aggs
            .iter()
            .all(|a| a.outcome == Some(ValidatorOutcome::Fail)));
        assert!(aggs
            .iter()
            .all(|a| a.validators.as_ref().unwrap()[0].outcome == Some(ValidatorOutcome::Fail)));
        assert!(!check_all_measurements_pass(&ms));
    }

    #[test]
    fn declared_multidim_measurement_never_recorded_keeps_its_axis_validators() {
        let config = phase(
            r#"
key: arm_motion
name: Arm motion
measurements:
  - key: motor_current
    name: Motor current
    x_axis:
      key: time
      unit: s
      validators: [{ operator: "==", expected_value: [0, 10] }]
    y_axis:
      - key: top
        unit: A
        validators: [{ operator: "!=", expected_value: [2, 2] }]
      - key: mid
        unit: A
"#,
        );
        let mut ms = vec![];
        complete_declared_measurements(&mut ms, &config.measurements);
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        assert!(ms[0].aggregations.is_none());
        let validators = ms[0].validators.as_ref().unwrap();
        // The x axis first, then each y. A limit on the measurement itself
        // is refused at load (`JudgeableMeasurement`).
        let limits: Vec<Vec<f64>> = validators
            .iter()
            .map(|v| match &v.expected_value {
                Some(ValidatorExpectedValue::Number(n)) => vec![*n],
                Some(ValidatorExpectedValue::NumberArray(a)) => a.clone(),
                other => panic!("unexpected limit {other:?}"),
            })
            .collect();
        assert_eq!(limits, vec![vec![0.0, 10.0], vec![2.0, 2.0]]);
        assert!(validators
            .iter()
            .all(|v| v.outcome == Some(ValidatorOutcome::Fail)));
    }

    /// A limit on a multi-dimensional measurement itself (refused at load)
    /// read the series as null and FAILed as "null is not a number". It is
    /// left UNSET and said so.
    #[test]
    fn a_limit_on_a_multi_dimensional_reading_is_unset_not_null() {
        let mut config = phase(
            "key: p\nname: P\nmeasurements:\n  - key: trace\n    name: Trace\n    x_axis:\n      key: t\n",
        );
        config.measurements[0].validators = Some(vec![lt(5.0)]);
        let (ms, issues) = auto_evaluate_measurements(
            vec![multidim("trace", vec![axis(Some("v"), 3.0)])],
            &config,
        );
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
        assert_eq!(
            ms[0].validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Unset)
        );
        assert_eq!(
            issues,
            vec![MatchIssue::LimitOnSeries {
                measurement: "trace".into(),
                operator: Some("<".into()),
            }]
        );
        assert!(!issues[0].to_string().contains("not a number"));

        // A Python-provided one, with no declaration, the same.
        let mut m = multidim("trace", vec![]);
        m.validators = Some(vec![lt(5.0)]);
        let (ms, issues) = auto_evaluate_measurements(vec![m], &phase("key: p\nname: P\n"));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
        assert!(
            matches!(issues[..], [MatchIssue::LimitOnSeries { .. }]),
            "{issues:?}"
        );
    }

    #[test]
    fn recorded_measurement_is_not_added_again() {
        let config = phase(MOTORS);
        let mut ms = vec![multidim("motor_current", vec![])];
        assert!(complete_declared_measurements(&mut ms, &config.measurements).is_empty());
        assert_eq!(ms.len(), 1);
    }

    #[test]
    fn keyed_y_axes_match_by_key_whatever_the_recorded_order() {
        // Threads finish in any order: `mid` landing first must not get
        // `top`'s limits.
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![
                axis(Some("mid"), 1.0),
                axis(Some("bot"), 4.296),
                axis(Some("top"), 1.2),
            ],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert!(issues.is_empty(), "{issues:?}");
        let y = y_axes(&ms[0]);
        assert_eq!(keys(&ms[0]), ["top", "mid", "bot"]);
        assert_eq!(max_outcome(&y[0]), Some(ValidatorOutcome::Pass));
        assert_eq!(max_outcome(&y[1]), Some(ValidatorOutcome::Pass));
        assert_eq!(max_outcome(&y[2]), Some(ValidatorOutcome::Fail));
        assert_eq!(
            y[0].description.as_deref(),
            Some("Current consumption of Top motor"),
            "every declared axis field is merged, description included"
        );
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn undeclared_y_axis_goes_after_the_declared_ones() {
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![
                axis(Some("temp"), 40.0),
                axis(Some("bot"), 4.296),
                axis(Some("top"), 1.2),
                axis(Some("mid"), 1.0),
            ],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(keys(&ms[0]), ["top", "mid", "bot", "temp"]);
        let y = y_axes(&ms[0]);
        assert_eq!(max_outcome(&y[2]), Some(ValidatorOutcome::Fail));
        assert_eq!(
            max_outcome(&y[3]),
            Some(ValidatorOutcome::Pass),
            "no declared limit follows `temp` into `bot`'s old slot"
        );
    }

    #[test]
    fn y_axes_keep_recorded_order_without_a_declaration() {
        let config = phase(
            r#"
key: arm_motion
name: Arm motion
measurements:
  - key: motor_current
    name: Motor current
"#,
        );
        let recorded = vec![multidim(
            "motor_current",
            vec![axis(Some("bot"), 1.0), axis(Some("top"), 1.0)],
        )];
        let (ms, _) = auto_evaluate_measurements(recorded, &config);
        assert_eq!(keys(&ms[0]), ["bot", "top"]);
    }

    #[test]
    fn x_axis_description_is_merged() {
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![
                axis(Some("top"), 1.0),
                axis(Some("mid"), 1.0),
                axis(Some("bot"), 1.0),
            ],
        )];
        let (ms, _) = auto_evaluate_measurements(recorded, &config);
        match &ms[0].value {
            MeasurementValue::MultiDimensional(md) => {
                assert_eq!(md.x_axis.description.as_deref(), Some("Time (s)"));
                assert_eq!(md.x_axis.unit.as_deref(), Some("s"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn keyless_y_axes_pair_by_position() {
        // The `MultiDim(Axis(...))` API has no key: its list order is the
        // author's, so position is the match.
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![axis(None, 1.0), axis(None, 1.0), axis(None, 2.0)],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert!(issues.is_empty(), "{issues:?}");
        let y = y_axes(&ms[0]);
        assert_eq!(y[2].key.as_deref(), Some("bot"));
        assert_eq!(max_outcome(&y[2]), Some(ValidatorOutcome::Fail));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn numeric_limit_on_an_axis_is_left_unset_and_reported() {
        // `<= 2` on a series: the old rollup read the unjudged validator
        // as PASS. It can't be judged, so the measurement is UNSET, and
        // the phase log says why. The aggregation beside it still passes.
        let config = phase(
            r#"
key: arm_motion
name: Arm motion
measurements:
  - key: motor_current
    name: Motor current
    y_axis:
      - key: top
        validators: [{ operator: "<=", expected_value: 2 }]
        aggregations: [{ type: max, validators: [{ operator: "<=", expected_value: 2 }] }]
"#,
        );
        let recorded = vec![multidim("motor_current", vec![axis(Some("top"), 1.4)])];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        let top = &y_axes(&ms[0])[0];
        assert_eq!(
            top.validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Unset)
        );
        assert_eq!(max_outcome(top), Some(ValidatorOutcome::Pass));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
        assert!(check_all_measurements_pass(&ms));
        assert_eq!(
            issues,
            vec![MatchIssue::UnsupportedAxisOperator {
                measurement: "motor_current".into(),
                axis: Some("top".into()),
                operator: "<=".into(),
            }]
        );
        assert_eq!(
            issues[0].to_string(),
            "Measurement motor_current, axis top: `<=` ignored on series (UNSET); use `==`, `!=` or an aggregation"
        );
    }

    #[test]
    fn membership_limit_on_an_axis_is_left_unset_and_reported() {
        // `in` / `not in` would test the whole series array for membership:
        // `in` always FAILed a good unit, `not in` always PASSed unchecked.
        for operator in ["in", "not in"] {
            let config = phase(&format!(
                r#"
key: arm_motion
name: Arm motion
measurements:
  - key: motor_current
    name: Motor current
    y_axis:
      - key: top
        validators: [{{ operator: "{operator}", expected_value: [0] }}]
"#
            ));
            let recorded = vec![multidim("motor_current", vec![axis(Some("top"), 1.4)])];
            let (ms, issues) = auto_evaluate_measurements(recorded, &config);
            let top = &y_axes(&ms[0])[0];
            assert_eq!(
                top.validators.as_ref().unwrap()[0].outcome,
                Some(ValidatorOutcome::Unset),
                "{operator}"
            );
            assert_eq!(ms[0].outcome, ValidatorOutcome::Unset, "{operator}");
            assert_eq!(
                issues,
                vec![MatchIssue::UnsupportedAxisOperator {
                    measurement: "motor_current".into(),
                    axis: Some("top".into()),
                    operator: operator.into(),
                }],
                "{operator}"
            );
        }
    }

    #[test]
    fn mixed_keyed_and_keyless_y_axes_are_reported_not_guessed() {
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![axis(Some("top"), 1.0), axis(None, 9.0), axis(None, 9.0)],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert_eq!(
            issues,
            vec![MatchIssue::MixedAxisKeys {
                measurement: "motor_current".into()
            }]
        );
        // No limit was applied to any series, so nothing could fail; the
        // reported issue keeps the measurement UNSET.
        assert!(y_axes(&ms[0])
            .iter()
            .all(|a| max_outcome(a) == Some(ValidatorOutcome::Pass)));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
    }

    #[test]
    fn declared_y_axis_never_recorded_is_reported_and_fails_the_measurement() {
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![axis(Some("top"), 1.0), axis(Some("mid"), 1.0)],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert_eq!(
            issues,
            vec![MatchIssue::NotRecorded {
                measurement: "motor_current".into(),
                part: DeclaredPart::YAxis("bot".into()),
            }]
        );
        assert_eq!(keys(&ms[0]), ["top", "mid", "bot"]);
        let bot = &y_axes(&ms[0])[2];
        assert!(bot.data.is_none(), "kept as an empty axis");
        assert_eq!(bot.unit.as_deref(), Some("A"));
        let max = &bot.aggregations.as_ref().unwrap()[0];
        assert!(max.value.is_none());
        assert_eq!(max.outcome, Some(ValidatorOutcome::Fail));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn declared_y_axis_never_recorded_without_limits_unsets_the_measurement() {
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: motor_current
    name: Motor current
    y_axis:
      - key: top
        aggregations:
          - type: max
            validators: [{ operator: "<", expected_value: 1.5 }]
      - key: temp
"#,
        );
        let recorded = vec![multidim("motor_current", vec![axis(Some("top"), 1.0)])];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
    }

    #[test]
    fn empty_declared_y_axis_takes_its_declared_position() {
        let config = phase(MOTORS);
        let recorded = vec![multidim(
            "motor_current",
            vec![axis(Some("bot"), 4.296), axis(Some("top"), 1.0)],
        )];
        let (ms, issues) = auto_evaluate_measurements(recorded, &config);
        assert_eq!(
            issues.len(),
            1,
            "one report for the axis, none for its aggregation: {issues:?}"
        );
        assert_eq!(keys(&ms[0]), ["top", "mid", "bot"]);
        assert!(y_axes(&ms[0])[1].data.is_none());
        assert_eq!(
            ms[0].outcome,
            ValidatorOutcome::Fail,
            "mid's limits had no value to meet"
        );
    }

    #[test]
    fn declared_aggregation_never_supplied_is_kept_empty_and_failed() {
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: ripple
    name: Ripple
    aggregations:
      - type: max
        validators: [{ operator: "<", expected_value: 1.5 }]
      - type: avg
        validators: [{ operator: "<", expected_value: 1.0 }]
"#,
        );
        let mut m = make_named("ripple", MeasurementValue::Numeric(1.0));
        m.aggregations = Some(vec![agg("max", 1.2)]);
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert_eq!(
            issues,
            vec![MatchIssue::NotRecorded {
                measurement: "ripple".into(),
                part: DeclaredPart::Aggregation {
                    axis: None,
                    aggregation_type: "avg".into()
                },
            }]
        );
        let aggs = ms[0].aggregations.as_ref().unwrap();
        assert_eq!(aggs.len(), 2);
        assert_eq!(aggs[0].outcome, Some(ValidatorOutcome::Pass));
        // Kept so the run shows which limit had no value to meet.
        assert_eq!(aggs[1].aggregation_type, "avg");
        assert!(aggs[1].value.is_none());
        assert_eq!(aggs[1].outcome, Some(ValidatorOutcome::Fail));
        assert_eq!(
            aggs[1].validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Fail)
        );
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn declared_aggregation_never_supplied_without_limits_unsets_the_measurement() {
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: ripple
    name: Ripple
    validators: [{ operator: "<", expected_value: 1.5 }]
    aggregations:
      - type: std
"#,
        );
        let m = make_named("ripple", MeasurementValue::Numeric(1.0));
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
    }

    #[test]
    fn undeclared_measurement_is_checked_against_its_own_limits_silently() {
        let config = phase("key: p\nname: P\n");
        let mut m = make_named("board_temp", MeasurementValue::Numeric(41.2));
        m.validators = Some(vec![lt(40.0)]);
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert!(issues.is_empty());
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        let mut ms = ms;
        assert!(complete_declared_measurements(&mut ms, &config.measurements).is_empty());
    }

    // --- Verdict rule (TP-951): UNSET means not checked ---

    fn scalar_with_aggs(aggs: Vec<AggregationSpec>) -> Measurement {
        let mut m = make_named("m", MeasurementValue::Numeric(1.0));
        m.aggregations = Some(aggs);
        m
    }

    fn checked_agg(kind: &str, value: Option<f64>, limit: f64) -> AggregationSpec {
        AggregationSpec {
            aggregation_type: kind.into(),
            outcome: None,
            value: value.map(AggregationValue::Number),
            unit: None,
            validators: Some(vec![lt(limit)]),
        }
    }

    fn evaluated(m: Measurement) -> Measurement {
        let (mut ms, _) = auto_evaluate_measurements(vec![m], &phase("key: p\nname: P\n"));
        ms.remove(0)
    }

    #[test]
    fn aggregations_without_validators_pass() {
        let m = evaluated(scalar_with_aggs(vec![agg("max", 1.2), agg("std", 0.3)]));
        let aggs = m.aggregations.as_ref().unwrap();
        assert!(aggs
            .iter()
            .all(|a| a.outcome == Some(ValidatorOutcome::Pass)));
        assert_eq!(m.outcome, ValidatorOutcome::Pass);
    }

    #[test]
    fn empty_aggregation_without_validators_is_unset_alone() {
        // No value and no limits: the aggregation reports UNSET, but with
        // nothing to judge it doesn't hold back a measurement that passed.
        let mut empty = agg("std", 0.0);
        empty.value = None;
        let m = evaluated(scalar_with_aggs(vec![
            checked_agg("max", Some(1.2), 1.5),
            empty,
        ]));
        let aggs = m.aggregations.as_ref().unwrap();
        assert_eq!(aggs[0].outcome, Some(ValidatorOutcome::Pass));
        assert_eq!(aggs[1].outcome, Some(ValidatorOutcome::Unset));
        assert_eq!(m.outcome, ValidatorOutcome::Pass);
    }

    #[test]
    fn aggregation_validators_decide_the_measurement() {
        let pass = evaluated(scalar_with_aggs(vec![
            checked_agg("max", Some(1.2), 1.5),
            checked_agg("min", Some(0.1), 1.5),
        ]));
        assert_eq!(pass.outcome, ValidatorOutcome::Pass);

        let fail = evaluated(scalar_with_aggs(vec![
            checked_agg("max", Some(1.2), 1.5),
            checked_agg("min", Some(2.0), 1.5),
        ]));
        assert_eq!(fail.outcome, ValidatorOutcome::Fail);
        assert!(check_all_measurements_pass(&[pass]));
        assert!(!check_all_measurements_pass(&[fail]));
    }

    #[test]
    fn aggregation_with_no_value_fails_its_limits() {
        let m = evaluated(scalar_with_aggs(vec![
            checked_agg("max", Some(1.2), 1.5),
            checked_agg("avg", None, 1.0),
            agg("std", 0.3),
        ]));
        let aggs = m.aggregations.as_ref().unwrap();
        assert_eq!(aggs[0].outcome, Some(ValidatorOutcome::Pass));
        assert_eq!(aggs[1].outcome, Some(ValidatorOutcome::Fail));
        assert_eq!(aggs[2].outcome, Some(ValidatorOutcome::Pass));
        assert_eq!(m.outcome, ValidatorOutcome::Fail);
    }

    #[test]
    fn measurement_recorded_as_none_fails_its_limits() {
        // No value can't meet `== true`, whether it was never recorded or
        // recorded as `None`.
        let config = phase(
            r#"
key: p
name: P
measurements:
  - key: self_test_ok
    name: Self test
    validators: [{ operator: "==", expected_value: true }]
"#,
        );
        let m = make_named("self_test_ok", MeasurementValue::Null);
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert!(issues.is_empty());
        assert_eq!(
            ms[0].validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Fail)
        );
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        assert!(!check_all_measurements_pass(&ms));

        // Without validators, there is nothing to fail.
        let m = make_named("operator_note", MeasurementValue::Null);
        let (ms, _) = auto_evaluate_measurements(vec![m], &config);
        assert_eq!(ms[0].outcome, ValidatorOutcome::Unset);
        assert!(check_all_measurements_pass(&ms));
    }

    const VOUT_LE: &str = r#"
key: p
name: P
measurements:
  - key: vout
    name: Vout
    validators: [{ operator: "<=", expected_value: 3.3 }]
    aggregations:
      - type: max
        validators: [{ operator: "<", expected_value: 5 }]
"#;

    fn unjudged(issues: &[MatchIssue]) -> Vec<String> {
        issues
            .iter()
            .filter(|i| matches!(i, MatchIssue::UnjudgedLimit { .. }))
            .map(|i| i.to_string())
            .collect()
    }

    /// A limit that got a value it can't compare used to read UNSET,
    /// which does not fail a phase: the measurement passed unchecked.
    #[test]
    fn a_value_a_limit_cannot_judge_reads_fail_with_the_reason() {
        let config = phase(VOUT_LE);
        let m = make_named("vout", MeasurementValue::String("3.0 V".into()));
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert_eq!(
            ms[0].validators.as_ref().unwrap()[0].outcome,
            Some(ValidatorOutcome::Fail)
        );
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        assert!(!check_all_measurements_pass(&ms));
        let messages = unjudged(&issues);
        assert_eq!(messages.len(), 1, "{issues:?}");
        assert!(
            messages[0].contains("Measurement vout: `<=` reads FAIL")
                && messages[0].contains("`\"3.0 V\"` not a number"),
            "{messages:?}"
        );
    }

    /// The loader refuses a quoted limit; one that reaches the evaluator
    /// anyway (built in code) still fails rather than passing.
    #[test]
    fn a_limit_that_can_never_judge_reads_fail() {
        let mut config = phase(VOUT_LE);
        config.measurements[0].validators.as_mut().unwrap()[0].expected_value =
            Some(ValidatorExpectedValue::String("3.3".into()));
        let m = make_named("vout", MeasurementValue::Numeric(3.0));
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        let messages = unjudged(&issues);
        assert!(
            messages.len() == 1 && messages[0].contains("remove the quotes"),
            "{issues:?}"
        );

        for (operator, expected) in [
            (ValidatorOperator::In, ValidatorExpectedValue::Number(3.0)),
            (
                ValidatorOperator::Matches,
                ValidatorExpectedValue::String("(".into()),
            ),
        ] {
            let validator = ValidatorSpec {
                outcome: None,
                operator: Some(operator),
                expected_value: Some(expected),
                expression: None,
            };
            assert_eq!(
                evaluate_single_validator(&validator, &serde_json::json!("3")),
                ValidatorOutcome::Fail,
                "{operator:?}"
            );
        }
        let no_limit = ValidatorSpec {
            outcome: None,
            operator: Some(ValidatorOperator::Eq),
            expected_value: None,
            expression: None,
        };
        assert_eq!(
            evaluate_single_validator(&no_limit, &serde_json::json!(3.0)),
            ValidatorOutcome::Fail
        );
    }

    #[test]
    fn an_aggregation_value_its_limit_cannot_judge_reads_fail() {
        let config = phase(VOUT_LE);
        let mut m = make_named("vout", MeasurementValue::Numeric(3.0));
        m.aggregations = Some(vec![AggregationSpec {
            aggregation_type: "max".into(),
            outcome: None,
            value: Some(AggregationValue::String("high".into())),
            unit: None,
            validators: None,
        }]);
        let (ms, issues) = auto_evaluate_measurements(vec![m], &config);
        let agg = &ms[0].aggregations.as_ref().unwrap()[0];
        assert_eq!(agg.outcome, Some(ValidatorOutcome::Fail));
        assert_eq!(ms[0].outcome, ValidatorOutcome::Fail);
        let messages = unjudged(&issues);
        assert!(
            messages.len() == 1 && messages[0].contains("Measurement vout, aggregation max: `<`"),
            "{issues:?}"
        );
    }

    /// The worker tags every value once (`{"Object": {...}}`), so a
    /// recorded one-key dict is the phase's data, never a type tag.
    #[test]
    fn a_one_key_dict_is_data_not_a_type_tag() {
        let mut map = serde_json::Map::new();
        map.insert("Numeric".into(), serde_json::json!(3.0));
        let value = MeasurementValue::Object(map);
        assert_eq!(value.to_raw_json(), serde_json::json!({"Numeric": 3.0}));
        assert_eq!(
            measurement_value_to_json(&value),
            serde_json::json!({"Numeric": 3.0})
        );
    }

    #[test]
    fn a_recorded_value_is_judged_against_its_declared_limits_at_once() {
        let specs: Vec<MeasurementSpec> = crate::procedure::yaml::from_str(
            r#"
- key: x
  name: X
  unit: V
  validators:
    - { operator: ">=", expected_value: 3 }
    - { operator: ">", expected_value: 2 }
    - { operator: "<=", expected_value: 5 }
"#,
        )
        .unwrap();
        let judged = |value| {
            let mut m = make(value, None);
            judge_recorded(&mut m, &specs);
            m
        };
        let pass = judged(MeasurementValue::Numeric(4.0));
        assert_eq!(pass.outcome, ValidatorOutcome::Pass);
        assert_eq!(pass.unit.as_deref(), Some("V"));
        assert_eq!(pass.validators.as_ref().map(Vec::len), Some(3));
        assert_eq!(
            judged(MeasurementValue::Numeric(6.0)).outcome,
            ValidatorOutcome::Fail
        );
        assert_eq!(
            judged(MeasurementValue::Null).outcome,
            ValidatorOutcome::Unset
        );
        assert_eq!(
            numeric_limits(specs[0].validators.as_deref().unwrap()),
            (Some(3.0), Some(5.0)),
            "the tightest bound on each side"
        );

        let mut undeclared = make(MeasurementValue::Numeric(1.0), None);
        undeclared.name = "y".into();
        judge_recorded(&mut undeclared, &specs);
        assert_eq!(
            undeclared.outcome,
            ValidatorOutcome::Pass,
            "no limits: passes, as at phase end"
        );
    }
}
