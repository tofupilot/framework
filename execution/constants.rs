/// Constants for the execution system
pub mod timeouts {
    /// Default timeout for UI responses in seconds
    pub const UI_RESPONSE_TIMEOUT_SECS: u64 = 300; // 5 minutes for operator responses

    /// Default timeout for worker shutdown in seconds
    pub const WORKER_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

    /// Default warning threshold (percentage of timeout)
    pub const TIMEOUT_WARNING_THRESHOLD: u64 = 75; // 75% of timeout
}

pub mod limits {
    /// Default limit for phase retries (additional attempts after initial execution)
    pub const DEFAULT_RETRY_LIMIT: usize = 3;

    /// Maximum concurrent job executions (prevents resource exhaustion)
    pub const MAX_CONCURRENT_JOBS: usize = 100;

    /// Maximum job queue size before rejecting new submissions
    pub const MAX_JOB_QUEUE_SIZE: usize = 10000;
}

pub mod scheduling {
    /// Delay between scheduling attempts when no work is available (milliseconds)
    pub const IDLE_POLL_DELAY_MS: u64 = 10;
}
