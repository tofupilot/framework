/// Constants for the execution system
pub mod timeouts {
    /// Default timeout for worker shutdown in seconds
    pub const WORKER_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

    /// Default warning threshold (percentage of timeout)
    pub const TIMEOUT_WARNING_THRESHOLD: u64 = 75; // 75% of timeout

    /// Max silence between sending a job command and the worker's
    /// `JobAck`. The ack is the first thing tp_worker.py writes after
    /// parsing the command — before any user code — so on a healthy
    /// machine it lands in milliseconds. A miss means the interpreter
    /// is alive but not executing (typically endpoint-protection
    /// suspending the process), the exact shape that hangs a run
    /// forever with no diagnostic. Generous to absorb a loaded box.
    pub const JOB_ACK_TIMEOUT_SECS: u64 = 30;

    /// Max silence while the phase's module is importing (between
    /// `JobAck` and `ModuleLoaded`, reset by any streamed event). This
    /// is the one window the Python-side phase timeout cannot bound —
    /// it is enforced from the same interpreter that is busy importing.
    /// Sized for an EDR scanning a fresh venv (minutes), while still
    /// converting a blocked-forever import (device open at module
    /// scope, held interpreter) into an actionable error.
    pub const MODULE_IMPORT_TIMEOUT_SECS: u64 = 300;
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
