use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::job::JobResult;
use super::orchestrator::orchestrator::ExecutionStats;
use crate::execution::job::Outcome;
use crate::execution::log::LogEntry;
use crate::measurements::Measurement;
use crate::procedure::schema::ProcedureDefinition;

#[derive(Debug, Serialize, Deserialize, specta::Type)]
pub struct DashboardInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub run_id: String,
    pub execution_id: String,
    pub timestamp: i64,
    pub duration_ms: i64,
    pub procedure: ProcedureInfo,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<crate::execution::types::UnitInfo>,
    pub phases: Vec<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<DashboardInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcedureInfo {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub job_id: String,
    pub slot_id: String,
    pub outcome: Outcome,
    pub start_time: i64,
    pub duration_ms: i64,
    pub measurements: Vec<Measurement>,
    pub logs: Vec<LogEntry>,
    pub error: Option<String>,
    pub attachments: Vec<String>,
    pub repeat_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportSummary {
    pub run_id: String,
    pub execution_id: String,
    pub timestamp: i64,
    pub directory: String,
    pub outcome: String,
    pub duration_ms: i64,
    pub total_phases: usize,
    pub failed_phases: usize,
    pub unit: Option<crate::execution::types::UnitInfo>,
    pub has_attachments: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<DashboardInfo>,
}

#[derive(Debug, Clone)]
pub struct ReportManager {
    reports_dir: PathBuf,
    current_report_dir: Option<PathBuf>,
    current_run_id: Option<String>,
    current_execution_id: Option<String>, // Groups multiple slot runs
    start_time: Option<i64>,
    procedure_def: Option<ProcedureDefinition>,
    job_attachments: HashMap<uuid::Uuid, Vec<String>>, // Map job_id to list of attachment filenames
    initial_unit_info: Option<crate::execution::types::UnitInfo>,
}

impl ReportManager {
    pub fn new(procedure_path: &Path) -> Result<Self, String> {
        let project_dir = procedure_path
            .parent()
            .ok_or("Invalid procedure path")?
            .to_path_buf();

        let reports_dir = project_dir.join("reports");

        fs::create_dir_all(&reports_dir)
            .map_err(|e| format!("Failed to create reports directory: {}", e))?;

        Ok(Self {
            reports_dir,
            current_report_dir: None,
            current_run_id: None,
            current_execution_id: None,
            start_time: None,
            procedure_def: None,
            job_attachments: HashMap::new(),
            initial_unit_info: None,
        })
    }

    pub fn start_report(
        &mut self,
        run_id: &str,
        execution_id: &str,
        slot_id: Option<&str>,
        procedure_def: &ProcedureDefinition,
        initial_unit_info: Option<crate::execution::types::UnitInfo>,
    ) -> Result<(), String> {
        self.initial_unit_info = initial_unit_info;
        let timestamp = Local::now();
        let short_id = if run_id.len() >= 8 {
            &run_id[..8]
        } else {
            run_id
        };

        let dir_name = if let Some(slot) = slot_id {
            format!("{}_RUN_{}_{}", timestamp.format("%Y-%m-%d_%H%M%S"), short_id, slot)
        } else {
            format!("{}_RUN_{}", timestamp.format("%Y-%m-%d_%H%M%S"), short_id)
        };

        self.start_report_with_dir(run_id, execution_id, &dir_name, procedure_def)
    }

    fn start_report_with_dir(
        &mut self,
        run_id: &str,
        execution_id: &str,
        dir_name: &str,
        procedure_def: &ProcedureDefinition,
    ) -> Result<(), String> {
        let report_dir = self.reports_dir.join(dir_name);

        fs::create_dir_all(&report_dir)
            .map_err(|e| format!("Failed to create report directory: {}", e))?;

        self.current_report_dir = Some(report_dir);
        self.current_run_id = Some(run_id.to_string());
        self.current_execution_id = Some(execution_id.to_string());
        self.start_time = Some(Local::now().timestamp_millis());
        self.procedure_def = Some(procedure_def.clone());

        log::info!("Starting test report: {}", dir_name);

        Ok(())
    }

    pub fn attach_file(
        &mut self,
        job_id: &uuid::Uuid,
        source: &Path,
        name: &str,
    ) -> Result<(), String> {
        if let Some(report_dir) = &self.current_report_dir {
            // Generate unique attachment ID
            let attachment_id = uuid::Uuid::new_v4();

            // Store file with UUID prefix in filename
            let stored_name = format!("{}_{}", &attachment_id.to_string()[..8], name);
            let dest = report_dir.join(&stored_name);

            fs::copy(source, &dest).map_err(|e| format!("Failed to copy attachment: {}", e))?;

            // Track attachment for this job
            self.job_attachments
                .entry(*job_id)
                .or_default()
                .push(stored_name);

            log::debug!(
                "📎 Attached file: {} (id: {})",
                name,
                &attachment_id.to_string()[..8]
            );
        }
        Ok(())
    }

    pub fn attach_data(
        &mut self,
        job_id: &uuid::Uuid,
        data: &[u8],
        name: &str,
    ) -> Result<(), String> {
        if let Some(report_dir) = &self.current_report_dir {
            // Generate unique attachment ID
            let attachment_id = uuid::Uuid::new_v4();

            // Store file with UUID prefix in filename
            let stored_name = format!("{}_{}", &attachment_id.to_string()[..8], name);
            let dest = report_dir.join(&stored_name);

            fs::write(&dest, data).map_err(|e| format!("Failed to write attachment: {}", e))?;

            // Track attachment for this job
            self.job_attachments
                .entry(*job_id)
                .or_default()
                .push(stored_name);

            log::debug!(
                "📎 Created attachment: {} (id: {})",
                name,
                &attachment_id.to_string()[..8]
            );
        }
        Ok(())
    }

    pub fn finalize_report(
        &mut self,
        stats: &ExecutionStats,
        job_results: &HashMap<uuid::Uuid, JobResult>,
        job_info: &HashMap<uuid::Uuid, crate::execution::state::JobInfo>,
    ) -> Result<(), String> {
        if let (Some(report_dir), Some(run_id), Some(execution_id), Some(start_time)) = (
            &self.current_report_dir,
            &self.current_run_id,
            &self.current_execution_id,
            self.start_time,
        ) {
            let duration_ms = Local::now().timestamp_millis() - start_time;

            let collected_unit_info = self.initial_unit_info.clone().or_else(|| {
                job_results
                    .values()
                    .filter_map(|r| r.unit.as_ref())
                    .max_by_key(|_| chrono::Utc::now())
                    .cloned()
            });

            // Build phase reports from job results
            let mut phases = Vec::new();
            for (job_id, result) in job_results {
                let info = job_info.get(job_id);
                phases.push(self.create_phase_entry(job_id, result, info)?);
            }

            // Sort phases by start time
            phases.sort_by_key(|p| p.start_time);

            let procedure_def = self
                .procedure_def
                .as_ref()
                .ok_or("Procedure definition not found")?;

            let report = Report {
                run_id: run_id.clone(),
                execution_id: execution_id.clone(),
                timestamp: start_time,
                duration_ms,
                procedure: ProcedureInfo {
                    name: procedure_def.name.clone(),
                    version: procedure_def.version.clone(),
                    description: Some(procedure_def.description.clone()),
                },
                outcome: stats.run_outcome.unwrap_or(Outcome::Pass),
                unit: collected_unit_info,
                phases,
                dashboard: None,
            };

            let json = serde_json::to_string_pretty(&report)
                .map_err(|e| format!("Failed to serialize report: {}", e))?;

            let report_path = report_dir.join("report.json");
            fs::write(report_path, json).map_err(|e| format!("Failed to write report: {}", e))?;

            log::info!("Test report saved: {}", report_dir.display());
        }

        Ok(())
    }

    fn create_phase_entry(
        &self,
        job_id: &uuid::Uuid,
        result: &JobResult,
        job_info: Option<&crate::execution::state::JobInfo>,
    ) -> Result<Phase, String> {
        // Use the pre-computed outcome from JobResult
        // This is ALWAYS set by the completion handler and accounts for measurements, retries, and all edge cases
        // Map Outcome::Retry to Outcome::Fail for report storage (intermediate retry attempts should show as failed)
        let outcome = match result.phase_outcome {
            Outcome::Retry => Outcome::Fail,
            other => other,
        };

        let error = if let Some(ref e) = result.error {
            Some(e.clone())
        } else {
            result
                .timeout_secs
                .map(|secs| format!("Timeout after {} seconds", secs))
        };

        // Get attachments specifically for this job from our tracking map
        let attachments = self
            .job_attachments
            .get(job_id)
            .cloned()
            .unwrap_or_else(Vec::new);

        let (phase_name, slot_id) = job_info
            .map(|info| {
                (
                    info.phase_name.clone(),
                    info.slot_id.clone().unwrap_or_else(|| "<shared>".to_string()),
                )
            })
            .unwrap_or_else(|| ("Unknown".to_string(), "Unknown".to_string()));

        Ok(Phase {
            name: phase_name,
            job_id: job_id.to_string(),
            slot_id,
            outcome,
            start_time: result.started_at.timestamp_millis(),
            duration_ms: result.completed_at.timestamp_millis()
                - result.started_at.timestamp_millis(),
            measurements: result.measurements.clone(),
            logs: result.logs.clone(),
            error,
            attachments,
            repeat_count: result.retry_count as u32,
        })
    }

    pub fn get_reports_dir(&self) -> &Path {
        &self.reports_dir
    }

    pub fn get_job_attachments(&self, job_id: &uuid::Uuid) -> Option<Vec<String>> {
        self.job_attachments.get(job_id).cloned()
    }

    pub fn get_current_report_dir_name(&self) -> Option<String> {
        self.current_report_dir
            .as_ref()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .map(|s| s.to_string())
    }

    pub fn get_current_run_id(&self) -> Option<String> {
        self.current_run_id.clone()
    }
}
