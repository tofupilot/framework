use crate::execution::ui_types::{PythonPhaseResult, UiConfig};
use crate::execution::LogEntry;
use crate::measurements::Measurement;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct JobCommand {
    pub job_id: String,
    pub slot_id: String,
    pub phase_name: String,
    pub module: String,
    pub function: String,
    pub plugs: HashMap<String, String>,
    pub ui_config: UiConfig,
    pub timeout_ms: Option<u64>,
    pub retry_count: usize,
    pub retry_limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkerResponse {
    Ready,
    JobComplete {
        result: JobResultData,
    },
    Error {
        message: String,
    },
    #[serde(rename = "attach_file")]
    AttachFile {
        job_id: String,
        source_path: String,
        attachment_name: String,
    },
    #[serde(rename = "attach_data")]
    AttachData {
        job_id: String,
        data: String, // Base64 encoded
        attachment_name: String,
    },
    UIUpdate {
        job_id: String,
        action: String,
        data: serde_json::Value,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JobResultData {
    pub success: bool,
    pub phase_result: Option<PythonPhaseResult>, // Raw return value from Python (action/intent)
    pub measurements: Vec<Measurement>,
    pub logs: Vec<LogEntry>,
    pub error: Option<String>,
    pub exit_code: Option<i32>, // Exit code from sys.exit() if called
    pub unit: Option<serde_json::Value>, // Unit information from Python
}
