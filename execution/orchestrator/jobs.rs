//! Job creation and management

use std::collections::HashMap;
use uuid::Uuid;

use crate::execution::job::{Job, RuntimeType};
use crate::execution::state::OrchestratorState;
use crate::procedure::schema::{PhaseDefinition, StageScope};
use crate::features::operator_ui::UiConfig;
use crate::procedure::schema::ProcedureDefinition;
use crate::features::operator_ui::convert_ui_config;
/// Enqueue jobs by stage/scope in the correct order
pub(super) fn enqueue_jobs_by_stage_scope(
    state: &mut OrchestratorState,
    procedure: &ProcedureDefinition,
    all_jobs: &[Job],
    stage_scope: StageScope,
    shared_only: bool,
) {
    for (current_type, phase) in procedure.get_all_phases_with_stage_scope() {
        if current_type == stage_scope && !phase.should_skip() {
            for job in all_jobs {
                if job.phase_key == phase.key {
                    let matches_scope = if shared_only { job.is_shared() } else { true };
                    if matches_scope {
                        state.enqueue_job(job.clone());
                    }
                }
            }
        }
    }
}

/// Filter jobs by slot ID and stage/scope
pub(super) fn filter_jobs_by_slot_and_type(
    jobs: &[Job],
    slot_id: &str,
    stage_scope: StageScope,
) -> Vec<Job> {
    jobs.iter()
        .filter(|job| job.slot_id.as_deref() == Some(slot_id) && job.stage_scope == stage_scope)
        .cloned()
        .collect()
}

/// Helper to create a job with the appropriate runtime type
pub(super) fn create_job_for_phase(
    phase: &PhaseDefinition,
    slot_id: Option<String>,
    stage_scope: StageScope,
    dependencies: Vec<String>,
    job_map: &HashMap<String, Uuid>,
    procedure_dir: &std::path::Path,
    procedure: &ProcedureDefinition,
) -> Job {
    // Determine runtime type from phase configuration
    let runtime_type = if phase.python.is_some() {
        RuntimeType::Python
    } else if phase.executable.is_some() {
        RuntimeType::Shell
    } else {
        RuntimeType::Native // UI-only or empty phases
    };

    let phase_key = phase.key.clone();

    let ui_config = if let Some(ref ui) = phase.ui {
        convert_ui_config(ui)
    } else {
        UiConfig::default()
    };

    if !ui_config.components.is_empty() {
        log::debug!(
            "🎨 Creating job for phase '{}' with {} UI components",
            phase.name.clone(),
            ui_config.components.len()
        );
    }

    // Get timeout - None means no timeout limit
    let timeout_ms = phase.get_timeout_ms();

    // Extract retry config from phase definition
    let (retry_limit, retry_delay_ms) = if let Some(ref retry) = phase.retry {
        (Some(retry.limit), retry.delay)
    } else {
        (None, None)
    };

    // Collect all available plug keys from procedure definition
    let all_available_plugs: Vec<String> = procedure
        .get_all_plugs_with_scope()
        .iter()
        .map(|(_, plug)| plug.key.clone())
        .collect();

    log::debug!(
        "DEBUG: Creating job for phase '{}' with {} available plugs: {:?}",
        phase.name.clone(),
        all_available_plugs.len(),
        all_available_plugs
    );

    match runtime_type {
        RuntimeType::Native => Job::new_native(
            slot_id,
            phase_key,
            phase.name.clone(),
            stage_scope,
            dependencies,
            all_available_plugs.clone(),
            ui_config,
            timeout_ms,
            retry_limit,
            retry_delay_ms,
            job_map,
            phase.measurements.clone(),
        ),
        RuntimeType::Shell => {
            let exec_config = phase
                .executable
                .as_ref()
                .expect("Executable config must exist");
            Job::new_shell(
                slot_id,
                phase_key,
                phase.name.clone(),
                stage_scope,
                exec_config.command.clone(),
                dependencies,
                all_available_plugs.clone(),
                ui_config,
                timeout_ms,
                retry_limit,
                retry_delay_ms,
                job_map,
                exec_config.shell.clone(),
                exec_config.working_directory.clone(),
                Some(procedure_dir.to_string_lossy().to_string()),
            )
        }
        RuntimeType::Python => {
            let python_spec = phase.python.as_ref().expect("Python config must exist");
            Job::new(
                slot_id,
                phase_key,
                phase.name.clone(),
                stage_scope,
                python_spec.get_module(),
                python_spec.get_callable_name(),
                dependencies,
                all_available_plugs.clone(),
                ui_config,
                timeout_ms,
                retry_limit,
                retry_delay_ms,
                job_map,
                phase.measurements.clone(),
            )
        }
    }
}

