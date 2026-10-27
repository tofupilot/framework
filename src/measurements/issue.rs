//! What the matching of declarations against recorded data reports.
//!
//! A leaf module: the run-time evaluator builds these, and the procedure
//! lint builds the same ones from a static scan, so both sides share one
//! wording without the lint depending on the evaluator.

/// One problem found while matching what the YAML declares against what
/// the phase recorded. Every level (measurement, y axis, aggregation)
/// reports through this type, and the worker turns each into a log line
/// on the phase, so the run log, `phase_complete` and the uploaded run
/// all carry the same wording.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchIssue {
    /// Declared in the YAML, never recorded by the phase.
    NotRecorded {
        measurement: String,
        part: DeclaredPart,
    },
    /// Some y axes were recorded with a key and others without, or the
    /// recorded keys can't meet keyless declarations: pairing them would
    /// be a guess, so none are paired.
    MixedAxisKeys { measurement: String },
    /// Measurements recorded without a declaration. A supported use, so
    /// only listed next to a missing declared measurement, where it is
    /// usually the other half of a naming mismatch.
    Undeclared { measurements: Vec<String> },
    /// A validator on an axis whose operator can't judge a series: only
    /// `==` and `!=` compare the whole series against the expected data.
    /// The validator is left UNSET, so the measurement never PASSes on
    /// a limit that was never checked. `operator` is the YAML spelling.
    UnsupportedAxisOperator {
        measurement: String,
        axis: Option<String>,
        operator: String,
    },
    /// A validator that got a value but could not judge it: a quoted
    /// number under `<=`, a text reading under a numeric limit, a pattern
    /// that is not a regular expression. It reads FAIL, since a limit
    /// nobody checked must not let the measurement pass. `part` names
    /// where the validator sits inside the measurement (an aggregation, an
    /// axis), `None` on the measurement itself.
    UnjudgedLimit {
        measurement: String,
        part: Option<String>,
        operator: Option<String>,
        reason: String,
    },
    /// A validator on a multi-dimensional measurement itself, not on one
    /// of its axes or aggregations: the reading is a set of series, which
    /// a measurement-level limit can't judge. The loader refuses one
    /// declared in the YAML; this is the run-time guard, and the
    /// validator is left UNSET.
    LimitOnSeries {
        measurement: String,
        operator: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclaredPart {
    Measurement,
    YAxis(String),
    Aggregation {
        axis: Option<String>,
        aggregation_type: String,
    },
}

impl MatchIssue {
    pub fn to_log_entry(&self) -> crate::log::LogEntry {
        let level = match self {
            MatchIssue::Undeclared { .. } => "INFO",
            _ => "WARNING",
        };
        crate::log::LogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: level.to_string(),
            message: self.to_string(),
            file: None,
            line: None,
        }
    }
}

impl std::fmt::Display for MatchIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchIssue::NotRecorded {
                measurement,
                part: DeclaredPart::Measurement,
            } => write!(f, "Measurement {measurement}: not recorded"),
            MatchIssue::NotRecorded {
                measurement,
                part: DeclaredPart::YAxis(axis),
            } => write!(f, "Measurement {measurement}, y axis {axis}: not recorded"),
            MatchIssue::NotRecorded {
                measurement,
                part:
                    DeclaredPart::Aggregation {
                        axis: Some(axis),
                        aggregation_type,
                    },
            } => write!(
                f,
                "Measurement {measurement}, aggregation {aggregation_type} (axis {axis}): not recorded"
            ),
            MatchIssue::NotRecorded {
                measurement,
                part:
                    DeclaredPart::Aggregation {
                        axis: None,
                        aggregation_type,
                    },
            } => write!(
                f,
                "Measurement {measurement}, aggregation {aggregation_type}: not recorded"
            ),
            MatchIssue::MixedAxisKeys { measurement } => write!(
                f,
                "Measurement {measurement}: mixed keyed and keyless y axes; key every y axis as in procedure.yaml"
            ),
            MatchIssue::Undeclared { measurements } => write!(
                f,
                "Undeclared measurements recorded: {}",
                measurements.join(", ")
            ),
            MatchIssue::UnsupportedAxisOperator {
                measurement,
                axis,
                operator,
            } => {
                let at = match axis {
                    Some(axis) => format!("Measurement {measurement}, axis {axis}"),
                    None => format!("Measurement {measurement}, axis"),
                };
                write!(
                    f,
                    "{at}: `{operator}` ignored on series (UNSET); use `==`, `!=` or an aggregation"
                )
            }
            MatchIssue::UnjudgedLimit {
                measurement,
                part,
                operator,
                reason,
            } => {
                let at = match part {
                    Some(part) => format!("Measurement {measurement}, {part}"),
                    None => format!("Measurement {measurement}"),
                };
                match operator {
                    Some(operator) => write!(f, "{at}: `{operator}` reads FAIL: {reason}"),
                    None => write!(f, "{at}: validator reads FAIL: {reason}"),
                }
            }
            MatchIssue::LimitOnSeries {
                measurement,
                operator,
            } => {
                let validator = match operator {
                    Some(operator) => format!("`{operator}`"),
                    None => "validator".to_string(),
                };
                write!(
                    f,
                    "Measurement {measurement}: {validator} ignored on series (UNSET); move it to an axis or an aggregation"
                )
            }
        }
    }
}
