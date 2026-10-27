use crate::job::{Job, JobResult};
use crate::log::LogEntry;
use uuid::Uuid;

pub fn convert_error_to_result(error: String, job: &Job, job_id: Uuid) -> JobResult {
    log::debug!("Job {} failed: {}", job_id, error);

    let error_logs = vec![LogEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: "ERROR".to_string(),
        message: error.clone(),
        file: None,
        line: None,
    }];

    let lower = error.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        // The budget (ms) from the error text, else the job's. Unknown: no
        // duration rather than "after 0ms".
        let timeout_ms = error
            .split_whitespace()
            .filter_map(|s| s.parse::<u64>().ok())
            .next()
            .or(job.timeout_ms)
            .filter(|ms| *ms > 0);
        let mut result = JobResult::new_timeout_ms(timeout_ms.unwrap_or(0));
        if timeout_ms.is_none() {
            result.timeout_ms = None;
            result.timeout_secs = None;
        }
        result.logs = error_logs;
        result
    } else {
        let mut result = JobResult::new_error(error);
        result.logs = error_logs;
        result
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    /// A `timeout: 300ms` phase lasted 300 ms, not a whole second: its
    /// duration and its start are the limit's, exactly.
    #[test]
    fn a_sub_second_timeout_keeps_its_milliseconds() {
        let mut job = crate::test_support::job(crate::procedure::schema::StageScope::Main);
        job.timeout_ms = Some(300);
        let result = convert_error_to_result("Timed out after 300ms".into(), &job, job.id);
        assert_eq!(result.timeout_ms, Some(300));
        assert_eq!(
            (result.completed_at - result.started_at).num_milliseconds(),
            300
        );
    }
}
