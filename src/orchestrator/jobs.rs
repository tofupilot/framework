//! Job creation and management

use std::collections::HashMap;
use uuid::Uuid;

use crate::job::{Job, JobSpec, RuntimeType, ShellSpec};
use crate::procedure::schema::ProcedureDefinition;
use crate::procedure::schema::{PhaseDefinition, StageScope};
use crate::state::OrchestratorState;
use crate::ui::UiConfig;
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
///
/// `partial_required_plugs` is `Some` on a partial run: the introspected
/// union of plug keys over the whole partial phase set, assigned to every
/// job so the creation gates start exactly that subset. `None` (full run)
/// keeps today's behaviour: every declared plug.
pub(super) fn create_job_for_phase(
    phase: &PhaseDefinition,
    slot_id: Option<String>,
    stage_scope: StageScope,
    job_map: &HashMap<String, Uuid>,
    procedure_dir: &std::path::Path,
    procedure: &ProcedureDefinition,
    partial_required_plugs: Option<&[String]>,
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

    let mut ui_config = phase.ui.as_ref().map(UiConfig::from).unwrap_or_default();
    // The run was refused before this point if the panel does not load
    // (`resolve_runtime_refs`); a file gone since only loses the skin.
    if let Some(rel) = phase.ui.as_ref().and_then(|u| u.panel.as_deref()) {
        match crate::ui::panel::load(procedure_dir, rel) {
            Ok(panel) => {
                // Its own submit button: the screen waits for it, as for
                // the Continue of an input component.
                if crate::ui::panel::has_submit(&panel.html) {
                    ui_config.requires_input = Some(true);
                }
                ui_config.panel = Some(panel);
            }
            Err(e) => log::warn!(
                "Phase '{}': {e}; showing the components instead",
                phase.name
            ),
        }
    }

    if !ui_config.components.is_empty() {
        log::debug!(
            "Creating job for phase '{}' with {} UI components",
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

    // Collect the run's plug keys: the introspected union on a partial
    // run, every declared plug on a full run.
    let all_available_plugs: Vec<String> = match partial_required_plugs {
        Some(subset) => subset.to_vec(),
        None => procedure
            .get_all_plugs_with_scope()
            .iter()
            .map(|(_, plug)| plug.key.clone())
            .collect(),
    };

    log::debug!(
        "Creating job for phase '{}' with {} available plugs: {:?}",
        phase.name.clone(),
        all_available_plugs.len(),
        all_available_plugs
    );

    let spec = JobSpec {
        slot_id,
        phase_key,
        phase_name: phase.name.clone(),
        stage_scope,
        depends_on: phase.depends_on.clone(),
        required_plugs: all_available_plugs,
        ui_config,
        timeout_ms,
        retry_limit,
        retry_delay_ms,
        phase_measurements: phase.measurements.clone(),
    };
    let mut job = match runtime_type {
        RuntimeType::Native => Job::new_native(spec, job_map),
        RuntimeType::Shell => {
            let exec_config = phase
                .executable
                .as_ref()
                .expect("Executable config must exist");
            let shell = ShellSpec {
                command: exec_config.command.clone(),
                shell_type: exec_config.shell.clone(),
                working_directory: exec_config.working_directory.clone(),
                procedure_dir: Some(procedure_dir.to_string_lossy().to_string()),
            };
            Job::new_shell(spec, shell, job_map)
        }
        RuntimeType::Python => {
            let python_spec = phase.python.as_ref().expect("Python config must exist");
            Job::new(
                spec,
                python_spec.get_module(),
                python_spec.get_callable_name(),
                job_map,
            )
        }
    };
    // What a phase of a sequence carries beyond a phase of the procedure:
    // its scope for cancellations and settlement, and what its Python
    // receives. Empty for a phase written in the procedure.
    job.sequence_path = phase.sequence_path.clone();
    job.sequence_stages = phase.sequence_stages.clone();
    job.inputs = phase.inputs.clone();
    job.plug_aliases = phase.plug_aliases.clone();
    job
}
