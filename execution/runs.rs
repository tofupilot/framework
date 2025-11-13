use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use ts_rs::TS;

use super::job::{JobResult, PhaseResult};
use super::orchestrator::ExecutionStats;
use crate::execution::job::Outcome;
use crate::execution::LogEntry;
use crate::measurements::Measurement;
use crate::schema::procedure::ProcedureDefinition;

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DashboardInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub upload_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub execution_id: String,
    pub timestamp: u64,
    pub duration_ms: u64,
    pub procedure: ProcedureInfo,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<crate::UnitInfo>,
    pub phases: Vec<Phase>,
    pub stats: RunStats,
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
    pub unit_id: Option<String>,
    pub outcome: String,
    pub start_time: u64,
    pub duration_ms: u64,
    pub measurements: Vec<Measurement>,
    pub logs: Vec<LogEntry>,
    pub error: Option<String>,
    pub attachments: Vec<String>,
    pub repeat_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunStats {
    pub total_jobs: usize,
    pub completed_jobs: usize,
    pub failed_jobs: usize,
    pub phases_passed: usize,
    pub phases_failed: usize,
    pub phases_skipped: usize,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunSummary {
    pub run_id: String,
    pub execution_id: String,
    #[ts(type = "number")]
    pub timestamp: u64,
    pub directory: String,
    pub outcome: String,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub total_phases: usize,
    pub failed_phases: usize,
    pub unit: Option<crate::UnitInfo>,
    pub has_attachments: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<DashboardInfo>,
}

#[derive(Debug, Clone)]
pub struct RunManager {
    runs_dir: PathBuf,
    current_run_dir: Option<PathBuf>,
    current_run_id: Option<String>,
    current_execution_id: Option<String>, // Groups multiple slot runs
    start_time: Option<u64>,
    procedure_def: Option<ProcedureDefinition>,
    job_attachments: HashMap<uuid::Uuid, Vec<String>>, // Map job_id to list of attachment filenames
    initial_unit_info: Option<crate::UnitInfo>,
}

impl RunManager {
    pub fn new(procedure_path: &Path) -> Result<Self, String> {
        let project_dir = procedure_path
            .parent()
            .ok_or("Invalid procedure path")?
            .to_path_buf();

        let runs_dir = project_dir.join("reports");

        fs::create_dir_all(&runs_dir)
            .map_err(|e| format!("Failed to create reports directory: {}", e))?;

        Ok(Self {
            runs_dir,
            current_run_dir: None,
            current_run_id: None,
            current_execution_id: None,
            start_time: None,
            procedure_def: None,
            job_attachments: HashMap::new(),
            initial_unit_info: None,
        })
    }

    pub fn start_run(
        &mut self,
        run_id: &str,
        execution_id: &str,
        procedure_def: &ProcedureDefinition,
        initial_unit_info: Option<crate::UnitInfo>,
    ) -> Result<(), String> {
        self.initial_unit_info = initial_unit_info;
        let timestamp = Local::now();
        // Extract short UUID (first 8 chars) if this is a full UUID, otherwise use as-is
        let short_id = if run_id.len() >= 8 {
            &run_id[..8]
        } else {
            run_id
        };
        let dir_name = format!("{}_RUN_{}", timestamp.format("%Y-%m-%d_%H%M%S"), short_id);
        self.start_run_with_dir(run_id, execution_id, &dir_name, procedure_def)
    }

    pub fn start_run_with_slot(
        &mut self,
        run_id: &str,
        execution_id: &str,
        slot_id: &str,
        procedure_def: &ProcedureDefinition,
        initial_unit_info: Option<crate::UnitInfo>,
    ) -> Result<(), String> {
        self.initial_unit_info = initial_unit_info;
        let timestamp = Local::now();
        let short_id = if run_id.len() >= 8 {
            &run_id[..8]
        } else {
            run_id
        };
        let dir_name = format!(
            "{}_RUN_{}_{}",
            timestamp.format("%Y-%m-%d_%H%M%S"),
            short_id,
            slot_id
        );
        self.start_run_with_dir(run_id, execution_id, &dir_name, procedure_def)
    }

    fn start_run_with_dir(
        &mut self,
        run_id: &str,
        execution_id: &str,
        dir_name: &str,
        procedure_def: &ProcedureDefinition,
    ) -> Result<(), String> {
        let run_dir = self.runs_dir.join(dir_name);

        fs::create_dir_all(&run_dir)
            .map_err(|e| format!("Failed to create run directory: {}", e))?;

        self.current_run_dir = Some(run_dir);
        self.current_run_id = Some(run_id.to_string());
        self.current_execution_id = Some(execution_id.to_string());
        self.start_time = Some(Local::now().timestamp_millis() as u64);
        self.procedure_def = Some(procedure_def.clone());

        crate::execution::cli_output::print_section(
            crate::execution::cli_output::Section::Reports,
            format!("Starting test report: {}", dir_name),
        );

        Ok(())
    }

    pub fn attach_file(
        &mut self,
        job_id: &uuid::Uuid,
        source: &Path,
        name: &str,
    ) -> Result<(), String> {
        if let Some(run_dir) = &self.current_run_dir {
            // Generate unique attachment ID
            let attachment_id = uuid::Uuid::new_v4();

            // Store file with UUID prefix in filename
            let stored_name = format!("{}_{}", &attachment_id.to_string()[..8], name);
            let dest = run_dir.join(&stored_name);

            fs::copy(source, &dest).map_err(|e| format!("Failed to copy attachment: {}", e))?;

            // Track attachment for this job
            self.job_attachments
                .entry(*job_id)
                .or_default()
                .push(stored_name);

            crate::execution::cli_output::debug(format!(
                "📎 Attached file: {} (id: {})",
                name,
                &attachment_id.to_string()[..8]
            ));
        }
        Ok(())
    }

    pub fn attach_data(
        &mut self,
        job_id: &uuid::Uuid,
        data: &[u8],
        name: &str,
    ) -> Result<(), String> {
        if let Some(run_dir) = &self.current_run_dir {
            // Generate unique attachment ID
            let attachment_id = uuid::Uuid::new_v4();

            // Store file with UUID prefix in filename
            let stored_name = format!("{}_{}", &attachment_id.to_string()[..8], name);
            let dest = run_dir.join(&stored_name);

            fs::write(&dest, data).map_err(|e| format!("Failed to write attachment: {}", e))?;

            // Track attachment for this job
            self.job_attachments
                .entry(*job_id)
                .or_default()
                .push(stored_name);

            crate::execution::cli_output::debug(format!(
                "📎 Created attachment: {} (id: {})",
                name,
                &attachment_id.to_string()[..8]
            ));
        }
        Ok(())
    }

    pub fn finalize_run(
        &mut self,
        stats: &ExecutionStats,
        job_results: &HashMap<uuid::Uuid, JobResult>,
        job_info: &HashMap<uuid::Uuid, (String, String, Option<String>)>,
    ) -> Result<(), String> {
        if let (Some(run_dir), Some(run_id), Some(execution_id), Some(start_time)) = (
            &self.current_run_dir,
            &self.current_run_id,
            &self.current_execution_id,
            self.start_time,
        ) {
            let duration_ms = Local::now().timestamp_millis() as u64 - start_time;

            // Merge unit info: start with initial unit, let phases override
            let collected_unit_info = {
                let mut merged_unit = self.initial_unit_info.clone();

                // Collect all units from job results sorted by completion time
                let mut units_with_time: Vec<_> = job_results
                    .values()
                    .filter_map(|r| r.unit.as_ref().map(|u| (r.completed_at, u)))
                    .collect();
                units_with_time.sort_by_key(|(time, _)| *time);

                // Merge field by field, later phases override earlier ones
                for (_, unit) in units_with_time {
                    if let Some(ref mut merged) = merged_unit {
                        // Phase-provided values override initial values (field by field)
                        if unit.serial_number.is_some() {
                            merged.serial_number = unit.serial_number.clone();
                        }
                        if unit.batch_number.is_some() {
                            merged.batch_number = unit.batch_number.clone();
                        }
                        if unit.part_number.is_some() {
                            merged.part_number = unit.part_number.clone();
                        }
                        if unit.revision_number.is_some() {
                            merged.revision_number = unit.revision_number.clone();
                        }
                        if unit.sub_units.is_some() {
                            merged.sub_units = unit.sub_units.clone();
                        }
                    } else {
                        // No initial unit, use first phase-provided unit
                        merged_unit = Some(unit.clone());
                    }
                }

                merged_unit
            };

            // Build phase reports from job results
            let mut phases = Vec::new();
            for (job_id, result) in job_results {
                let info = job_info.get(job_id);
                phases.push(self.create_phase_report(job_id, result, info)?);
            }

            // Sort phases by start time
            phases.sort_by_key(|p| p.start_time);

            let report = Run {
                run_id: run_id.clone(),
                execution_id: execution_id.clone(),
                timestamp: start_time,
                duration_ms,
                procedure: ProcedureInfo {
                    name: self
                        .procedure_def
                        .as_ref()
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| "Unknown".to_string()),
                    version: self
                        .procedure_def
                        .as_ref()
                        .map(|d| d.version.clone())
                        .unwrap_or_else(|| "1.0.0".to_string()),
                    description: self.procedure_def.as_ref().map(|d| d.description.clone()),
                },
                outcome: stats.run_outcome.unwrap_or(Outcome::Pass),
                unit: collected_unit_info,
                phases: phases.clone(),
                stats: RunStats {
                    total_jobs: stats.total_jobs,
                    completed_jobs: stats.completed_jobs,
                    failed_jobs: stats.failed_jobs,
                    phases_passed: phases.iter().filter(|p| p.outcome == "pass").count(),
                    phases_failed: phases
                        .iter()
                        .filter(|p| p.outcome == "fail" || p.outcome == "error")
                        .count(),
                    phases_skipped: phases.iter().filter(|p| p.outcome == "skip").count(),
                },
                dashboard: None,
            };

            // Write minified JSON report
            let json = serde_json::to_string(&report)
                .map_err(|e| format!("Failed to serialize report: {}", e))?;

            let report_path = run_dir.join("report.json");
            fs::write(report_path, json).map_err(|e| format!("Failed to write report: {}", e))?;

            crate::execution::cli_output::print_section(
                crate::execution::cli_output::Section::Reports,
                format!("Test report saved: {}", run_dir.display()),
            );
        }

        Ok(())
    }

    fn create_phase_report(
        &self,
        job_id: &uuid::Uuid,
        result: &JobResult,
        job_info: Option<&(String, String, Option<String>)>,
    ) -> Result<Phase, String> {
        // Determine outcome based on execution context
        let outcome = if result.error.is_some() {
            "error"
        } else if result.timeout_secs.is_some() {
            "timeout"
        } else {
            match &result.phase_result {
                PhaseResult::Continue => "pass",
                PhaseResult::Skip => "skip",
                PhaseResult::Stop => "stop",
                PhaseResult::Fail => "fail",
                PhaseResult::Retry => "retry",
            }
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
            .map(|(_, name, slot)| {
                (
                    name.clone(),
                    slot.clone().unwrap_or_else(|| "<shared>".to_string()),
                )
            })
            .unwrap_or_else(|| ("Unknown".to_string(), "Unknown".to_string()));

        Ok(Phase {
            name: phase_name,
            job_id: job_id.to_string(),
            slot_id,
            unit_id: None,
            outcome: outcome.to_string(),
            start_time: result.started_at.timestamp_millis() as u64,
            duration_ms: (result.completed_at.timestamp_millis()
                - result.started_at.timestamp_millis()) as u64,
            measurements: result.measurements.clone(),
            logs: {
                let mut logs = result.logs.clone();
                logs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                logs
            },
            error,
            attachments,
            repeat_count: result.retry_count as u32,
        })
    }

    pub fn get_runs_dir(&self) -> &Path {
        &self.runs_dir
    }

    pub fn get_job_attachments(&self, job_id: &uuid::Uuid) -> Option<Vec<String>> {
        self.job_attachments.get(job_id).cloned()
    }

    pub fn get_current_run_dir_name(&self) -> Option<String> {
        self.current_run_dir
            .as_ref()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .map(|s| s.to_string())
    }

    pub fn get_current_run_id(&self) -> Option<String> {
        self.current_run_id.clone()
    }
}
