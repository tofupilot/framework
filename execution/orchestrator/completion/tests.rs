#[cfg(test)]
mod outcome_resolver_tests {
    use crate::execution::job::{Job, JobResult, JobStatus, Outcome, PhaseResult, RuntimeType};
    use crate::execution::orchestrator::completion::outcome_resolver::resolve_outcome;
    use crate::measurements::types::{Measurement, MeasurementValue};
    use crate::procedure::schema::{StageScope, ValidatorOutcome, ValidatorSpec};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn create_test_job(retry_count: usize, retry_limit: usize) -> Job {
        let id = Uuid::new_v4();
        Job {
            id,
            slot_id: None,
            phase_key: "test_phase".to_string(),
            phase_name: "Test Phase".to_string(),
            stage_scope: StageScope::Main,
            module: "phases.test".to_string(),
            function: "test".to_string(),
            depends_on: HashSet::new(),
            required_plugs: vec![],
            ui_config: Default::default(),
            status: JobStatus::Running,
            result: None,
            retry_count,
            retry_limit,
            retry_delay_ms: None,
            timeout_ms: None,
            runtime_type: RuntimeType::Python,
            command: None,
            shell_type: None,
            working_directory: None,
            procedure_dir: None,
            phase_measurements: vec![],
            initial_unit_info: None,
            phase_results: std::collections::HashMap::new(),
            dependency_id: id,
        }
    }

    fn create_measurement_with_validator(outcome: ValidatorOutcome) -> Measurement {
        Measurement {
            name: "voltage".to_string(),
            value: MeasurementValue::Numeric(3.3),
            unit: Some("V".to_string()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            description: None,
            validators: Some(vec![ValidatorSpec {
                outcome: Some(outcome),
                operator: Some("less_than".to_string()),
                expected_value: None,
                expression: None,
            }]),
            aggregations: None,
        }
    }

    fn create_basic_job_result(
        phase_result: PhaseResult,
        measurements: Vec<Measurement>,
    ) -> JobResult {
        let now = chrono::Utc::now();
        JobResult {
            phase_result,
            phase_outcome: Outcome::Pass,
            next_action: None,
            timeout_secs: None,
            error: None,
            exit_code: None,
            measurements,
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    #[test]
    fn test_happy_path_continue_pass() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Pass)],
        );

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Pass);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_timeout_overrides_user_intent() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.timeout_secs = Some(60);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Timeout);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_error_overrides_user_intent() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.error = Some("Division by zero".to_string());

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Error);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_shutdown_produces_stop_outcome() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Continue, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Stop);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_explicit_stop() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Stop, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Stop);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_explicit_skip() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Skip, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Skip);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_explicit_fail() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Fail, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_measurement_validation_failure() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_retry_within_limit() {
        let job = create_test_job(1, 3);
        let job_result = create_basic_job_result(PhaseResult::Retry, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Retry);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_retry_limit_exceeded() {
        let job = create_test_job(3, 3);
        let job_result = create_basic_job_result(PhaseResult::Retry, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
        assert!(is_retry_exceeded);
    }

    #[test]
    fn test_priority_error_over_timeout() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.timeout_secs = Some(60);
        job_result.error = Some("Some error".to_string());

        let (outcome, _) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Error);
    }

    #[test]
    fn test_priority_error_over_shutdown() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.error = Some("Some error".to_string());

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Error);
    }

    #[test]
    fn test_priority_shutdown_over_measurement() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Stop);
    }

    #[test]
    fn test_priority_measurement_over_phase_result() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, _) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
    }

    #[test]
    fn test_retry_at_exact_limit_boundary() {
        let job = create_test_job(2, 3);
        let job_result = create_basic_job_result(PhaseResult::Retry, vec![]);

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Retry);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_retry_overrides_measurement_failure_when_authorized() {
        let job = create_test_job(1, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Retry,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Retry);
        assert!(!is_retry_exceeded);
    }

    #[test]
    fn test_measurement_failure_applies_when_retry_not_authorized() {
        let job = create_test_job(3, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Retry,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, is_retry_exceeded) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
        assert!(is_retry_exceeded);
    }

    #[test]
    fn test_multiple_measurements_one_fails() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![
                create_measurement_with_validator(ValidatorOutcome::Pass),
                create_measurement_with_validator(ValidatorOutcome::Fail),
                create_measurement_with_validator(ValidatorOutcome::Pass),
            ],
        );

        let (outcome, _) = resolve_outcome(&job_result, &job, false);

        assert_eq!(outcome, Outcome::Fail);
    }

    #[test]
    fn test_complex_priority_error_timeout_shutdown() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.error = Some("Error".to_string());
        job_result.timeout_secs = Some(60);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Error);
    }

    #[test]
    fn test_priority_timeout_over_shutdown() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.timeout_secs = Some(60);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Timeout);
    }

    #[test]
    fn test_priority_shutdown_over_retry() {
        let job = create_test_job(1, 3);
        let job_result = create_basic_job_result(PhaseResult::Retry, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Stop);
    }

    #[test]
    fn test_priority_shutdown_over_skip() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Skip, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Stop);
    }

    #[test]
    fn test_priority_shutdown_over_explicit_fail() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Fail, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);

        assert_eq!(outcome, Outcome::Stop);
    }
}

#[cfg(test)]
mod next_action_tests {
    use crate::execution::job::{JobResult, Outcome, PhaseResult};
    use crate::execution::orchestrator::completion::next_action::determine_next_action;
    use crate::procedure::schema::{PhaseDefinition, PhaseNextAction, ThenConfig};

    fn create_basic_job_result(phase_result: PhaseResult) -> JobResult {
        let now = chrono::Utc::now();
        JobResult {
            phase_result,
            phase_outcome: Outcome::Pass,
            next_action: None,
            timeout_secs: None,
            error: None,
            exit_code: None,
            measurements: vec![],
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    fn create_phase_def(then_config: Option<ThenConfig>) -> PhaseDefinition {
        PhaseDefinition {
            key: "test_phase".to_string(),
            name: "Test Phase".to_string(),
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
            then: then_config,
        }
    }

    #[test]
    fn test_outcome_pass_continues() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Pass;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_outcome_skip_continues() {
        let job_result = create_basic_job_result(PhaseResult::Skip);
        let outcome = Outcome::Skip;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_outcome_retry_retries() {
        let job_result = create_basic_job_result(PhaseResult::Retry);
        let outcome = Outcome::Retry;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Retry);
    }

    #[test]
    fn test_outcome_error_stops() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Error;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_outcome_timeout_stops() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Timeout;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_outcome_stop_stops() {
        let job_result = create_basic_job_result(PhaseResult::Stop);
        let outcome = Outcome::Stop;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_fail_without_on_first_failure_continues() {
        let job_result = create_basic_job_result(PhaseResult::Fail);
        let outcome = Outcome::Fail;

        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_fail_with_on_first_failure_stops() {
        let job_result = create_basic_job_result(PhaseResult::Fail);
        let outcome = Outcome::Fail;

        let next_action = determine_next_action(&job_result, &outcome, None, true);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_then_pass_override() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Pass;
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: Some(PhaseNextAction::Stop),
            fail: None,
            error: None,
            timeout: None,
        }));

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_then_fail_override() {
        let job_result = create_basic_job_result(PhaseResult::Fail);
        let outcome = Outcome::Fail;
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: None,
            fail: Some(PhaseNextAction::Continue),
            error: None,
            timeout: None,
        }));

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), true);

        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_then_fail_continue_overrides_on_first_failure_stop() {
        let job_result = create_basic_job_result(PhaseResult::Fail);
        let outcome = Outcome::Fail;
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: None,
            fail: Some(PhaseNextAction::Continue),
            error: None,
            timeout: None,
        }));

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), true);

        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_timeout_always_stops_despite_then_config() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Timeout;
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: None,
            fail: None,
            error: None,
            timeout: Some(PhaseNextAction::Continue),
        }));

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_error_always_stops_despite_then_config() {
        let job_result = create_basic_job_result(PhaseResult::Continue);
        let outcome = Outcome::Error;
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: None,
            fail: None,
            error: Some(PhaseNextAction::Continue),
            timeout: None,
        }));

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), false);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_on_first_failure_respected_when_no_then_config() {
        let job_result = create_basic_job_result(PhaseResult::Fail);
        let outcome = Outcome::Fail;
        let phase_def = create_phase_def(None);

        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), true);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_phase_result_retry_when_limit_exceeded_fails() {
        let job_result = create_basic_job_result(PhaseResult::Retry);
        let outcome = Outcome::Fail;

        let next_action = determine_next_action(&job_result, &outcome, None, true);

        assert_eq!(next_action, PhaseNextAction::Stop);
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::execution::job::{Job, JobResult, JobStatus, Outcome, PhaseResult, RuntimeType};
    use crate::execution::orchestrator::completion::next_action::determine_next_action;
    use crate::execution::orchestrator::completion::outcome_resolver::resolve_outcome;
    use crate::measurements::types::{Measurement, MeasurementValue};
    use crate::procedure::schema::{PhaseDefinition, PhaseNextAction, StageScope, ThenConfig, ValidatorOutcome, ValidatorSpec};
    use std::collections::HashSet;
    use uuid::Uuid;

    fn create_test_job(retry_count: usize, retry_limit: usize) -> Job {
        let id = Uuid::new_v4();
        Job {
            id,
            slot_id: None,
            phase_key: "test_phase".to_string(),
            phase_name: "Test Phase".to_string(),
            stage_scope: StageScope::Main,
            module: "phases.test".to_string(),
            function: "test".to_string(),
            depends_on: HashSet::new(),
            required_plugs: vec![],
            ui_config: Default::default(),
            status: JobStatus::Running,
            result: None,
            retry_count,
            retry_limit,
            retry_delay_ms: None,
            timeout_ms: None,
            runtime_type: RuntimeType::Python,
            command: None,
            shell_type: None,
            working_directory: None,
            procedure_dir: None,
            phase_measurements: vec![],
            initial_unit_info: None,
            phase_results: std::collections::HashMap::new(),
            dependency_id: id,
        }
    }

    fn create_phase_def(then_config: Option<ThenConfig>) -> PhaseDefinition {
        PhaseDefinition {
            key: "test_phase".to_string(),
            name: "Test Phase".to_string(),
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
            then: then_config,
        }
    }

    fn create_basic_job_result(
        phase_result: PhaseResult,
        measurements: Vec<Measurement>,
    ) -> JobResult {
        let now = chrono::Utc::now();
        JobResult {
            phase_result,
            phase_outcome: Outcome::Pass,
            next_action: None,
            timeout_secs: None,
            error: None,
            exit_code: None,
            measurements,
            logs: vec![],
            started_at: now,
            completed_at: now,
            resource_metrics: None,
            unit: None,
            retry_count: 0,
        }
    }

    fn create_measurement_with_validator(outcome: ValidatorOutcome) -> Measurement {
        Measurement {
            name: "voltage".to_string(),
            value: MeasurementValue::Numeric(3.3),
            unit: Some("V".to_string()),
            timestamp: chrono::Utc::now().to_rfc3339(),
            description: None,
            validators: Some(vec![ValidatorSpec {
                outcome: Some(outcome),
                operator: Some("less_than".to_string()),
                expected_value: None,
                expression: None,
            }]),
            aggregations: None,
        }
    }

    #[test]
    fn test_example1_happy_path() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Pass)],
        );

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(outcome, Outcome::Pass);
        assert_eq!(next_action, PhaseNextAction::Continue);
    }

    #[test]
    fn test_example2_timeout_overrides_user() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.timeout_secs = Some(60);

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(outcome, Outcome::Timeout);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_retry_flow() {
        let job = create_test_job(1, 3);
        let job_result = create_basic_job_result(PhaseResult::Retry, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(outcome, Outcome::Retry);
        assert_eq!(next_action, PhaseNextAction::Retry);
    }

    #[test]
    fn test_measurement_failure_with_on_first_failure() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(
            PhaseResult::Continue,
            vec![create_measurement_with_validator(ValidatorOutcome::Fail)],
        );

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, true);

        assert_eq!(outcome, Outcome::Fail);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_explicit_fail_with_on_first_failure() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Fail, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, true);

        assert_eq!(outcome, Outcome::Fail);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_error_always_stops() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.error = Some("Exception occurred".to_string());

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(outcome, Outcome::Error);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_timeout_always_stops() {
        let job = create_test_job(0, 3);
        let mut job_result = create_basic_job_result(PhaseResult::Continue, vec![]);
        job_result.timeout_secs = Some(60);
        let phase_def = create_phase_def(Some(ThenConfig {
            pass: None,
            fail: None,
            error: None,
            timeout: Some(PhaseNextAction::Continue),
        }));

        let (outcome, _) = resolve_outcome(&job_result, &job, false);
        let next_action = determine_next_action(&job_result, &outcome, Some(&phase_def), false);

        assert_eq!(outcome, Outcome::Timeout);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }

    #[test]
    fn test_shutdown_during_execution() {
        let job = create_test_job(0, 3);
        let job_result = create_basic_job_result(PhaseResult::Continue, vec![]);

        let (outcome, _) = resolve_outcome(&job_result, &job, true);
        let next_action = determine_next_action(&job_result, &outcome, None, false);

        assert_eq!(outcome, Outcome::Stop);
        assert_eq!(next_action, PhaseNextAction::Stop);
    }
}
