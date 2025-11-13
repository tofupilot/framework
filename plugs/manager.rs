use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::plugs::plug_service::PlugServiceManager;
use crate::{PlugStatusUpdateEvent, PlugScope, PlugStage, PlugStatusValue};

fn emit_plug_status(
    app_handle: Option<&tauri::AppHandle>,
    plug_key: String,
    plug_name: String,
    scope: PlugScope,
    slot_id: Option<String>,
    stage: PlugStage,
    status: PlugStatusValue,
) {
    if let Some(app) = app_handle {
        let event = PlugStatusUpdateEvent {
            plug_key: plug_key.clone(),
            plug_name: plug_name.clone(),
            scope,
            slot_id,
            stage,
            status,
        };

        println!("🔌 [BACKEND] Emitting plug-status-update: {:?}", event);
        let _ = app.emit("plug-status-update", event);
    }
}

#[derive(Debug, Clone)]
pub struct ResourceAllocation {
    pub job_id: Uuid,
    pub allocated_resources: HashMap<String, String>, // resource_type -> specific_instance
    pub plug_ports: HashMap<String, u16>,             // plug_name -> port
}

#[derive(Debug, Clone)]
pub struct PlugInstance {
    pub port: u16,
    pub slot_id: Option<String>, // None for all-slots plugs
    pub ref_count: usize,        // Number of jobs using this instance
}

#[derive(Debug)]
pub struct ResourceManager {
    pools: Arc<RwLock<HashMap<String, ResourcePool>>>,
    allocations: Arc<RwLock<Vec<ResourceAllocation>>>,
    plug_service_manager: Arc<PlugServiceManager>,
    // Track plug instances by name and optionally slot
    plug_instances: Arc<RwLock<HashMap<String, PlugInstance>>>, // "plugname" or "plugname_slot1"
    plug_scopes: Arc<RwLock<HashMap<String, PlugScope>>>, // plug_name -> scope
    procedure_plugs_lock: Arc<Mutex<HashSet<String>>>, // Track all-slots plugs in use
    manual_plugs: Arc<RwLock<HashSet<String>>>, // Track manually-started plugs (debug mode)
}

#[derive(Debug)]
struct ResourcePool {
    available: HashSet<String>,
    total: HashSet<String>,
}

impl ResourceManager {
    pub fn new(procedure_dir: PathBuf) -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            allocations: Arc::new(RwLock::new(Vec::new())),
            plug_service_manager: Arc::new(PlugServiceManager::new(procedure_dir)),
            plug_instances: Arc::new(RwLock::new(HashMap::new())),
            plug_scopes: Arc::new(RwLock::new(HashMap::new())),
            procedure_plugs_lock: Arc::new(Mutex::new(HashSet::new())),
            manual_plugs: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub async fn set_plug_scopes(&self, scopes: HashMap<String, PlugScope>) {
        // Store the scopes for all plugs
        let mut plug_scopes = self.plug_scopes.write().await;
        *plug_scopes = scopes;
    }

    pub async fn register_resource_pool(&self, resource_type: String, instances: Vec<String>) {
        let pool = ResourcePool {
            available: instances.iter().cloned().collect(),
            total: instances.iter().cloned().collect(),
        };

        self.pools.write().await.insert(resource_type, pool);
    }

    pub async fn can_allocate_resources(&self, required_resources: &[String]) -> bool {
        let pools = self.pools.read().await;

        for resource_type in required_resources {
            if let Some(pool) = pools.get(resource_type) {
                if pool.available.is_empty() {
                    return false;
                }
            } else {
                continue;
            }
        }

        true
    }

    pub async fn allocate_resources(
        &self,
        job_id: Uuid,
        required_resources: &[String],
    ) -> Result<ResourceAllocation, String> {
        let mut pools = self.pools.write().await;
        let mut allocated = HashMap::new();
        let mut reserved_instances = Vec::new();

        // Try to allocate all required resources
        for resource_type in required_resources {
            if let Some(pool) = pools.get_mut(resource_type) {
                if let Some(instance) = pool.available.iter().next().cloned() {
                    pool.available.remove(&instance);
                    allocated.insert(resource_type.clone(), instance.clone());
                    reserved_instances.push((resource_type.clone(), instance));
                } else {
                    for (rollback_type, rollback_instance) in reserved_instances {
                        if let Some(rollback_pool) = pools.get_mut(&rollback_type) {
                            rollback_pool.available.insert(rollback_instance);
                        }
                    }
                    return Err(format!("No available instances of {}", resource_type));
                }
            }
        }

        let allocation = ResourceAllocation {
            job_id,
            allocated_resources: allocated,
            plug_ports: HashMap::new(), // Will be populated when plugs are started
        };

        self.allocations.write().await.push(allocation.clone());

        Ok(allocation)
    }

    pub async fn release_resources(&self, job_id: Uuid) -> Result<(), String> {
        let mut allocations = self.allocations.write().await;
        let mut pools = self.pools.write().await;

        if let Some(pos) = allocations.iter().position(|a| a.job_id == job_id) {
            let allocation = allocations.remove(pos);
            for (resource_type, instance) in allocation.allocated_resources {
                if let Some(pool) = pools.get_mut(&resource_type) {
                    pool.available.insert(instance);
                }
            }

            Ok(())
        } else {
            Err(format!("No allocation found for job {}", job_id))
        }
    }

    pub async fn get_resource_stats(&self) -> HashMap<String, (usize, usize)> {
        let pools = self.pools.read().await;
        let mut stats = HashMap::new();

        for (resource_type, pool) in pools.iter() {
            stats.insert(
                resource_type.clone(),
                (pool.available.len(), pool.total.len()),
            );
        }

        stats
    }

    /// Start plug services for a job with scope awareness
    pub async fn start_plug_services(
        &self,
        job_id: Uuid,
        plug_configs: &HashMap<String, serde_json::Value>,
    ) -> Result<HashMap<String, u16>, String> {
        self.start_plug_services_for_slot(job_id, plug_configs, None)
            .await
    }

    /// Start plug services for a job, optionally in a specific slot
    pub async fn start_plug_services_for_slot(
        &self,
        job_id: Uuid,
        plug_configs: &HashMap<String, serde_json::Value>,
        slot_id: Option<String>,
    ) -> Result<HashMap<String, u16>, String> {
        let mut plug_ports = HashMap::new();
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        // Start or reuse plug services based on scope
        for plug_name in plug_configs.keys() {
            let scope = scopes
                .get(plug_name)
                .cloned()
                .unwrap_or(PlugScope::Each);

            // Determine the instance key based on scope
            let instance_key = match scope {
                PlugScope::Each => {
                    // For slot-level plugs, include slot ID in the key
                    if let Some(ref slot) = slot_id {
                        format!("{}_{}", plug_name, slot)
                    } else {
                        // If no slot specified, treat as job-level (backward compat)
                        format!("{}_{}", plug_name, job_id)
                    }
                }
                PlugScope::All => {
                    // For all-slots plugs, use just the plug name
                    plug_name.clone()
                }
            };

            // Plug should already exist - just increment reference count
            if let Some(instance) = instances.get_mut(&instance_key) {
                instance.ref_count += 1;
                plug_ports.insert(plug_name.clone(), instance.port);
                crate::cli_output::debug(format!(
                    "Phase using plug {} (ref_count: {})",
                    instance_key, instance.ref_count
                ));
                // No event needed - plug is already ready
            } else {
                return Err(format!(
                    "Plug {} should have been created at scope boundary but doesn't exist",
                    instance_key
                ));
            }

            // For all-slots plugs, track usage for locking
            if matches!(scope, PlugScope::All) {
                let mut lock = self.procedure_plugs_lock.lock().await;
                lock.insert(plug_name.clone());
            }
        }

        // Update the allocation with plug ports
        let mut allocations = self.allocations.write().await;
        if let Some(allocation) = allocations.iter_mut().find(|a| a.job_id == job_id) {
            allocation.plug_ports = plug_ports.clone();
        }

        Ok(plug_ports)
    }

    /// Stop plug services for a job with scope awareness
    pub async fn stop_plug_services(
        &self,
        job_id: Uuid,
    ) -> Result<(), String> {
        self.stop_plug_services_for_slot(job_id, None)
            .await
    }

    /// Stop plug services for a job, handling scope properly
    pub async fn stop_plug_services_for_slot(
        &self,
        job_id: Uuid,
        slot_id: Option<String>,
    ) -> Result<(), String> {
        let allocations = self.allocations.read().await;
        let scopes = self.plug_scopes.read().await;

        if let Some(allocation) = allocations.iter().find(|a| a.job_id == job_id) {
            let mut instances = self.plug_instances.write().await;

            for plug_name in allocation.plug_ports.keys() {
                let scope = scopes
                    .get(plug_name)
                    .cloned()
                    .unwrap_or(PlugScope::Each);

                // Determine the instance key
                let instance_key = match scope {
                    PlugScope::Each => {
                        if let Some(ref slot) = slot_id {
                            format!("{}_{}", plug_name, slot)
                        } else {
                            format!("{}_{}", plug_name, job_id)
                        }
                    }
                    PlugScope::All => plug_name.clone(),
                };

                // Just decrement ref count - don't destroy plugs here
                if let Some(instance) = instances.get_mut(&instance_key) {
                    instance.ref_count -= 1;
                    crate::cli_output::debug(format!(
                        "Phase done using plug {} (ref_count: {})",
                        instance_key, instance.ref_count
                    ));
                    // Plug stays ready - will be destroyed at scope boundary
                }
            }
        }
        Ok(())
    }

    /// Create all-slots plugs at procedure start
    pub async fn create_procedure_plugs(
        &self,
        plug_configs: &HashMap<String, serde_json::Value>,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        for (plug_name, plug_config) in plug_configs {
            let scope = scopes
                .get(plug_name)
                .cloned()
                .unwrap_or(PlugScope::Each);

            if matches!(scope, PlugScope::All) {
                // Only create all-slots plugs here
                let instance_key = plug_name.clone();

                if !instances.contains_key(&instance_key) {
                    // Emit initializing event
                    emit_plug_status(
                        app_handle,
                        instance_key.clone(),
                        plug_name.clone(),
                        scope.clone(),
                        None,
                        PlugStage::Setup,
                        PlugStatusValue::Initializing,
                    );

                    // Start the plug service
                    let port = match self
                        .plug_service_manager
                        .start_plug_service(instance_key.clone(), plug_name.clone(), plug_config.clone(), app_handle)
                        .await
                    {
                        Ok(port) => port,
                        Err(e) => {
                            // Emit error status before returning
                            emit_plug_status(
                                app_handle,
                                instance_key.clone(),
                                plug_name.clone(),
                                scope.clone(),
                                None,
                                PlugStage::Setup,
                                PlugStatusValue::Error,
                            );
                            return Err(e);
                        }
                    };

                    instances.insert(
                        instance_key.clone(),
                        PlugInstance {
                            port,
                            slot_id: None, // All-slots
                            ref_count: 0,  // Will be incremented when slots use it
                        },
                    );

                    crate::cli_output::print_section(
                        crate::cli_output::Section::Plugs,
                        format!(
                            "Created all-slots plug {} on port {}",
                            instance_key, port
                        ),
                    );

                    // Emit ready event
                    emit_plug_status(
                        app_handle,
                        instance_key.clone(),
                        plug_name.clone(),
                        scope.clone(),
                        None,
                        PlugStage::Setup,
                        PlugStatusValue::Active,
                    );
                }
            }
        }

        Ok(())
    }

    /// Create slot-level plugs at slot start
    pub async fn create_slot_plugs(
        &self,
        slot_id: String,
        plug_configs: &HashMap<String, serde_json::Value>,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        for (plug_name, plug_config) in plug_configs {
            let scope = scopes
                .get(plug_name)
                .cloned()
                .unwrap_or(PlugScope::Each);

            if matches!(scope, PlugScope::Each) {
                // Only create slot-level plugs here
                let instance_key = format!("{}_{}", plug_name, slot_id);

                if !instances.contains_key(&instance_key) {
                    // Emit initializing event
                    emit_plug_status(
                        app_handle,
                        instance_key.clone(),
                        plug_name.clone(),
                        scope.clone(),
                        Some(slot_id.clone()),
                        PlugStage::Setup,
                        PlugStatusValue::Initializing,
                    );

                    // Start the plug service
                    let port = match self
                        .plug_service_manager
                        .start_plug_service(instance_key.clone(), plug_name.clone(), plug_config.clone(), app_handle)
                        .await
                    {
                        Ok(port) => port,
                        Err(e) => {
                            // Emit error status before returning
                            emit_plug_status(
                                app_handle,
                                instance_key.clone(),
                                plug_name.clone(),
                                scope.clone(),
                                Some(slot_id.clone()),
                                PlugStage::Setup,
                                PlugStatusValue::Error,
                            );
                            return Err(e);
                        }
                    };

                    instances.insert(
                        instance_key.clone(),
                        PlugInstance {
                            port,
                            slot_id: Some(slot_id.clone()),
                            ref_count: 0, // Will be incremented when phases use it
                        },
                    );

                    crate::cli_output::print_section(
                        crate::cli_output::Section::Plugs,
                        format!("Created slot-level plug {} on port {}", instance_key, port),
                    );

                    // Emit ready event
                    emit_plug_status(
                        app_handle,
                        instance_key.clone(),
                        plug_name.clone(),
                        scope.clone(),
                        Some(slot_id.clone()),
                        PlugStage::Setup,
                        PlugStatusValue::Active,
                    );
                }
            }
        }

        Ok(())
    }

    /// Check if there are any each-scope plugs for a given slot
    pub async fn has_each_scope_plugs(&self, slot_id: &str) -> bool {
        let scopes = self.plug_scopes.read().await;
        let instances = self.plug_instances.read().await;

        instances.keys().any(|key| {
            if key.ends_with(&format!("_{}", slot_id)) {
                let plug_name = key.strip_suffix(&format!("_{}", slot_id)).unwrap_or(key);
                let scope = scopes
                    .get(plug_name)
                    .cloned()
                    .unwrap_or(PlugScope::Each);
                matches!(scope, PlugScope::Each)
            } else {
                false
            }
        })
    }

    /// Check if there are any all-scope plugs
    pub async fn has_all_scope_plugs(&self) -> bool {
        let scopes = self.plug_scopes.read().await;
        let instances = self.plug_instances.read().await;

        instances.keys().any(|key| {
            let scope = scopes.get(key.as_str()).cloned().unwrap_or(PlugScope::Each);
            matches!(scope, PlugScope::All)
        })
    }

    /// Destroy each-scope plugs at slot end
    pub async fn destroy_each_scope_plugs(
        &self,
        slot_id: String,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        let keys_to_remove: Vec<String> = instances
            .keys()
            .filter(|key| {
                if key.ends_with(&format!("_{}", slot_id)) {
                    let plug_name = key.strip_suffix(&format!("_{}", slot_id)).unwrap_or(key);
                    let scope = scopes
                        .get(plug_name)
                        .cloned()
                        .unwrap_or(PlugScope::Each);
                    matches!(scope, PlugScope::Each)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        for instance_key in keys_to_remove {
            if let Some(_instance) = instances.remove(&instance_key) {
                // Extract plug name by removing the "_slot_X" suffix
                let plug_name = instance_key
                    .strip_suffix(&format!("_{}", slot_id))
                    .unwrap_or(&instance_key);

                // Emit stopping event
                emit_plug_status(
                    app_handle,
                    instance_key.clone(),
                    plug_name.to_string(),
                    PlugScope::Each,
                    Some(slot_id.clone()),
                    PlugStage::Teardown,
                    PlugStatusValue::Destructing,
                );

                // Stop the plug service
                if let Err(e) = self
                    .plug_service_manager
                    .stop_plug_service(&instance_key)
                    .await
                {
                    crate::cli_output::warning(format!(
                        "Failed to stop plug service {}: {}",
                        instance_key, e
                    ));
                }

                crate::cli_output::print_section(
                    crate::cli_output::Section::Plugs,
                    format!("Destroyed slot-level plug {}", instance_key),
                );

                // Emit inactive event
                emit_plug_status(
                    app_handle,
                    instance_key.clone(),
                    plug_name.to_string(),
                    PlugScope::Each,
                    Some(slot_id.clone()),
                    PlugStage::Teardown,
                    PlugStatusValue::Idle,
                );
            }
        }

        Ok(())
    }

    /// Destroy all-scope plugs at procedure end
    pub async fn destroy_all_scope_plugs(
        &self,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        let keys_to_remove: Vec<String> = instances
            .keys()
            .filter(|key| {
                let scope = scopes.get(*key).cloned().unwrap_or(PlugScope::Each);
                matches!(scope, PlugScope::All)
            })
            .cloned()
            .collect();

        for instance_key in keys_to_remove {
            if let Some(_instance) = instances.remove(&instance_key) {
                // Emit stopping event
                emit_plug_status(
                    app_handle,
                    instance_key.clone(),
                    instance_key.clone(),
                    PlugScope::All,
                    None,
                    PlugStage::Teardown,
                    PlugStatusValue::Destructing,
                );

                // Stop the plug service
                if let Err(e) = self
                    .plug_service_manager
                    .stop_plug_service(&instance_key)
                    .await
                {
                    crate::cli_output::warning(format!(
                        "Failed to stop plug service {}: {}",
                        instance_key, e
                    ));
                }

                crate::cli_output::print_section(
                    crate::cli_output::Section::Plugs,
                    format!("Destroyed all-slots plug {}", instance_key),
                );

                // Emit inactive event
                emit_plug_status(
                    app_handle,
                    instance_key.clone(),
                    instance_key.clone(),
                    PlugScope::All,
                    None,
                    PlugStage::Teardown,
                    PlugStatusValue::Idle,
                );
            }
        }

        Ok(())
    }

    /// Get access to the plug service manager
    pub fn get_plug_service_manager(&self) -> &Arc<PlugServiceManager> {
        &self.plug_service_manager
    }

    /// Start a manual plug (from UI debug buttons)
    pub async fn start_manual_plug(
        &self,
        plug_name: String,
        plug_config: serde_json::Value,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<u16, String> {
        let mut instances = self.plug_instances.write().await;
        let mut manual_plugs = self.manual_plugs.write().await;

        // Check if plug is already managed by orchestrator
        if instances.contains_key(&plug_name) && !manual_plugs.contains(&plug_name) {
            return Err(format!(
                "Plug '{}' is currently managed by a running procedure. Stop the procedure first.",
                plug_name
            ));
        }

        // Check if already manually started
        if manual_plugs.contains(&plug_name) {
            return Err(format!("Plug '{}' is already running manually", plug_name));
        }

        // Emit initializing event
        emit_plug_status(
            app_handle,
            plug_name.clone(),
            plug_name.clone(),
            PlugScope::All,
            None,
            PlugStage::Manual,
            PlugStatusValue::Initializing,
        );

        // Start the plug service using the same service manager
        let port = self.plug_service_manager
            .start_plug_service(plug_name.clone(), plug_name.clone(), plug_config, app_handle)
            .await?;

        // Track in the same instances map
        instances.insert(
            plug_name.clone(),
            PlugInstance {
                port,
                slot_id: Some("manual".to_string()), // Mark as manual
                ref_count: 0, // Manual plugs don't use ref counting
            },
        );

        // Mark as manually started
        manual_plugs.insert(plug_name.clone());

        crate::cli_output::debug(format!(
            "Started manual plug '{}' on port {}",
            plug_name, port
        ));

        // Emit ready event
        emit_plug_status(
            app_handle,
            plug_name.clone(),
            plug_name.clone(),
            PlugScope::All,
            None,
            PlugStage::Manual,
            PlugStatusValue::Active,
        );

        Ok(port)
    }

    /// Stop a manual plug
    pub async fn stop_manual_plug(
        &self,
        plug_name: &str,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let mut instances = self.plug_instances.write().await;
        let mut manual_plugs = self.manual_plugs.write().await;

        // Check if this is actually a manual plug
        if !manual_plugs.contains(plug_name) {
            return Err(format!(
                "Plug '{}' is not a manually-started plug",
                plug_name
            ));
        }

        // Emit stopping event
        emit_plug_status(
            app_handle,
            plug_name.to_string(),
            plug_name.to_string(),
            PlugScope::All,
            None,
            PlugStage::Manual,
            PlugStatusValue::Destructing,
        );

        // Remove from instances
        if let Some(_instance) = instances.remove(plug_name) {
            // Stop the plug service
            self.plug_service_manager
                .stop_plug_service(plug_name)
                .await?;

            crate::cli_output::debug(format!(
                "Stopped manual plug '{}'",
                plug_name
            ));
        }

        // Remove from manual tracking
        manual_plugs.remove(plug_name);

        // Emit inactive event
        emit_plug_status(
            app_handle,
            plug_name.to_string(),
            plug_name.to_string(),
            PlugScope::All,
            None,
            PlugStage::Manual,
            PlugStatusValue::Idle,
        );

        Ok(())
    }

    /// Clean up all manual plugs (call on orchestrator start)
    pub async fn teardown_manual_plugs(
        &self,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let manual_plugs: Vec<String> = {
            let plugs = self.manual_plugs.read().await;
            plugs.iter().cloned().collect()
        };

        for plug_name in manual_plugs {
            crate::cli_output::warning(format!(
                "Cleaning up manually-started plug '{}' before procedure run",
                plug_name
            ));
            let _ = self.stop_manual_plug(&plug_name, app_handle).await;
        }

        Ok(())
    }

    /// Force destroy all plugs (both each-scope and all-scope) without teardown
    /// Used during force kill operations
    pub async fn force_destroy_all_plugs(
        &self,
        app_handle: Option<&tauri::AppHandle>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        let all_keys: Vec<String> = instances.keys().cloned().collect();

        crate::cli_output::print_section(
            crate::cli_output::Section::Plugs,
            format!("Force destroying {} plug instances", all_keys.len()),
        );

        for instance_key in all_keys {
            if let Some(instance) = instances.remove(&instance_key) {
                let (plug_name, scope, slot_id) = if let Some(ref slot) = instance.slot_id {
                    let plug_name = instance_key
                        .strip_suffix(&format!("_{}", slot))
                        .unwrap_or(&instance_key);
                    let scope = scopes.get(plug_name).cloned().unwrap_or(PlugScope::Each);
                    (plug_name, scope, Some(slot.clone()))
                } else {
                    let scope = scopes.get(&instance_key).cloned().unwrap_or(PlugScope::All);
                    (instance_key.as_str(), scope, None)
                };

                if let Err(e) = self
                    .plug_service_manager
                    .force_kill_plug_service(&instance_key)
                    .await
                {
                    crate::cli_output::warning(format!(
                        "Failed to force kill plug service {}: {}",
                        instance_key, e
                    ));
                }

                crate::cli_output::print_section(
                    crate::cli_output::Section::Plugs,
                    format!("Force destroyed plug {}", instance_key),
                );

                emit_plug_status(
                    app_handle,
                    instance_key.clone(),
                    plug_name.to_string(),
                    scope,
                    slot_id,
                    PlugStage::Teardown,
                    PlugStatusValue::Skipped,
                );
            }
        }

        Ok(())
    }
}
