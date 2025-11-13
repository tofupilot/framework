//! Orchestrator initialization and job graph creation
//!
//! This module handles:
//! - Orchestrator initialization
//! - Report manager setup
//! - Procedure submission and job graph creation
//! - Job dependency resolution

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::execution::cli_output;
use crate::execution::constants::limits;
use crate::execution::job::Job;
use crate::execution::runs::RunManager;
use crate::schema::procedure::ProcedureDefinition;
use crate::PlugScope;

use super::{job_helpers, ExecutionStrategy, Orchestrator};

impl Orchestrator {
    pub async fn initialize(&mut self) -> Result<(), String> {
        // Start all workers (app_handle is optional for CLI mode)
        let app_handle = self.app_handle.as_ref();

        let mut workers = self.workers.write().await;
        for worker in workers.iter_mut() {
            worker.start(app_handle).await?;
        }
        Ok(())
    }

    pub async fn initialize_report_managers(
        &mut self,
        procedure_path: &std::path::Path,
        execution_id: &str,
        procedure_def: &ProcedureDefinition,
        slots: &[String],
    ) -> Result<(), String> {
        // Store the execution ID
        self.execution_id = Some(execution_id.to_string());

        let mut report_managers = self.report_managers.write().await;
        report_managers.clear();

        // Create a separate report manager for each slot
        // Note: Shared phases will be included in each slot's report rather than having a separate SHARED report
        for slot_id in slots {
            // Generate unique run ID for this slot
            let slot_run_id = uuid::Uuid::new_v4().to_string();

            let mut report_manager = RunManager::new(procedure_path)?;

            // Start run with slot-specific directory - this will include both slot and shared phases
            report_manager.start_run_with_slot(
                &slot_run_id,
                execution_id,
                slot_id,
                procedure_def,
                self.initial_unit_info.clone(),
            )?;

            // Store the run_id if this is the first slot
            if self.run_id.is_none() {
                self.run_id = Some(slot_run_id.clone());
            }

            report_managers.insert(slot_id.clone(), report_manager);
        }

        Ok(())
    }

    pub async fn submit_procedure(
        &mut self,
        procedure: &ProcedureDefinition,
        slots: Vec<String>,
        execution_strategy: ExecutionStrategy,
        initial_unit_info: crate::UnitInfo,
    ) -> Result<(), String> {
        // Store procedure definition
        self.procedure_definition = Some(procedure.clone());

        // Store initial unit info FIRST before anything else uses it
        crate::execution::cli_output::verbose(format!(
            "📋 submit_procedure: initial_unit_info = serial:{:?}, part:{:?}",
            initial_unit_info.serial_number, initial_unit_info.part_number
        ));
        self.initial_unit_info = Some(initial_unit_info.clone());

        // Extract plug scopes and pass to ResourceManager
        {
            let mut scopes = HashMap::new();
            for plug_def in &procedure.plugs {
                let scope = if plug_def.scope == Some(crate::schema::procedure::Scope::All) {
                    PlugScope::All
                } else {
                    PlugScope::Each
                };
                scopes.insert(plug_def.get_key(), scope);
            }
            let resource_manager = self.resource_manager.write().await;
            resource_manager.set_plug_scopes(scopes).await;

            // NOTE: All-scope plugs will be created before first SetupAll phase runs
        }

        let mut state = self.state.write().await;

        // Set should_stop_on_first_failure flag from procedure configuration
        state.should_stop_on_first_failure = matches!(
            procedure.on_first_failure,
            crate::schema::procedure::FirstFailureAction::Stop
        );
        if state.should_stop_on_first_failure {
            cli_output::print_section(
                cli_output::Section::Config,
                "on_first_failure is set to STOP - test will stop on first phase failure",
            );
        }

        // Initialize display based on CLI mode preferences
        {
            let _total_phases = procedure
                .get_all_phases_with_stage_scope()
                .into_iter()
                .filter(|(_, phase)| !phase.should_skip())
                .count();
        }

        // Check queue size limit using total phase count
        let total_phases = procedure.total_phase_count();
        if state.job_queue.len() + (slots.len() * total_phases) > limits::MAX_JOB_QUEUE_SIZE {
            return Err(format!(
                "Job queue size limit exceeded ({})",
                limits::MAX_JOB_QUEUE_SIZE
            ));
        }

        // Create global job mapping for all slots/phases
        let mut global_job_map: HashMap<String, Uuid> = HashMap::new();
        let mut all_jobs = Vec::new();

        // Track setup_procedure job IDs for implicit dependencies
        let mut setup_procedure_job_ids: HashSet<Uuid> = HashSet::new();
        // Track setup_slot job IDs per slot for implicit dependencies
        let mut setup_slot_job_ids: HashMap<String, HashSet<Uuid>> = HashMap::new();
        // Track main phase job IDs per slot for implicit dependencies
        let mut main_phase_job_ids: HashMap<String, HashSet<Uuid>> = HashMap::new();
        // Track each-slot teardown job IDs per slot for implicit dependencies
        let mut teardown_slot_job_ids: HashMap<String, HashSet<Uuid>> = HashMap::new();
        // Track ALL each-slot teardown job IDs across all slots for all-slots teardown dependencies
        let mut all_teardown_slot_job_ids: HashSet<Uuid> = HashSet::new();

        // First pass: create all jobs for all stage/scope combinations and store their IDs for dependency resolution

        // Cache the phase list to avoid re-iteration
        let all_phases_with_stage = procedure.get_all_phases_with_stage_scope();

        // Create all-slots phases once (shared across all slots)
        for &(stage_scope, phase) in all_phases_with_stage.iter() {
            if phase.should_skip() {
                continue;
            }

            match stage_scope {
                StageScope::SetupAll | StageScope::TeardownAll => {
                    // Build dependencies including implicit ones
                    let dependencies = phase.depends_on.clone();

                    // All-slots teardown must wait for all each-slot teardown phases
                    // (will be updated in second pass after we create each-slot teardown jobs)

                    // Create all-slots phases with no slot (shared)
                    let job = job_helpers::create_job_for_phase(
                        phase,
                        None, // No slot = shared across all slots
                        stage_scope,
                        dependencies,
                        &global_job_map,
                        &self.procedure_dir,
                        procedure,
                    );

                    // Store mapping for dependency resolution (use key for matching)
                    let key = format!("SHARED:{}", phase.get_key());
                    global_job_map.insert(key, job.id);

                    // Track setup_procedure jobs
                    if matches!(stage_scope, StageScope::SetupAll) {
                        setup_procedure_job_ids.insert(job.id);
                    }

                    all_jobs.push(job);
                }
                _ => {
                    // Skip slot-level phases in this first loop - we'll handle them per-slot below
                }
            }
        }

        // Create slot-level phases for each slot
        for slot_id in &slots {
            for &(stage_scope, phase) in all_phases_with_stage.iter() {
                if phase.should_skip() {
                    continue;
                }

                match stage_scope {
                    StageScope::SetupEach | StageScope::Main | StageScope::TeardownEach => {
                        // Create slot-specific phases (implicit dependencies added later)
                        let mut job = job_helpers::create_job_for_phase(
                            phase,
                            Some(slot_id.clone()),
                            stage_scope,
                            phase.depends_on.clone(),
                            &global_job_map,
                            &self.procedure_dir,
                            procedure,
                        );

                        // Add implicit dependencies based on stage/scope
                        match stage_scope {
                            StageScope::SetupEach => {
                                // Each-slot setup phases must wait for ALL all-slots setup phases
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                            }
                            StageScope::Main => {
                                // Main phases must wait for:
                                // 1. ALL all-slots setup phases
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                                // 2. Their slot's each-slot setup phases (will be added after we create them)
                            }
                            StageScope::TeardownEach => {
                                // Each-slot teardown phases must wait for ALL all-slots setup phases
                                // (Main phase dependencies will ensure proper ordering)
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                            }
                            _ => {}
                        }

                        // Store mapping for dependency resolution (use key for matching)
                        let key = format!("{}:{}", slot_id, phase.get_key());
                        global_job_map.insert(key, job.id);

                        // Track jobs by type for dependency management
                        match stage_scope {
                            StageScope::SetupEach => {
                                setup_slot_job_ids
                                    .entry(slot_id.clone())
                                    .or_default()
                                    .insert(job.id);
                            }
                            StageScope::Main => {
                                main_phase_job_ids
                                    .entry(slot_id.clone())
                                    .or_default()
                                    .insert(job.id);
                            }
                            StageScope::TeardownEach => {
                                teardown_slot_job_ids
                                    .entry(slot_id.clone())
                                    .or_default()
                                    .insert(job.id);
                                all_teardown_slot_job_ids.insert(job.id);
                            }
                            _ => {}
                        }

                        all_jobs.push(job);
                    }
                    _ => {
                        // Skip all-slots phases - already created above
                    }
                }
            }
        }

        // Second pass: Update phase dependencies to include implicit cross-phase dependencies
        for job in &mut all_jobs {
            match job.stage_scope {
                StageScope::SetupEach => {
                    // Each-slot setup phases must wait for ALL all-slots setup phases to complete
                    job.depends_on
                        .extend(setup_procedure_job_ids.iter().copied());
                }
                StageScope::Main => {
                    // Main phases need their slot's each-slot setup phases as dependencies
                    if let Some(slot_id) = &job.slot_id {
                        if let Some(setup_jobs) = setup_slot_job_ids.get(slot_id) {
                            job.depends_on.extend(setup_jobs.iter().copied());
                        }
                    }
                }
                StageScope::TeardownEach => {
                    // Each-slot teardown phases need their slot's Main phases as dependencies
                    if let Some(slot_id) = &job.slot_id {
                        if let Some(main_jobs) = main_phase_job_ids.get(slot_id) {
                            job.depends_on.extend(main_jobs.iter().copied());
                        }
                    }
                }
                StageScope::TeardownAll => {
                    // All-slots teardown phases must wait for ALL each-slot teardown phases across ALL slots
                    job.depends_on
                        .extend(all_teardown_slot_job_ids.iter().copied());
                }
                _ => {}
            }
        }

        // Third pass: enqueue jobs in proper execution order based on stage/scope combinations
        use crate::schema::procedure::StageScope;

        match execution_strategy {
            ExecutionStrategy::SlotFirst => {
                // Slot-first: complete all phases for each slot before moving to next
                cli_output::print_section(
                    cli_output::Section::Config,
                    "Using SLOT-FIRST execution model",
                );

                // Setup procedure phases (run once for all slots)
                for job in &all_jobs {
                    if matches!(job.stage_scope, StageScope::SetupAll) && job.is_shared() {
                        state.enqueue_job(job.clone());
                    }
                }

                // Store slot jobs for deferred queueing
                let mut slot_jobs: Vec<(String, Vec<Job>)> = Vec::new();

                // Group jobs by slot
                for slot_id in &slots {
                    let mut current_slot_jobs = Vec::new();

                    // Collect all jobs for this slot in execution order
                    current_slot_jobs.extend(job_helpers::filter_jobs_by_slot_and_type(
                        &all_jobs,
                        slot_id,
                        StageScope::SetupEach,
                    ));
                    current_slot_jobs.extend(job_helpers::filter_jobs_by_slot_and_type(
                        &all_jobs,
                        slot_id,
                        StageScope::Main,
                    ));
                    current_slot_jobs.extend(job_helpers::filter_jobs_by_slot_and_type(
                        &all_jobs,
                        slot_id,
                        StageScope::TeardownEach,
                    ));

                    if !current_slot_jobs.is_empty() {
                        slot_jobs.push((slot_id.clone(), current_slot_jobs));
                    }
                }

                // Store slot jobs for deferred execution
                // Only the first slot's jobs are enqueued initially
                if let Some((first_slot_id, first_slot_jobs)) = slot_jobs.first() {
                    cli_output::verbose(format!("📦 Starting with slot: {}", first_slot_id));
                    for job in first_slot_jobs {
                        state.enqueue_job(job.clone());
                    }
                }

                // Store remaining slots for later
                if slot_jobs.len() > 1 {
                    state.pending_slot_jobs = slot_jobs.into_iter().skip(1).collect();
                    cli_output::print_section(
                        cli_output::Section::Config,
                        format!(
                            "{} slots queued for sequential processing",
                            state.pending_slot_jobs.len()
                        ),
                    );
                }

                // Teardown procedure phases will be enqueued after all slots complete
                let mut teardown_procedure_jobs = Vec::new();
                for job in &all_jobs {
                    if matches!(job.stage_scope, StageScope::TeardownAll) && job.is_shared() {
                        teardown_procedure_jobs.push(job.clone());
                    }
                }
                state.teardown_procedure_jobs = teardown_procedure_jobs;
            }
            ExecutionStrategy::PhaseFirst => {
                // Phase-first: run same phase across all slots before moving to next phase
                cli_output::print_section(
                    cli_output::Section::Config,
                    "Using PHASE-FIRST execution model (default)",
                );

                job_helpers::enqueue_jobs_by_stage_scope(
                    &mut state,
                    procedure,
                    &all_jobs,
                    StageScope::SetupAll,
                    true,
                );
                job_helpers::enqueue_jobs_by_stage_scope(
                    &mut state,
                    procedure,
                    &all_jobs,
                    StageScope::SetupEach,
                    false,
                );
                job_helpers::enqueue_jobs_by_stage_scope(
                    &mut state,
                    procedure,
                    &all_jobs,
                    StageScope::Main,
                    false,
                );
                job_helpers::enqueue_jobs_by_stage_scope(
                    &mut state,
                    procedure,
                    &all_jobs,
                    StageScope::TeardownEach,
                    false,
                );
                job_helpers::enqueue_jobs_by_stage_scope(
                    &mut state,
                    procedure,
                    &all_jobs,
                    StageScope::TeardownAll,
                    true,
                );
            }
        }

        // Add plug scope operations to total job count for progress tracking
        // Each plug has 2 scope operations: init + delete
        let (procedure_plugs, slot_plugs): (Vec<_>, Vec<_>) = procedure
            .plugs
            .iter()
            .partition(|p| p.scope == Some(crate::schema::procedure::Scope::All));

        let procedure_plug_count = procedure_plugs.len();
        let slot_plug_count = slot_plugs.len();
        let plug_scope_operations =
            (procedure_plug_count * 2) + (slot_plug_count * slots.len() * 2);
        state.total_jobs_submitted += plug_scope_operations;

        cli_output::print_section(
            cli_output::Section::Init,
            format!(
                "Submitted {} jobs to queue ({} plug scope operations)",
                state.job_queue.len(),
                plug_scope_operations
            ),
        );

        // Emit execution plan to frontend
        self.emit_execution_plan(procedure, &state, &slots).await;

        Ok(())
    }
}
