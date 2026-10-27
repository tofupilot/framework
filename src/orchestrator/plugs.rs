//! Plug scope management

use std::collections::{HashMap, HashSet};

use crate::procedure::schema::{ProcedureDefinition, StageScope};

use crate::job::Job;

use super::Orchestrator;
impl Orchestrator {
    /// Ensure plugs are created at the appropriate scope boundaries
    ///
    /// Lifecycle boundaries:
    /// - Station (scope: station, host present): Borrowed from the
    ///   station plug host once per execution, before the first phase
    ///   that needs them. Never destroyed by the run.
    /// - Execution-wide (scope: execution): Created once before the first phase that needs them
    /// - Per-slot (scope: slot): Created per-slot before the first phase that needs them for that slot
    pub(super) async fn ensure_plugs_created_for_job(&self, job: &Job) -> Result<(), String> {
        let procedure_def = &self.procedure_definition;

        // Borrow station plugs from the host if not yet borrowed this
        // execution. Without a host, station plugs were downgraded to
        // execution scope at init and flow through the branch below instead.
        if let Some(host) = &self.station_plug_host {
            let needs_station_plugs = matches!(job.stage_scope, StageScope::SetupAll)
                || procedure_def
                    .plugs
                    .iter()
                    .any(|p| p.scope_is_station() && job.required_plugs.contains(&p.key));

            if needs_station_plugs {
                let mut acquired = self.station_plugs_acquired.write().await;
                if !*acquired {
                    // Narrowed to the run's required set like the two
                    // branches below: on a partial run only station plugs
                    // in the introspected union are borrowed, so the host
                    // never spins up instruments the selected phases can't
                    // touch (and never emits plug_status for plugs absent
                    // from the execution plan).
                    let station_plugs: Vec<_> = procedure_def
                        .plugs
                        .iter()
                        .filter(|p| p.scope_is_station() && job.required_plugs.contains(&p.key))
                        .collect();

                    if !station_plugs.is_empty() {
                        log::info!(
                            "Borrowing {} station plug(s) before phase '{}'",
                            station_plugs.len(),
                            job.phase_name
                        );
                        self.emit_plug_scope_event("running").await;

                        // Acquire first, register under the resource-
                        // manager guard second, emit last. Keeping the
                        // rm guard out of scope during `acquire` (slow:
                        // may spawn Python) and during the emit (which
                        // takes `state.write()`) preserves the global
                        // `state → resource_manager` lock order that
                        // scheduling.rs documents.
                        // Build every config first so a bad one fails
                        // before any spawn, then acquire concurrently:
                        // `acquire` holds the host lock only for its
                        // bookkeeping, so the spawns overlap.
                        let mut configs = Vec::with_capacity(station_plugs.len());
                        for plug in &station_plugs {
                            let config_json = self.plug_config_json(plug, None).map_err(|e| {
                                format!("Cannot build config for plug {}: {}", plug.key, e)
                            })?;
                            configs.push(config_json);
                        }
                        let results =
                            futures::future::join_all(station_plugs.iter().zip(configs).map(
                                |(plug, config_json)| {
                                    host.acquire(
                                        &self.procedure_dir,
                                        &self.python_path,
                                        &self.procedure_env,
                                        &plug.key,
                                        &plug.name,
                                        config_json,
                                        &self.event_sink,
                                    )
                                },
                            ))
                            .await;

                        let mut acquired_ports = Vec::with_capacity(station_plugs.len());
                        let mut first_err = None;
                        for (plug, result) in station_plugs.iter().zip(results) {
                            match result {
                                Ok(port) => {
                                    acquired_ports.push((plug.key.clone(), plug.name.clone(), port))
                                }
                                Err(e) => {
                                    first_err.get_or_insert(format!(
                                        "Cannot acquire station plug {}: {}",
                                        plug.key, e
                                    ));
                                }
                            }
                        }
                        if let Some(e) = first_err {
                            self.emit_plug_scope_event("error").await;
                            return Err(e);
                        }

                        {
                            let resource_manager = self.resource_manager.write().await;
                            for (key, name, port) in acquired_ports {
                                resource_manager
                                    .register_station_plug(key, name, port)
                                    .await;
                            }
                        }

                        self.emit_plug_scope_event("pass").await;

                        // Latch only after a real acquisition. The filtered
                        // set is per-run constant (`required_plugs` carries
                        // the same union on every job), so an empty set
                        // stays empty all run and re-entering here is a
                        // cheap no-op — while latching on empty would skip
                        // acquisition outright if that invariant ever broke.
                        *acquired = true;
                    }
                }
            }
        }

        // Create execution-wide plugs if not yet created
        // Triggered by: SetupAll phase, or any phase that requires a
        // scope:execution plug — including station plugs downgraded to execution
        // scope when no host is present (the ResourceManager's scope map
        // holds the downgraded value, so create_procedure_plugs will
        // pick them up here).
        //
        // Only the plugs the job's run actually requires are started:
        // `required_plugs` is every declared plug on a full run and the
        // introspected union on a partial run, so this is what keeps a
        // partial run from starting instruments its phases never touch.
        let hostless = self.station_plug_host.is_none();
        let wanted_procedure_keys: Vec<String> = procedure_def
            .plugs
            .iter()
            .filter(|p| {
                (p.scope_is_execution() || (hostless && p.scope_is_station()))
                    && job.required_plugs.contains(&p.key)
            })
            .map(|p| p.key.clone())
            .collect();

        let needs_procedure_plugs =
            matches!(job.stage_scope, StageScope::SetupAll) || !wanted_procedure_keys.is_empty();

        if needs_procedure_plugs {
            let mut created = self.procedure_plugs_created.write().await;
            // `None` until the first job passes this gate: the scope-batch
            // progress event pair fires exactly once per run, even when the
            // first batch has nothing to create (submit_procedure reserved
            // exactly one pair for it).
            let first_batch = created.is_none();
            let created_keys = created.get_or_insert_with(std::collections::HashSet::new);
            let missing: Vec<String> = wanted_procedure_keys
                .iter()
                .filter(|k| !created_keys.contains(*k))
                .cloned()
                .collect();

            if first_batch || !missing.is_empty() {
                log::info!(
                    "Creating execution-scope plugs before phase '{}'",
                    job.phase_name
                );

                let resource_manager = self.resource_manager.write().await;

                if first_batch {
                    // Clean up any manually-started plugs to prevent conflicts
                    let teardown_result = resource_manager
                        .teardown_manual_plugs(&self.event_sink)
                        .await;

                    if let Err(e) = teardown_result {
                        log::debug!("Manual plug teardown: {}", e);
                        // Continue anyway - not fatal
                    }

                    self.emit_plug_scope_event("running").await;
                }

                let plug_configs: HashMap<String, serde_json::Value> = self
                    .get_all_plug_configs(procedure_def, None)
                    .into_iter()
                    .filter(|(key, _)| missing.contains(key))
                    .collect();
                let plug_display_names = self.get_plug_display_names(procedure_def);
                let plug_result = resource_manager
                    .create_procedure_plugs(&plug_configs, &plug_display_names, &self.event_sink)
                    .await;

                match plug_result {
                    Ok(_) => {
                        log::info!("Successfully created execution-scope plugs");
                        if first_batch {
                            self.emit_plug_scope_event("pass").await;
                        }
                        created_keys.extend(missing);
                    }
                    Err(e) => {
                        // `e` names the plug already.
                        let error_msg = e.to_string();
                        if first_batch {
                            self.emit_plug_scope_event("error").await;
                        }
                        return Err(error_msg);
                    }
                }
            }
        }

        // Create slot-scope plugs if not yet created for this slot
        // Triggered by: SetupEach phase, or any slot-scoped phase that
        // requires a `scope: slot` plug. Narrowed to the job's required set
        // the same way as the execution-scope branch above; tracking is
        // genuinely per-slot (a multi-slot procedure with no `unit:`
        // block runs on all its slots).
        let wanted_slot_keys: Vec<String> = procedure_def
            .plugs
            .iter()
            .filter(|p| {
                matches!(p.scope, crate::procedure::schema::Scope::Slot)
                    && job.required_plugs.contains(&p.key)
            })
            .map(|p| p.key.clone())
            .collect();

        let needs_slot_plugs = job.slot_id.is_some()
            && (matches!(job.stage_scope, StageScope::SetupEach) || !wanted_slot_keys.is_empty());

        if needs_slot_plugs {
            if let Some(ref slot_id) = job.slot_id {
                let mut created_slots = self.slot_plugs_created.write().await;
                // Entry presence marks the slot's batch event pair as
                // fired, mirroring the execution-scope gate above.
                let first_batch = !created_slots.contains_key(slot_id);
                // Teardown work never opens a slot's instruments: a slot
                // whose plugs never opened (its setup never came) runs
                // the teardown phases that need none, and the ones taking
                // a plug read SKIP, as the docs say.
                if first_batch && job.is_teardown_work() {
                    return Ok(());
                }
                let missing_of = |created: &HashMap<String, HashSet<String>>, slot: &str| {
                    wanted_slot_keys
                        .iter()
                        .filter(|k| !created.get(slot).is_some_and(|c| c.contains(*k)))
                        .cloned()
                        .collect::<Vec<String>>()
                };
                let missing = missing_of(&created_slots, slot_id);

                if first_batch || !missing.is_empty() {
                    // The other slots' first batch opens with this one:
                    // their phases come next and would each wait out the
                    // same `__init__`, one slot after the other, with the
                    // scheduler (and every event behind it) held up.
                    // Slot-first runs a slot at a time on purpose.
                    let mut slots = vec![slot_id.clone()];
                    if first_batch {
                        let state = self.state.read().await;
                        if !state.slot_first {
                            slots.extend(
                                state
                                    .slots
                                    .iter()
                                    .filter(|s| *s != slot_id && !created_slots.contains_key(*s))
                                    .cloned(),
                            );
                        }
                    }
                    log::info!(
                        "Creating slot-scope plugs for {} before phase '{}'",
                        slots.join(", "),
                        job.phase_name
                    );

                    if first_batch {
                        for _ in &slots {
                            self.emit_plug_scope_event("running").await;
                        }
                    }

                    let batches: Vec<(String, HashMap<String, serde_json::Value>)> = slots
                        .iter()
                        .map(|slot| {
                            let wanted = missing_of(&created_slots, slot);
                            let configs = self
                                .get_all_plug_configs(procedure_def, Some(slot))
                                .into_iter()
                                .filter(|(key, _)| wanted.contains(key))
                                .collect();
                            (slot.clone(), configs)
                        })
                        .collect();
                    let resource_manager = self.resource_manager.write().await;
                    let plug_display_names = self.get_plug_display_names(procedure_def);
                    let plug_result = resource_manager
                        .create_slot_plugs(batches, &plug_display_names, &self.event_sink)
                        .await;
                    // A slot whose instruments opened has its teardown to
                    // run (an `__init__` may close a relay, energize a
                    // load), whether or not a phase of it ran, and
                    // whether or not the batch failed elsewhere.
                    let mut opened = Vec::new();
                    for slot in &slots {
                        if resource_manager.has_slot_scope_plugs(slot).await {
                            opened.push(slot.clone());
                        }
                    }
                    drop(resource_manager);
                    self.state.write().await.started_slots.extend(opened);

                    match plug_result {
                        Ok(_) => {
                            log::info!(
                                "Successfully created slot-scope plugs for {}",
                                slots.join(", ")
                            );
                            for slot in &slots {
                                let wanted = missing_of(&created_slots, slot);
                                created_slots
                                    .entry(slot.clone())
                                    .or_default()
                                    .extend(wanted);
                                if first_batch {
                                    self.emit_plug_scope_event("pass").await;
                                }
                            }
                        }
                        Err(e) => {
                            // The slot named only when there is more than
                            // one to tell apart.
                            let multi_slot = procedure_def
                                .execution
                                .as_ref()
                                .is_some_and(|x| x.slots.len() > 1);
                            let error_msg = if multi_slot && slots.len() == 1 {
                                format!("slot {slot_id}: {e}")
                            } else {
                                e
                            };
                            if first_batch {
                                for slot in &slots {
                                    created_slots.entry(slot.clone()).or_default();
                                    self.emit_plug_scope_event("error").await;
                                }
                            }
                            return Err(error_msg);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// One plug's spawn config with its `with:` and `env:` expanded for
    /// `slot_id` (`None` for an execution or station plug).
    pub(super) fn plug_config_json(
        &self,
        plug: &crate::procedure::schema::PlugDefinition,
        slot_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        match &self.run_values {
            Some(values) => {
                let slot = super::initialization::slot_context(&self.procedure_definition, slot_id);
                plug.to_resolved_config_json(&self.procedure_dir, values, slot.as_ref())
            }
            None => plug.to_config_json(&self.procedure_dir),
        }
    }

    /// Every plug's spawn config, expanded for `slot_id` (see
    /// `plug_config_json`).
    pub(super) fn get_all_plug_configs(
        &self,
        procedure: &ProcedureDefinition,
        slot_id: Option<&str>,
    ) -> HashMap<String, serde_json::Value> {
        let mut configs = HashMap::new();
        for def in &procedure.plugs {
            match self.plug_config_json(def, slot_id) {
                Ok(config) => {
                    configs.insert(def.key.clone(), config);
                }
                Err(e) => {
                    log::error!("Cannot build config for plug {}: {}", def.key, e);
                }
            }
        }
        configs
    }

    /// Get all plug display names from the procedure definition
    pub(super) fn get_plug_display_names(
        &self,
        procedure: &ProcedureDefinition,
    ) -> HashMap<String, String> {
        let mut names = HashMap::new();
        for def in &procedure.plugs {
            names.insert(def.key.clone(), def.name.clone());
        }
        names
    }

    /// Get plug configurations for a specific job from the stored procedure definition
    pub(super) fn get_plug_configs_for_job(&self, job: &Job) -> HashMap<String, serde_json::Value> {
        let mut plug_configs = HashMap::new();

        for plug_key in &job.required_plugs {
            let plug_def = self
                .procedure_definition
                .plugs
                .iter()
                .find(|p| &p.key == plug_key);

            if let Some(plug_def) = plug_def {
                match plug_def.to_config_json(&self.procedure_dir) {
                    Ok(config) => {
                        plug_configs.insert(plug_key.clone(), config);
                    }
                    Err(e) => {
                        log::error!("Cannot build config for plug {}: {}", plug_key, e);
                    }
                }
            } else {
                log::warn!(
                    "Plug {} needed by phase {} not declared in procedure.yaml",
                    plug_key,
                    job.phase_key
                );
            }
        }

        plug_configs
    }
}
