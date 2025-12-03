//! Plug scope management

use std::collections::HashMap;


use crate::procedure::schema::{ProcedureDefinition, StageScope};
use tauri::AppHandle;

use crate::execution::job::Job;

use super::orchestrator::Orchestrator;
impl Orchestrator {
    /// Ensure plugs are created at the appropriate scope boundaries
    ///
    /// Lifecycle boundaries:
    /// - All-slots (scope: all): Created once before the first phase that needs them
    /// - Each-slot (scope: each): Created per-slot before the first phase that needs them for that slot
    pub(super) async fn ensure_plugs_created_for_job(
        &self,
        job: &Job,
        app_handle: Option<&AppHandle>,
    ) -> Result<(), String> {
        let procedure_def = &self.procedure_definition;

        // Create all-slots plugs if not yet created
        // Triggered by: SetupAll phase, or any phase that requires a scope:all plug
        let needs_procedure_plugs = matches!(job.stage_scope, StageScope::SetupAll)
            || procedure_def
                .plugs
                .iter()
                .any(|p| p.scope_is_all() && job.required_plugs.contains(&p.key));

        if needs_procedure_plugs {
            let mut procedure_plugs_created = self.procedure_plugs_created.write().await;
            if !*procedure_plugs_created {
                log::info!("Creating all-slots plugs before phase '{}'", job.phase_name);

                // First: Clean up any manually-started plugs to prevent conflicts
                let resource_manager = self.resource_manager.write().await;
                let teardown_result = resource_manager.teardown_manual_plugs(app_handle).await;

                if let Err(e) = teardown_result {
                    log::warn!("Warning during manual plug teardown: {}", e);
                    // Continue anyway - not fatal
                }

                self.emit_plug_scope_event("running").await;

                let all_plug_configs = self.get_all_plug_configs(procedure_def);
                let plug_display_names = self.get_plug_display_names(procedure_def);
                let plug_result = resource_manager
                    .create_procedure_plugs(&all_plug_configs, &plug_display_names, app_handle)
                    .await;

                match plug_result {
                    Ok(_) => {
                        log::info!("Successfully created all-slots plugs");
                        self.emit_plug_scope_event("pass").await;
                        *procedure_plugs_created = true;
                    }
                    Err(e) => {
                        let error_msg = format!("Failed to create all-slots plugs: {}", e);
                        self.emit_plug_scope_event("error").await;
                        return Err(error_msg);
                    }
                }
            }
        }

        // Create each-slot plugs if not yet created for this slot
        // Triggered by: SetupEach phase, or any slot-scoped phase that requires a scope:each plug
        let needs_slot_plugs = job.slot_id.is_some()
            && (matches!(job.stage_scope, StageScope::SetupEach)
                || procedure_def
                    .plugs
                    .iter()
                    .any(|p| !p.scope_is_all() && job.required_plugs.contains(&p.key)));

        if needs_slot_plugs {
            if let Some(ref slot_id) = job.slot_id {
                let mut created_slots = self.slot_plugs_created.write().await;
                if !created_slots.contains(slot_id) {
                    log::info!(
                        "Creating each-slot plugs for {} before phase '{}'",
                        slot_id,
                        job.phase_name
                    );

                    self.emit_plug_scope_event("running").await;

                    let resource_manager = self.resource_manager.write().await;
                    let all_plug_configs = self.get_all_plug_configs(procedure_def);
                    let plug_display_names = self.get_plug_display_names(procedure_def);
                    let plug_result = resource_manager
                        .create_slot_plugs(slot_id.clone(), &all_plug_configs, &plug_display_names, app_handle)
                        .await;

                    match plug_result {
                        Ok(_) => {
                            log::info!(
                                "Successfully created each-slot plugs for {}",
                                slot_id
                            );
                            self.emit_plug_scope_event("pass").await;
                            created_slots.insert(slot_id.clone());
                        }
                        Err(e) => {
                            let error_msg =
                                format!("Failed to create each-slot plugs for {}: {}", slot_id, e);
                            self.emit_plug_scope_event("error").await;
                            return Err(error_msg);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get all plug configurations from the procedure definition
    pub(super) fn get_all_plug_configs(
        &self,
        procedure: &ProcedureDefinition,
    ) -> HashMap<String, serde_json::Value> {
        let mut configs = HashMap::new();
        for def in &procedure.plugs {
            match def.to_config_json(&self.procedure_dir) {
                Ok(config) => {
                    configs.insert(def.key.clone(), config);
                }
                Err(e) => {
                    log::error!("Failed to get config for plug '{}': {}", def.key, e);
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
            let plug_def = self.procedure_definition.plugs.iter().find(|p| &p.key == plug_key);

            if let Some(plug_def) = plug_def {
                match plug_def.to_config_json(&self.procedure_dir) {
                    Ok(config) => {
                        plug_configs.insert(plug_key.clone(), config);
                    }
                    Err(e) => {
                        log::error!("Failed to get config for plug '{}': {}", plug_key, e);
                    }
                }
            } else {
                log::warn!(
                    "WARNING: Warning: Plug '{}' required by job '{}' not found in procedure definition",
                    plug_key, job.phase_name
                );
            }
        }

        plug_configs
    }
}

