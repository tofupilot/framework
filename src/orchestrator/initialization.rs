//! Orchestrator initialization and job graph creation
//!
//! This module handles:
//! - Orchestrator initialization
//! - Procedure submission and job graph creation
//! - Job dependency resolution

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::constants::limits;
use crate::events::PlugScope;
use crate::job::Job;

use super::jobs;
use super::{ExecutionStrategy, Orchestrator};

impl Orchestrator {
    pub async fn initialize(&mut self) -> Result<(), String> {
        // A host that resolved nothing runs with the declarations' defaults:
        // a required value then refuses the run here, before any process
        // starts. Before the workers start, which take the procedure's
        // `env:` expanded with these values.
        if self.run_values.is_none() {
            let values = crate::procedure::resolve_values(
                &self.procedure_definition,
                Default::default(),
                &Default::default(),
            )
            .map_err(|e| e.to_string())?;
            self.set_run_values(values);
        }

        // Use the orchestrator's pre-resolved Python path when available
        // (CLI runs always set it). Fall back to the engine's walk-up
        // resolver for legacy callers that don't pass one in.
        let python_cmd =
            crate::python::resolve_or_walk(&self.python_path, &self.procedure_dir).await?;

        // The interpreter is resolved here so a missing venv still fails
        // the run before it starts; the spawns themselves run in the
        // background and the first job on each worker waits for its
        // own (`Worker::await_started`). On a station this overlaps the
        // pool spawn (~150ms) with identify and plug startup instead of
        // serialising the three.
        let workers = self.workers.read().await;
        for worker in workers.iter() {
            worker.start_in_background(python_cmd.clone());
        }

        Ok(())
    }

    pub async fn submit_procedure(
        &mut self,
        slots: Vec<String>,
        execution_strategy: ExecutionStrategy,
        initial_unit_infos: std::collections::HashMap<String, crate::unit::UnitInfo>,
        only_phases: &[String],
    ) -> Result<(), String> {
        // Partial run: resolve the targets' transitive dependency closure
        // once, before any state is touched. Setup and teardown phases
        // always run; only the main stage is filtered by this set. No
        // target is a full run.
        let partial_main_set = (!only_phases.is_empty())
            .then(|| partial_main_phase_set_many(&self.procedure_definition, only_phases))
            .transpose()?;

        // Partial run: introspect phase signatures once (one extra Python
        // process start) and narrow the run's plug set to the union of
        // what the executing phases can touch. Full runs keep today's
        // start-everything behaviour — narrowing them would change
        // scheduling for every station in the field.
        let partial_required_plugs: Option<Vec<String>> = match &partial_main_set {
            Some(set) => Some(
                match crate::procedure::introspect_procedure(
                    &self.procedure_dir,
                    &self.python_path,
                    &self.procedure_definition,
                )
                .await
                {
                    Ok(introspection) => {
                        union_required_plugs(&self.procedure_definition, set, &introspection)
                    }
                    Err(e) => {
                        log::warn!("Phase introspection failed: {e}; starting all plugs");
                        self.procedure_definition
                            .plugs
                            .iter()
                            .map(|p| p.key.clone())
                            .collect()
                    }
                },
            ),
            None => None,
        };

        // Store procedure definition

        // Store initial unit infos FIRST before anything else uses it
        self.initial_unit_infos = initial_unit_infos.clone();

        // Extract plug scopes and pass to ResourceManager
        {
            let mut scopes = HashMap::new();
            for plug_def in &self.procedure_definition.plugs {
                let scope = match plug_def.scope {
                    crate::procedure::schema::Scope::Execution => PlugScope::Execution,
                    crate::procedure::schema::Scope::Slot => PlugScope::Slot,
                    crate::procedure::schema::Scope::Station => {
                        if self.station_plug_host.is_some() {
                            PlugScope::Station
                        } else {
                            // No station context to own the instance (one-shot
                            // run, Studio): degrade to execution scope so the plug
                            // still gets a single shared instance for this
                            // execution and is torn down with it.
                            log::info!(
                                "Plug '{}' has scope: station but no station \
                                 context is present; behaving as execution-scoped \
                                 for this execution",
                                plug_def.key
                            );
                            PlugScope::Execution
                        }
                    }
                };
                scopes.insert(plug_def.key.clone(), scope);
            }
            let resource_manager = self.resource_manager.write().await;
            resource_manager.set_plug_scopes(scopes).await;

            // NOTE: Execution-scope plugs will be created before first SetupAll phase runs
        }

        // Hand back station plugs this procedure no longer declares before
        // the run asks for the ones it does. A key that was renamed or
        // deleted is never acquired again, so nothing else would ever
        // reach that instance — and its interpreter is still holding the
        // instrument the new key is about to open.
        if let Some(host) = &self.station_plug_host {
            let declared: std::collections::HashSet<String> = self
                .procedure_definition
                .plugs
                .iter()
                .filter(|p| p.scope_is_station())
                .map(|p| p.key.clone())
                .collect();
            host.release_absent(
                &self.procedure_dir,
                &self.python_path,
                &declared,
                &self.event_sink,
            )
            .await;
        }

        let mut state = self.state.write().await;
        state.slots = slots.clone();
        state.slot_first = matches!(execution_strategy, ExecutionStrategy::SlotFirst);

        // Set should_stop_on_first_failure flag from procedure configuration
        state.should_stop_on_first_failure = self
            .procedure_definition
            .execution
            .as_ref()
            .map(|e| {
                matches!(
                    e.on_first_failure,
                    crate::procedure::schema::FirstFailureAction::Stop
                )
            })
            .unwrap_or(true);
        if state.should_stop_on_first_failure {
            log::info!("on_first_failure is set to STOP - test will stop on first phase failure");
        }

        // Validate configuration consistency and emit warnings
        if let Some(exec_config) = &self.procedure_definition.execution {
            let all_phases = self.procedure_definition.get_all_phases_with_stage_scope();
            let phase_defs: Vec<_> = all_phases.iter().map(|(_, phase)| *phase).collect();
            let warnings = exec_config.validate_consistency(&phase_defs);
            for warning in warnings {
                log::warn!("{warning}");
            }
        }

        // Initialize display based on CLI mode preferences
        {
            let _total_phases = self
                .procedure_definition
                .get_all_phases_with_stage_scope()
                .into_iter()
                .filter(|(_, phase)| !phase.should_skip())
                .count();
        }

        // Check queue size limit using total phase count
        let total_phases = self.procedure_definition.total_phase_count();
        if state.job_queue.len() + (slots.len() * total_phases) > limits::MAX_JOB_QUEUE_SIZE {
            return Err(format!(
                "Too many phases: {} slots x {} phases, max {}",
                slots.len(),
                total_phases,
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
        // Track per-slot teardown job IDs per slot for implicit dependencies
        let mut teardown_slot_job_ids: HashMap<String, HashSet<Uuid>> = HashMap::new();
        // Track ALL per-slot teardown job IDs across all slots for shared teardown dependencies
        let mut all_teardown_slot_job_ids: HashSet<Uuid> = HashSet::new();

        // First pass: create all jobs for all stage/scope combinations and store their IDs for dependency resolution

        // Cache the phase list to avoid re-iteration
        let all_phases_with_stage = self.procedure_definition.get_all_phases_with_stage_scope();

        // Create the shared phases once, for every slot
        // No partial-run filter here: this loop only creates SetupAll /
        // TeardownAll jobs, and setup/teardown phases always run.
        for &(stage_scope, phase) in all_phases_with_stage.iter() {
            if phase.should_skip() {
                continue;
            }

            match stage_scope {
                StageScope::SetupAll | StageScope::TeardownAll => {
                    // Shared teardown must wait for all per-slot teardown phases
                    // (will be updated in second pass after we create per-slot teardown jobs)

                    // Shared phases carry no slot
                    let job = jobs::create_job_for_phase(
                        phase,
                        None, // No slot = shared across all slots
                        stage_scope,
                        &global_job_map,
                        &self.procedure_dir,
                        &self.procedure_definition,
                        partial_required_plugs.as_deref(),
                    );

                    // Store mapping for dependency resolution (use key for matching)
                    let key = format!("SHARED:{}", phase.key);
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

                // Partial run: main phases outside the target's dependency
                // closure get no job at all. The implicit stage wiring
                // below operates on whatever jobs exist and stays correct.
                if let Some(set) = &partial_main_set {
                    if matches!(stage_scope, StageScope::Main) && !set.contains(&phase.key) {
                        continue;
                    }
                }

                match stage_scope {
                    StageScope::SetupEach | StageScope::Main | StageScope::TeardownEach => {
                        // Create slot-specific phases (implicit dependencies added later)
                        let mut job = jobs::create_job_for_phase(
                            phase,
                            Some(slot_id.clone()),
                            stage_scope,
                            &global_job_map,
                            &self.procedure_dir,
                            &self.procedure_definition,
                            partial_required_plugs.as_deref(),
                        );

                        // Add implicit dependencies based on stage/scope
                        match stage_scope {
                            StageScope::SetupEach => {
                                // Per-slot setup phases must wait for ALL shared setup phases
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                            }
                            StageScope::Main => {
                                // Main phases must wait for:
                                // 1. ALL shared setup phases
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                                // 2. Their slot's per-slot setup phases (will be added after we create them)
                            }
                            StageScope::TeardownEach => {
                                // Per-slot teardown phases must wait for ALL shared setup phases
                                // (Main phase dependencies will ensure proper ordering)
                                job.depends_on
                                    .extend(setup_procedure_job_ids.iter().copied());
                            }
                            _ => {}
                        }

                        // Store mapping for dependency resolution (use key for matching)
                        let key = format!("{}:{}", slot_id, phase.key);
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
                        // Skip shared phases - already created above
                    }
                }
            }
        }

        // Explicit `depends_on` again, now that every job exists: jobs are
        // created in file order, so a dependency on a phase further down
        // was looked up before its job was in the map and dropped.
        {
            let phases = self.procedure_definition.get_all_phases_with_stage_scope();
            for job in &mut all_jobs {
                let Some((_, phase)) = phases
                    .iter()
                    .find(|(stage, p)| *stage == job.stage_scope && p.key == job.phase_key)
                else {
                    continue;
                };
                for dep in &phase.depends_on {
                    let found = crate::job::resolve_dependency(
                        &global_job_map,
                        job.slot_id.as_deref(),
                        dep,
                    );
                    // An earlier-stage dependency without a matching job
                    // (a shared teardown naming a per-slot main phase) is
                    // already implied by the stage order: nothing to warn.
                    let implied = phases
                        .iter()
                        .find(|(_, p)| &p.key == dep)
                        .is_some_and(|(s, _)| s.rank() < job.stage_scope.rank());
                    match found {
                        Some(id) => {
                            job.depends_on.insert(id);
                        }
                        None if implied => {}
                        None => match &job.slot_id {
                            Some(slot) => log::warn!(
                                "Ignored `depends_on: {dep}` of phase {} (slot {slot}): {dep} skipped",
                                job.phase_key
                            ),
                            None => log::warn!(
                                "Ignored `depends_on: {dep}` of phase {}: {dep} skipped",
                                job.phase_key
                            ),
                        },
                    }
                }
            }
        }

        // Second pass: Update phase dependencies to include implicit cross-phase dependencies
        for job in &mut all_jobs {
            match job.stage_scope {
                StageScope::SetupEach => {
                    // Per-slot setup phases must wait for ALL shared setup phases to complete
                    job.depends_on
                        .extend(setup_procedure_job_ids.iter().copied());
                }
                StageScope::Main => {
                    // Main phases need their slot's per-slot setup phases as dependencies
                    if let Some(slot_id) = &job.slot_id {
                        if let Some(setup_jobs) = setup_slot_job_ids.get(slot_id) {
                            job.depends_on.extend(setup_jobs.iter().copied());
                        }
                    }
                }
                StageScope::TeardownEach => {
                    // Per-slot teardown phases need their slot's Main phases as dependencies
                    if let Some(slot_id) = &job.slot_id {
                        if let Some(main_jobs) = main_phase_job_ids.get(slot_id) {
                            job.depends_on.extend(main_jobs.iter().copied());
                        }
                    }
                }
                StageScope::TeardownAll => {
                    // Shared teardown phases must wait for ALL Main phases AND all TeardownEach phases
                    // This ensures teardown runs after all main work is complete, even if no TeardownEach phases exist
                    for main_jobs in main_phase_job_ids.values() {
                        job.depends_on.extend(main_jobs.iter().copied());
                    }
                    job.depends_on
                        .extend(all_teardown_slot_job_ids.iter().copied());
                }
                _ => {}
            }
        }

        // What each job's Python receives from the run's values, resolved
        // for its slot: `with:` kwargs, `env:`, the `run` context, config and
        // secrets. Retries clone the job, and keep them.
        if let Some(values) = &self.run_values {
            for job in &mut all_jobs {
                if let Some(phase) = all_phases_with_stage
                    .iter()
                    .find(|(_, p)| p.key == job.phase_key)
                    .map(|(_, p)| *p)
                {
                    job.run = job_run_values(
                        &self.procedure_definition,
                        phase,
                        job.slot_id.as_deref(),
                        values,
                    );
                }
            }
        }
        for job in &mut all_jobs {
            job.capture_locals = self.capture_locals;
        }

        // Every sequence scope knows its jobs before any is enqueued, so a
        // scope with nothing left to run is told apart from one whose jobs
        // are still deferred (slot-first), and a call with no job settles.
        state.register_sequence_jobs(&all_jobs, &self.procedure_definition.sequences, &slots);

        // Third pass: enqueue jobs in proper execution order based on stage/scope combinations
        use crate::procedure::schema::StageScope;

        match execution_strategy {
            ExecutionStrategy::SlotFirst => {
                // Slot-first: complete all phases for each slot before moving to next
                log::info!("Using SLOT-FIRST execution model");

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
                    current_slot_jobs.extend(jobs::filter_jobs_by_slot_and_type(
                        &all_jobs,
                        slot_id,
                        StageScope::SetupEach,
                    ));
                    current_slot_jobs.extend(jobs::filter_jobs_by_slot_and_type(
                        &all_jobs,
                        slot_id,
                        StageScope::Main,
                    ));
                    current_slot_jobs.extend(jobs::filter_jobs_by_slot_and_type(
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
                    log::trace!("📦 Starting with slot: {}", first_slot_id);
                    for job in first_slot_jobs {
                        state.enqueue_job(job.clone());
                    }
                }

                // Store remaining slots for later
                if slot_jobs.len() > 1 {
                    state.pending_slot_jobs = slot_jobs.into_iter().skip(1).collect();
                    log::info!(
                        "{} slots queued for sequential processing",
                        state.pending_slot_jobs.len()
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
                jobs::enqueue_jobs_by_stage_scope(
                    &mut state,
                    &self.procedure_definition,
                    &all_jobs,
                    StageScope::SetupAll,
                    true,
                );
                jobs::enqueue_jobs_by_stage_scope(
                    &mut state,
                    &self.procedure_definition,
                    &all_jobs,
                    StageScope::SetupEach,
                    false,
                );
                jobs::enqueue_jobs_by_stage_scope(
                    &mut state,
                    &self.procedure_definition,
                    &all_jobs,
                    StageScope::Main,
                    false,
                );
                jobs::enqueue_jobs_by_stage_scope(
                    &mut state,
                    &self.procedure_definition,
                    &all_jobs,
                    StageScope::TeardownEach,
                    false,
                );
                jobs::enqueue_jobs_by_stage_scope(
                    &mut state,
                    &self.procedure_definition,
                    &all_jobs,
                    StageScope::TeardownAll,
                    true,
                );
            }
        }

        // Add plug scope operations to total job count for progress tracking
        // emit_plug_scope_event fires once per scope-batch, not per-plug:
        //   init:     1 event if execution-scope plugs/SetupAll exist, 1 per slot if slot-scope plugs/SetupEach exist
        //   teardown: 1 event if execution-scope plugs exist, 1 per slot if slot-scope plugs exist
        // Station plugs held by a host count like execution-scope for the init
        // batch (one acquire event) but contribute no teardown event —
        // they outlive the run. Without a host they degrade to execution scope
        // (see set_plug_scopes above) and count as execution-scope on both
        // sides.
        //
        // Reservations must agree with the emit gates: on a partial run the
        // creation/acquisition/teardown paths only ever see plugs in the
        // introspected union, so a declared plug outside it emits nothing
        // and must not reserve an event slot — a reserved-but-never-emitted
        // pair leaves run_progress permanently short of total. Full runs
        // have no union and count every declared plug, as before.
        let in_union = |key: &String| {
            partial_required_plugs
                .as_ref()
                .is_none_or(|union| union.contains(key))
        };
        let has_station_host = self.station_plug_host.is_some();
        let has_station_scope_plugs = self
            .procedure_definition
            .plugs
            .iter()
            .any(|p| p.scope == crate::procedure::schema::Scope::Station && in_union(&p.key));
        let has_execution_scope_plugs = self
            .procedure_definition
            .plugs
            .iter()
            .any(|p| p.scope == crate::procedure::schema::Scope::Execution && in_union(&p.key))
            || (has_station_scope_plugs && !has_station_host);
        let has_slot_scope_plugs = self
            .procedure_definition
            .plugs
            .iter()
            .any(|p| p.scope == crate::procedure::schema::Scope::Slot && in_union(&p.key));
        let has_hosted_station_plugs = has_station_scope_plugs && has_station_host;

        let all_phases = self.procedure_definition.get_all_phases_with_stage_scope();
        let has_setup_all = all_phases.iter().any(|(s, p)| {
            matches!(s, crate::procedure::schema::StageScope::SetupAll) && !p.should_skip()
        });
        let has_setup_each = all_phases.iter().any(|(s, p)| {
            matches!(s, crate::procedure::schema::StageScope::SetupEach) && !p.should_skip()
        });

        // The station branch and the execution branch in
        // ensure_plugs_created_for_job are INDEPENDENT emitters — each
        // fires its own terminal plug-scope event. A procedure with a
        // hosted station plug AND a SetupAll trigger produces two init
        // events, so they need two reserved slots, not a shared one
        // (sharing made progress overshoot 100%).
        let init_events = (if has_execution_scope_plugs || has_setup_all {
            1
        } else {
            0
        }) + (if has_hosted_station_plugs { 1 } else { 0 })
            + (if has_slot_scope_plugs || has_setup_each {
                slots.len()
            } else {
                0
            });
        let teardown_events = (if has_execution_scope_plugs { 1 } else { 0 })
            + (if has_slot_scope_plugs { slots.len() } else { 0 });
        let plug_scope_operations = init_events + teardown_events;
        state.reserve_plug_scope_ops(plug_scope_operations);

        // Emit execution plan to frontend, narrowed to what this run
        // will actually execute — the UI paints anything planned but
        // never run with the final outcome, and seeds a card for every
        // planned plug.
        self.emit_execution_plan(
            &self.procedure_definition,
            &state,
            &slots,
            partial_main_set.as_ref(),
            partial_required_plugs.as_deref(),
        )
        .await;

        // A call none of whose phases runs (all disabled, or outside a
        // partial run) has nothing to wait for: it settles as Skip now.
        if !self.procedure_definition.sequences.is_empty() {
            self.settle_sequences(&mut state).await;
        }

        Ok(())
    }
}

/// Union of `plug_keys_for_phase` over every phase a partial run will
/// execute: all setup, the main-stage partial set, all teardown — not the
/// target phase alone, or the setup phase that initialises an instrument
/// runs without it.
///
/// A `None` from introspection (import error) makes the whole union fall
/// back to every declared plug — the degradation over-starts instead of
/// silently under-starting. `Some(vec![])` contributes nothing, which is
/// what lets a play on a callable-less phase start no plug at all.
///
/// Keys come back in declaration order.
/// The slot a job runs for, with its display name (the key when the slot
/// has none); `None` for a shared job.
pub(super) fn slot_context(
    procedure: &crate::procedure::schema::ProcedureDefinition,
    slot_id: Option<&str>,
) -> Option<crate::procedure::SlotContext> {
    let key = slot_id?;
    let name = procedure
        .execution
        .as_ref()
        .and_then(|e| e.slots.iter().find(|s| s.key == key))
        .map(|s| s.name.clone())
        .unwrap_or_else(|| key.to_string());
    Some(crate::procedure::SlotContext {
        key: key.to_string(),
        name,
    })
}

/// What one job of `phase` receives from the run's values (see
/// `Job::run`). A phase of a sequence reads the keys its file declares, a
/// phase of the procedure the procedure's; a declared key without a value
/// reads as `null`.
pub(super) fn job_run_values(
    procedure: &crate::procedure::schema::ProcedureDefinition,
    phase: &crate::procedure::schema::PhaseDefinition,
    slot_id: Option<&str>,
    values: &crate::procedure::RunValues,
) -> crate::job::JobRunValues {
    let slot = slot_context(procedure, slot_id);
    let slot = slot.as_ref();
    let (config_keys, secret_keys): (Vec<&String>, Vec<&String>) = match &phase.value_scope {
        Some(scope) => (
            scope.config.iter().collect(),
            scope.secrets.iter().collect(),
        ),
        None => (
            procedure.config.keys().collect(),
            procedure.secrets.keys().collect(),
        ),
    };
    let pick =
        |keys: Vec<&String>,
         from: &std::collections::BTreeMap<String, crate::procedure::ResolvedValue>| {
            keys.into_iter()
                .map(|k| {
                    (
                        k.clone(),
                        from.get(k).map(|v| v.value.clone()).unwrap_or_default(),
                    )
                })
                .collect()
        };
    let with = match values.expand(
        &serde_json::Value::Object(phase.with.clone().into_iter().collect()),
        slot,
    ) {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => Default::default(),
    };
    crate::job::JobRunValues {
        with,
        env: values.expand_env(&phase.env, slot),
        context: values.context.to_json(slot),
        config: pick(config_keys, &values.config),
        secrets: pick(secret_keys, &values.secrets),
    }
}

fn union_required_plugs(
    procedure: &crate::procedure::schema::ProcedureDefinition,
    partial_main_set: &HashSet<String>,
    introspection: &crate::procedure::Introspection,
) -> Vec<String> {
    let all_keys = || {
        procedure
            .plugs
            .iter()
            .map(|p| p.key.clone())
            .collect::<Vec<_>>()
    };

    let mut needed: HashSet<String> = HashSet::new();
    for (stage_scope, phase) in procedure.iter_phases_with_stage() {
        if phase.should_skip() {
            continue;
        }
        let runs = match stage_scope {
            crate::procedure::schema::StageScope::Main => partial_main_set.contains(&phase.key),
            // Setup and teardown phases always run.
            _ => true,
        };
        if !runs {
            continue;
        }
        match procedure.plug_keys_for_phase(phase, introspection) {
            Some(keys) => needed.extend(keys),
            None => return all_keys(),
        }
    }

    procedure
        .plugs
        .iter()
        .filter(|p| needed.contains(&p.key))
        .map(|p| p.key.clone())
        .collect()
}

/// `partial_main_phase_set` for several targets: the union of their
/// closures, refused at the first invalid one.
pub fn partial_main_phase_set_many(
    procedure: &crate::procedure::schema::ProcedureDefinition,
    targets: &[String],
) -> Result<HashSet<String>, String> {
    let mut set = HashSet::new();
    for target in targets {
        set.extend(partial_main_phase_set(procedure, target)?);
    }
    Ok(set)
}

/// Main-stage phase set for a partial run: the target plus the transitive
/// `depends_on` closure of the target. `depends_on` only resolves within a
/// stage (the Builder enforces this), so the walk stays inside `main`.
/// Setup and teardown phases are not part of the set — they always run and
/// are never filtered.
///
/// Errors when the target key is unknown or names a setup/teardown phase.
/// Dependencies on unknown keys are tolerated, exactly like the job-graph
/// builder tolerates them.
pub fn partial_main_phase_set(
    procedure: &crate::procedure::schema::ProcedureDefinition,
    target: &str,
) -> Result<HashSet<String>, String> {
    if procedure
        .setup
        .iter()
        .chain(procedure.teardown.iter())
        .any(|p| p.key == target)
    {
        return Err(format!(
            "`{target}` is a setup or teardown phase, which always runs; a partial run targets main phases"
        ));
    }

    let by_key: HashMap<&str, &crate::procedure::schema::PhaseDefinition> =
        procedure.main.iter().map(|p| (p.key.as_str(), p)).collect();

    // A sequence call as the target: every phase it brought into main,
    // plus their dependency closure (the call's own `depends_on` sits on
    // each of them). A call made from setup or teardown is refused like a
    // setup or teardown phase.
    let mut stack: Vec<String> = Vec::new();
    if let Some(call) = procedure.sequences.iter().find(|s| s.path_str() == target) {
        if call.stage != crate::procedure::schema::PhaseStage::Setup
            && call.stage != crate::procedure::schema::PhaseStage::Teardown
            || call.path.len() > 1
        {
            let root_stage = procedure
                .sequences
                .iter()
                .find(|s| s.path.len() == 1 && s.path[0] == call.path[0])
                .map(|s| s.stage);
            if root_stage == Some(crate::procedure::schema::PhaseStage::Main) {
                stack.extend(
                    call.all_keys()
                        .filter(|k| by_key.contains_key(k.as_str()))
                        .cloned(),
                );
            }
        }
        if stack.is_empty() {
            return Err(format!(
                "`{target}` is a sequence called from setup or teardown; \
                 a partial run targets main only"
            ));
        }
    } else if !by_key.contains_key(target) {
        let mut keys: Vec<&str> = by_key.keys().copied().collect();
        keys.sort_unstable();
        return Err(
            match crate::procedure::pysource::closest(target, keys.iter().copied())
                .filter(|near| near.len().max(target.len()) > 2)
            {
                Some(near) => format!("no main phase `{target}`; did you mean `{near}`?"),
                None => format!(
                    "no main phase `{target}` (main phases: {})",
                    keys.join(", ")
                ),
            },
        );
    } else {
        stack.push(target.to_string());
    }

    let mut set: HashSet<String> = HashSet::new();
    while let Some(key) = stack.pop() {
        if !set.insert(key.clone()) {
            // Already visited: diamonds and self-references terminate here.
            continue;
        }
        if let Some(phase) = by_key.get(key.as_str()) {
            for dep in &phase.depends_on {
                if !set.contains(dep) {
                    stack.push(dep.clone());
                }
            }
        }
    }

    // A call entered by the partial run still tears down: the teardown
    // phases of every call enclosing a phase of the set join it. The
    // call's setup came along through the barrier the main phases depend
    // on; its teardown depends on them, not the reverse, so the closure
    // alone would leave the bench powered. Their own dependencies are not
    // walked: a dependency on a key that does not run is tolerated by the
    // job builder, so the teardown still waits for what does run.
    let in_set: Vec<String> = set.iter().cloned().collect();
    for call in &procedure.sequences {
        let prefix = format!("{}/", call.path_str());
        if in_set.iter().any(|k| k.starts_with(&prefix)) {
            set.extend(
                call.teardown_keys
                    .iter()
                    .filter(|k| by_key.contains_key(k.as_str()))
                    .cloned(),
            );
        }
    }

    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::schema::{ProcedureDefinition, ProcedureYaml};

    fn procedure(yaml: &str) -> ProcedureDefinition {
        let raw: ProcedureYaml = crate::procedure::yaml::from_str(yaml).unwrap();
        ProcedureDefinition::try_from(raw).unwrap()
    }

    fn set(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    const PROCEDURE: &str = r#"
name: test
version: 1.0.0
setup:
  - key: prepare
    name: Prepare
main:
  - key: a
    name: A
  - key: b
    name: B
    depends_on: [a]
  - key: c
    name: C
    depends_on: [b]
  - key: d1
    name: D1
    depends_on: [a]
  - key: d2
    name: D2
    depends_on: [a]
  - key: diamond
    name: Diamond
    depends_on: [d1, d2]
  - key: selfref
    name: Selfref
    depends_on: [selfref]
  - key: dangling
    name: Dangling
    depends_on: [does_not_exist]
teardown:
  - key: cleanup
    name: Cleanup
"#;

    #[test]
    fn chain_walks_to_fixpoint() {
        let def = procedure(PROCEDURE);
        assert_eq!(
            partial_main_phase_set(&def, "c").unwrap(),
            set(&["c", "b", "a"])
        );
    }

    #[test]
    fn diamond_visits_each_node_once() {
        let def = procedure(PROCEDURE);
        assert_eq!(
            partial_main_phase_set(&def, "diamond").unwrap(),
            set(&["diamond", "d1", "d2", "a"])
        );
    }

    #[test]
    fn self_reference_terminates() {
        let def = procedure(PROCEDURE);
        assert_eq!(
            partial_main_phase_set(&def, "selfref").unwrap(),
            set(&["selfref"])
        );
    }

    #[test]
    fn unknown_target_is_an_error() {
        let def = procedure(PROCEDURE);
        assert!(partial_main_phase_set(&def, "nope").is_err());
    }

    #[test]
    fn setup_and_teardown_targets_are_rejected() {
        let def = procedure(PROCEDURE);
        assert!(partial_main_phase_set(&def, "prepare").is_err());
        assert!(partial_main_phase_set(&def, "cleanup").is_err());
    }

    #[test]
    fn target_with_no_deps_is_just_itself() {
        let def = procedure(PROCEDURE);
        assert_eq!(partial_main_phase_set(&def, "a").unwrap(), set(&["a"]));
    }

    #[test]
    fn unknown_dependency_keys_are_tolerated() {
        let def = procedure(PROCEDURE);
        assert_eq!(
            partial_main_phase_set(&def, "dangling").unwrap(),
            set(&["dangling", "does_not_exist"])
        );
    }

    // --- union_required_plugs ---

    use crate::procedure::{Introspection, PhaseSignature};

    const THREE_PLUG_PROCEDURE: &str = r#"
name: test
version: 1.0.0
plugs:
  - name: p1
    python: plugs.p1:P1
  - name: p2
    python: plugs.p2:P2
  - name: p3
    python: plugs.p3:P3
setup:
  - key: prep
    name: Prep
    python: phases.s:prep
main:
  - key: a
    name: A
    python: phases.m:a
  - key: b
    name: B
  - key: c
    name: C
    python: phases.m:c
teardown:
  - key: cleanup
    name: Cleanup
    python: phases.t:cleanup
"#;

    fn signature(params: &[&str]) -> PhaseSignature {
        PhaseSignature {
            params: Some(params.iter().map(|s| s.to_string()).collect()),
            error: None,
        }
    }

    fn intr(entries: Vec<(&str, PhaseSignature)>) -> Introspection {
        Introspection {
            phases: entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    #[test]
    fn partial_set_starts_one_plug_of_three() {
        let def = procedure(THREE_PLUG_PROCEDURE);
        let i = intr(vec![
            ("prep", signature(&["phase"])),
            ("a", signature(&["measurements", "p1"])),
            ("cleanup", signature(&["phase"])),
        ]);
        assert_eq!(
            union_required_plugs(&def, &set(&["a"]), &i),
            vec!["p1".to_string()]
        );
    }

    #[test]
    fn setup_phase_pulls_in_plug_the_target_does_not_use() {
        let def = procedure(THREE_PLUG_PROCEDURE);
        let i = intr(vec![
            ("prep", signature(&["phase", "p2"])),
            ("a", signature(&["measurements", "p1"])),
            ("cleanup", signature(&["phase"])),
        ]);
        assert_eq!(
            union_required_plugs(&def, &set(&["a"]), &i),
            vec!["p1".to_string(), "p2".to_string()]
        );
    }

    #[test]
    fn phase_with_no_plugs_starts_none() {
        let def = procedure(THREE_PLUG_PROCEDURE);
        // Target `b` has no callable at all; setup/teardown take no plug.
        let i = intr(vec![
            ("prep", signature(&["phase"])),
            ("cleanup", signature(&["phase"])),
        ]);
        assert_eq!(
            union_required_plugs(&def, &set(&["b"]), &i),
            Vec::<String>::new()
        );
    }

    #[test]
    fn import_error_in_the_set_starts_all_plugs() {
        let def = procedure(THREE_PLUG_PROCEDURE);
        let i = intr(vec![
            ("prep", signature(&["phase"])),
            (
                "c",
                PhaseSignature {
                    params: None,
                    error: Some("No module named 'serial'".to_string()),
                },
            ),
            ("cleanup", signature(&["phase"])),
        ]);
        assert_eq!(
            union_required_plugs(&def, &set(&["c"]), &i),
            vec!["p1".to_string(), "p2".to_string(), "p3".to_string()]
        );
    }

    #[test]
    fn import_error_outside_the_set_is_ignored() {
        let def = procedure(THREE_PLUG_PROCEDURE);
        // `c` fails to import but isn't in the partial set: its error
        // must not widen the union (or stop the run).
        let i = intr(vec![
            ("prep", signature(&["phase"])),
            ("a", signature(&["p1"])),
            (
                "c",
                PhaseSignature {
                    params: None,
                    error: Some("No module named 'serial'".to_string()),
                },
            ),
            ("cleanup", signature(&["phase"])),
        ]);
        assert_eq!(
            union_required_plugs(&def, &set(&["a"]), &i),
            vec!["p1".to_string()]
        );
    }
}
