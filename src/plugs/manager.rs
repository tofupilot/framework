use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::event_sink::{EventSink, ExecutionEvent};
use crate::events::{PlugScope, PlugStage, PlugStatusUpdateEvent, PlugStatusValue};
use crate::plugs::plug_service::PlugServiceManager;

pub(crate) fn emit_plug_status(
    event_sink: &Arc<dyn EventSink>,
    plug_key: String,
    plug_name: String,
    scope: PlugScope,
    slot_id: Option<String>,
    stage: PlugStage,
    status: PlugStatusValue,
) {
    let event = PlugStatusUpdateEvent {
        plug_key: plug_key.clone(),
        plug_name: plug_name.clone(),
        scope,
        slot_id,
        stage,
        status,
    };

    log::debug!("PLUG [BACKEND] Emitting plug-status-update: {:?}", event);
    event_sink.emit(&ExecutionEvent::PlugStatus(event));
}

/// A live plug service, as addressed by the instance map. Its map key is
/// `plug_key` for a shared instance (execution, station, manual) and
/// `plug_key_<slot>` for a per-slot one — which is why identity is
/// carried in the struct rather than recovered from that key: stripping
/// a `_<slot>` suffix off a string is guesswork, and it put the INSTANCE
/// key on the wire as `plug_key` on the force-destroy path (a slot
/// plug's teardown event then named a plug no consumer had heard of).
///
/// `display_name` is here for the same reason: the teardown paths had no
/// access to the procedure's names and emitted the key as the name, so a
/// plug renamed itself from "Power Supply" to "power_supply" halfway
/// through a run.
#[derive(Debug, Clone)]
pub struct PlugInstance {
    pub port: u16,
    /// The plug's own key — never the instance key.
    pub plug_key: String,
    /// The `name:` from the procedure. Falls back to the key for a
    /// manual plug, which has no procedure entry.
    pub display_name: String,
    /// The slot this instance belongs to; `None` for shared instances.
    pub slot_id: Option<String>,
}

#[derive(Debug)]
pub struct ResourceManager {
    plug_service_manager: Arc<PlugServiceManager>,
    // Track plug instances by key and optionally slot
    plug_instances: Arc<RwLock<HashMap<String, PlugInstance>>>, // "plug_key" or "plug_key_slot1"
    plug_scopes: Arc<RwLock<HashMap<String, PlugScope>>>,       // plug_key -> scope
    manual_plugs: Arc<RwLock<HashSet<String>>>, // Track manually-started plugs (debug mode)
    /// Why an instance never opened ("plug_key" or "plug_key_slot1"):
    /// its own error, or a sibling's that dropped it. Named to a phase
    /// that asks for it (`resolve_plug_ports`).
    not_opened: Arc<RwLock<HashMap<String, String>>>,
    /// The plugs a batch is opening right now. A stop drops the batch
    /// mid-way (`create_plugs` never reaches its status loop): what is
    /// left here is then told `skipped` by `abandon_opening`.
    opening: Arc<std::sync::Mutex<Vec<Opening>>>,
}

/// A plug of a batch still opening (see `ResourceManager::opening`).
#[derive(Debug)]
struct Opening {
    instance_key: String,
    plug_name: String,
    display_name: String,
    scope: PlugScope,
    slot_id: Option<String>,
    /// The port its service serves on once `__init__` returned, before
    /// the batch was dropped: it is open, and is kept (see
    /// `abandon_opening`).
    opened: Option<u16>,
}

/// How long a force kill lets each plug run its cleanup (`__del__`)
/// before killing its process: past the 3 s the plug service waits for a
/// method still running (`tp_plug.py` `_cleanup`), so a call that returns
/// in that window still lets `__del__` finish.
const FORCE_CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// How long the other plugs of a batch may still finish opening once one
/// failed (see `create_plugs`).
const SIBLING_FAILURE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl ResourceManager {
    pub fn new(procedure_dir: PathBuf) -> Self {
        Self::new_with_python(procedure_dir, None)
    }

    pub fn new_with_python(procedure_dir: PathBuf, python_path: Option<PathBuf>) -> Self {
        Self {
            plug_service_manager: Arc::new(PlugServiceManager::new_with_python(
                procedure_dir,
                python_path,
            )),
            plug_instances: Arc::new(RwLock::new(HashMap::new())),
            plug_scopes: Arc::new(RwLock::new(HashMap::new())),
            manual_plugs: Arc::new(RwLock::new(HashSet::new())),
            not_opened: Arc::new(RwLock::new(HashMap::new())),
            opening: Default::default(),
        }
    }

    /// The procedure's `dashboard:` credentials for the plug processes
    /// this run spawns.
    pub fn set_procedure_env(&self, env: crate::procedure_env::ProcedureEnv) {
        self.plug_service_manager.set_procedure_env(env);
    }

    pub async fn set_plug_scopes(&self, scopes: HashMap<String, PlugScope>) {
        // Store the scopes for all plugs
        let mut plug_scopes = self.plug_scopes.write().await;
        *plug_scopes = scopes;
    }

    /// Resolve the port of every plug a job needs, in a specific slot.
    ///
    /// Named "start" for history: the plugs are already running (a scope
    /// boundary created them), so this only addresses them. The returned
    /// map is what the caller hands the worker.
    pub async fn start_plug_services_for_slot(
        &self,
        plug_configs: &HashMap<String, serde_json::Value>,
        slot_id: Option<String>,
    ) -> Result<HashMap<String, u16>, String> {
        let (ports, mut unavailable, _) = self.resolve_plug_ports(plug_configs, slot_id).await;
        let mut keys: Vec<&String> = unavailable.keys().collect();
        keys.sort();
        match keys.first().cloned().cloned() {
            Some(first) => Err(unavailable.remove(&first).unwrap_or_default()),
            None => Ok(ports),
        }
    }

    /// The port of every plug in `plug_configs` this job can reach, and why
    /// each of the others cannot be reached. A job lists every plug of the
    /// procedure; its Python signature decides which ones it takes, so an
    /// unreachable plug is only an error for a phase that asks for it.
    pub async fn resolve_plug_ports(
        &self,
        plug_configs: &HashMap<String, serde_json::Value>,
        slot_id: Option<String>,
    ) -> (
        HashMap<String, u16>,
        HashMap<String, String>,
        HashSet<String>,
    ) {
        let mut plug_ports = HashMap::new();
        let mut unavailable = HashMap::new();
        // Of those, the ones that failed to open or never got to (a
        // stop, a sibling's failure), as opposed to out of reach.
        let mut not_opened = HashSet::new();
        let scopes = self.plug_scopes.read().await;
        // Read, not write: resolving a port mutates nothing now that the
        // reference count is gone, so concurrent jobs no longer serialize
        // on this lock to look up a number.
        let instances = self.plug_instances.read().await;

        // Start or reuse plug services based on scope
        for plug_name in plug_configs.keys() {
            let scope = scopes.get(plug_name).cloned().unwrap_or(PlugScope::Slot);

            // Determine the instance key based on scope. Run and station
            // scopes share one instance across slots, so the bare plug
            // key addresses it.
            let instance_key = match scope {
                PlugScope::Slot => match &slot_id {
                    Some(slot) => format!("{}_{}", plug_name, slot),
                    // A slot plug's instance is keyed by its slot, so
                    // there is nothing to address for a job that has no
                    // slot (an execution-wide setup/teardown phase whose
                    // Python names a slot-scope plug). This errored before
                    // too — the key was built from the job id, which no
                    // create path ever writes — but said the plug was
                    // missing rather than that it cannot be reached from
                    // here. Falling back to the bare key would be worse
                    // than either: that addresses a SHARED instance.
                    None => {
                        unavailable.insert(
                            plug_name.clone(),
                            "slot-scoped, phase is shared; set plug `scope: execution` or \
                             phase `scope: slot`"
                                .to_string(),
                        );
                        continue;
                    }
                },
                PlugScope::Execution | PlugScope::Station => plug_name.clone(),
            };

            // Plug should already exist at this point — a scope boundary
            // created it. Resolving its port is all a phase needs.
            if let Some(instance) = instances.get(&instance_key) {
                plug_ports.insert(plug_name.clone(), instance.port);
                log::debug!("Phase using plug {}", instance_key);
                // No event needed - plug is already ready
            } else {
                let why = self.not_opened.read().await.get(&instance_key).cloned();
                not_opened.insert(plug_name.clone());
                unavailable.insert(
                    plug_name.clone(),
                    why.unwrap_or_else(|| "the run ended before it was opened".to_string()),
                );
                continue;
            }
        }

        (plug_ports, unavailable, not_opened)
    }

    // A job finishing needs no counterpart to `start_plug_services_*`: a
    // plug outlives every phase that uses it and is destroyed at its
    // scope boundary. The `stop_plug_services*` pair that used to sit
    // here only moved `PlugInstance::ref_count`, which nothing ever
    // read — so per job it took two locks, walked the allocation and
    // rebuilt every instance key to reach a counter that decided
    // nothing. It also underflowed on the partial-start path (an early
    // `Err` leaves increments behind and never records the ports the
    // decrement walks). Destruction lives in `destroy_*_scope_plugs`.

    /// Register a station-owned plug instance so phases can allocate it
    /// like any other. The service process belongs to the
    /// `StationPlugHost` — it is deliberately NOT in this manager's
    /// `PlugServiceManager`, so no run-owned teardown path (auto-
    /// teardown, shutdown sweep, `Drop`) can reach it.
    pub async fn register_station_plug(&self, plug_key: String, display_name: String, port: u16) {
        let mut instances = self.plug_instances.write().await;
        instances.insert(
            plug_key.clone(),
            PlugInstance {
                port,
                plug_key,
                display_name,
                slot_id: None, // shared across slots, like execution scope
            },
        );
    }

    /// Start the execution-scoped plugs that are not running yet, all at
    /// once. Each plug is its own Python process; starting them one after
    /// another cost their spawn times summed (200-400ms each) on every
    /// run. The declaration map is a HashMap, so no procedure could rely
    /// on a start order. The first failure is returned after every
    /// spawn has settled; the ones that came up are registered so the
    /// run's teardown reaches them.
    pub async fn create_procedure_plugs(
        &self,
        plug_configs: &HashMap<String, serde_json::Value>,
        plug_display_names: &HashMap<String, String>,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        self.create_plugs(
            PlugScope::Execution,
            None,
            plug_configs,
            plug_display_names,
            event_sink,
        )
        .await
    }

    /// `create_procedure_plugs` for slot-scoped plugs, several slots'
    /// batches (slot, its plugs' configs) together: a fixture of N nests
    /// opens in the time of its slowest plug, not N times it.
    pub async fn create_slot_plugs(
        &self,
        batches: Vec<(String, HashMap<String, serde_json::Value>)>,
        plug_display_names: &HashMap<String, String>,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        let batches: Vec<_> = batches
            .into_iter()
            .map(|(slot, configs)| (Some(slot), configs))
            .collect();
        self.create_plugs_in(PlugScope::Slot, &batches, plug_display_names, event_sink)
            .await
    }

    async fn create_plugs(
        &self,
        wanted_scope: PlugScope,
        slot_id: Option<String>,
        plug_configs: &HashMap<String, serde_json::Value>,
        plug_display_names: &HashMap<String, String>,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        self.create_plugs_in(
            wanted_scope,
            &[(slot_id, plug_configs.clone())],
            plug_display_names,
            event_sink,
        )
        .await
    }

    async fn create_plugs_in(
        &self,
        wanted_scope: PlugScope,
        batches: &[(Option<String>, HashMap<String, serde_json::Value>)],
        plug_display_names: &HashMap<String, String>,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        // Extract (or verify) the embedded plug script once, before the
        // concurrent starts: each start would otherwise race the same
        // per-process temp file on the first run after an upgrade.
        PlugServiceManager::find_plug_script_cli()?;

        struct Pending {
            plug_name: String,
            instance_key: String,
            display_name: String,
            config: serde_json::Value,
            slot_id: Option<String>,
        }
        let mut pending = Vec::new();
        for (slot_id, plug_config) in batches
            .iter()
            .flat_map(|(slot, configs)| configs.iter().map(move |c| (slot, c)))
        {
            let (plug_name, plug_config) = plug_config;
            let scope = scopes.get(plug_name).cloned().unwrap_or(PlugScope::Slot);
            if scope != wanted_scope {
                continue;
            }
            let instance_key = match &slot_id {
                Some(slot) => format!("{}_{}", plug_name, slot),
                None => plug_name.clone(),
            };
            if instances.contains_key(&instance_key) {
                continue;
            }
            let display_name = plug_display_names
                .get(plug_name)
                .cloned()
                .unwrap_or_else(|| plug_name.clone());
            emit_plug_status(
                event_sink,
                plug_name.clone(),
                display_name.clone(),
                wanted_scope,
                slot_id.clone(),
                PlugStage::Setup,
                PlugStatusValue::Initializing,
            );
            if let Ok(mut opening) = self.opening.lock() {
                opening.push(Opening {
                    instance_key: instance_key.clone(),
                    plug_name: plug_name.clone(),
                    display_name: display_name.clone(),
                    scope: wanted_scope,
                    slot_id: slot_id.clone(),
                    opened: None,
                });
            }
            pending.push(Pending {
                plug_name: plug_name.clone(),
                instance_key,
                display_name,
                config: plug_config.clone(),
                slot_id: slot_id.clone(),
            });
        }

        // All start together. Once one fails the run cannot start, so the
        // others get `SIBLING_FAILURE_GRACE` to report how they went (two
        // broken instruments both read as broken) and the slow ones are
        // dropped then, which kills their half-started interpreter: a run
        // no longer waits out a 60s `init_timeout` it will not use.
        use futures::stream::StreamExt;
        let mut starts: futures::stream::FuturesUnordered<_> = pending
            .iter()
            .enumerate()
            .map(|(i, p)| async move {
                let result = self
                    .plug_service_manager
                    .start_plug_service(
                        p.instance_key.clone(),
                        p.plug_name.clone(),
                        p.display_name.clone(),
                        p.config.clone(),
                        p.slot_id.clone(),
                        event_sink,
                    )
                    .await;
                (i, result)
            })
            .collect();
        let mut results: Vec<Option<Result<u16, String>>> = pending.iter().map(|_| None).collect();
        let mut give_up_at: Option<tokio::time::Instant> = None;
        loop {
            let next = match give_up_at {
                Some(at) => match tokio::time::timeout_at(at, starts.next()).await {
                    Ok(next) => next,
                    Err(_) => break,
                },
                None => starts.next().await,
            };
            let Some((i, result)) = next else { break };
            if result.is_err() && give_up_at.is_none() {
                give_up_at = Some(tokio::time::Instant::now() + SIBLING_FAILURE_GRACE);
            }
            if let Ok(port) = &result {
                if let Ok(mut opening) = self.opening.lock() {
                    if let Some(o) = opening
                        .iter_mut()
                        .find(|o| o.instance_key == pending[i].instance_key)
                    {
                        o.opened = Some(*port);
                    }
                }
            }
            results[i] = Some(result);
        }
        drop(starts);
        if let Ok(mut opening) = self.opening.lock() {
            opening.retain(|o| !pending.iter().any(|p| p.instance_key == o.instance_key));
        }

        let mut first_err = None;
        let failed_first = pending
            .iter()
            .zip(&results)
            .find(|(_, r)| matches!(r, Some(Err(_))))
            .map(|(p, _)| p.plug_name.clone());
        let mut not_opened = self.not_opened.write().await;
        for (p, result) in pending.into_iter().zip(results) {
            let Some(result) = result else {
                // Dropped after a sibling failed: never opened.
                not_opened.insert(
                    p.instance_key.clone(),
                    // Only a failure leaves a result unset here.
                    format!(
                        "dropped when plug {} failed to open",
                        failed_first.as_deref().unwrap_or("another")
                    ),
                );
                emit_plug_status(
                    event_sink,
                    p.plug_name,
                    p.display_name,
                    wanted_scope,
                    p.slot_id.clone(),
                    PlugStage::Setup,
                    PlugStatusValue::Skipped,
                );
                continue;
            };
            match result {
                Ok(port) => {
                    not_opened.remove(&p.instance_key);
                    instances.insert(
                        p.instance_key.clone(),
                        PlugInstance {
                            port,
                            plug_key: p.plug_name.clone(),
                            display_name: p.display_name.clone(),
                            slot_id: p.slot_id.clone(),
                        },
                    );
                    log::info!("Created plug {} on port {}", p.instance_key, port);
                    emit_plug_status(
                        event_sink,
                        p.plug_name,
                        p.display_name,
                        wanted_scope,
                        p.slot_id.clone(),
                        PlugStage::Setup,
                        PlugStatusValue::Active,
                    );
                }
                Err(e) => {
                    // The service's error names the plug already.
                    let why = e
                        .strip_prefix(&format!("Plug {}: ", p.plug_name))
                        .unwrap_or(&e);
                    let why = why
                        .strip_prefix("Plug initialization failed: ")
                        .unwrap_or(why)
                        .to_string();
                    not_opened.insert(p.instance_key.clone(), why);
                    emit_plug_status(
                        event_sink,
                        p.plug_name,
                        p.display_name,
                        wanted_scope,
                        p.slot_id.clone(),
                        PlugStage::Setup,
                        PlugStatusValue::Error,
                    );
                    // Several slots opened together: say which one.
                    let e = match &p.slot_id {
                        Some(slot) if batches.len() > 1 => format!("slot {slot}: {e}"),
                        _ => e,
                    };
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The batch that was opening was dropped by a stop. A plug whose
    /// `__init__` had returned is open: it is registered like any open
    /// plug (`active`), so the teardown phases that take it run and the
    /// run's release closes it. The others never opened: `skipped`, with
    /// why, for a phase that asks for one.
    pub async fn abandon_opening(&self, event_sink: &Arc<dyn EventSink>) {
        let abandoned: Vec<Opening> = match self.opening.lock() {
            Ok(mut opening) => std::mem::take(&mut *opening),
            Err(_) => return,
        };
        let mut instances = self.plug_instances.write().await;
        let mut not_opened = self.not_opened.write().await;
        for o in abandoned {
            let status = match o.opened {
                Some(port) => {
                    not_opened.remove(&o.instance_key);
                    instances.insert(
                        o.instance_key,
                        PlugInstance {
                            port,
                            plug_key: o.plug_name.clone(),
                            display_name: o.display_name.clone(),
                            slot_id: o.slot_id.clone(),
                        },
                    );
                    PlugStatusValue::Active
                }
                None => {
                    not_opened.insert(
                        o.instance_key,
                        "the run was stopped while it opened".to_string(),
                    );
                    PlugStatusValue::Skipped
                }
            };
            emit_plug_status(
                event_sink,
                o.plug_name,
                o.display_name,
                o.scope,
                o.slot_id,
                PlugStage::Setup,
                status,
            );
        }
    }

    /// Whether `slot_id` has slot-scope plugs open.
    pub async fn has_slot_scope_plugs(&self, slot_id: &str) -> bool {
        let scopes = self.plug_scopes.read().await;
        let instances = self.plug_instances.read().await;

        instances
            .values()
            .any(|instance| self.is_slot_instance(instance, slot_id, &scopes))
    }

    /// Whether `instance` is a slot-scope instance belonging to `slot_id`.
    /// Both the "is there anything to tear down" question and the
    /// teardown itself must answer this identically, so they share it.
    fn is_slot_instance(
        &self,
        instance: &PlugInstance,
        slot_id: &str,
        scopes: &HashMap<String, PlugScope>,
    ) -> bool {
        instance.slot_id.as_deref() == Some(slot_id)
            && matches!(
                scopes
                    .get(&instance.plug_key)
                    .cloned()
                    .unwrap_or(PlugScope::Slot),
                PlugScope::Slot
            )
    }

    /// Whether execution-scope plugs are open.
    pub async fn has_execution_scope_plugs(&self) -> bool {
        let scopes = self.plug_scopes.read().await;
        let instances = self.plug_instances.read().await;

        instances.values().any(|instance| {
            let scope = scopes
                .get(&instance.plug_key)
                .cloned()
                .unwrap_or(PlugScope::Slot);
            matches!(scope, PlugScope::Execution) && instance.slot_id.is_none()
        })
    }

    /// Close the slot-scope plugs of a slot, at its end.
    pub async fn destroy_slot_scope_plugs(
        &self,
        slot_id: String,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        let keys_to_remove: Vec<String> = instances
            .iter()
            .filter(|(_, instance)| self.is_slot_instance(instance, &slot_id, &scopes))
            .map(|(key, _)| key.clone())
            .collect();

        for instance_key in keys_to_remove {
            if let Some(instance) = instances.remove(&instance_key) {
                let plug_key = instance.plug_key.as_str();
                let display_name = instance.display_name.as_str();

                // Emit stopping event
                emit_plug_status(
                    event_sink,
                    plug_key.to_string(),
                    display_name.to_string(),
                    PlugScope::Slot,
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
                    log::warn!("Cannot stop plug {instance_key}: {e}");
                }

                log::info!("Destroyed slot-level plug {}", instance_key);

                // Emit inactive event
                emit_plug_status(
                    event_sink,
                    plug_key.to_string(),
                    display_name.to_string(),
                    PlugScope::Slot,
                    Some(slot_id.clone()),
                    PlugStage::Teardown,
                    PlugStatusValue::Idle,
                );
            }
        }

        Ok(())
    }

    /// Close the execution-scope plugs, at the procedure end.
    pub async fn destroy_execution_scope_plugs(
        &self,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;

        let keys_to_remove: Vec<String> = instances
            .iter()
            .filter(|(_, instance)| {
                let scope = scopes
                    .get(&instance.plug_key)
                    .cloned()
                    .unwrap_or(PlugScope::Slot);
                matches!(scope, PlugScope::Execution) && instance.slot_id.is_none()
            })
            .map(|(key, _)| key.clone())
            .collect();

        for instance_key in keys_to_remove {
            if let Some(instance) = instances.remove(&instance_key) {
                let plug_key = instance.plug_key.clone();
                let display_name = instance.display_name.clone();

                // Emit stopping event
                emit_plug_status(
                    event_sink,
                    plug_key.clone(),
                    display_name.clone(),
                    PlugScope::Execution,
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
                    log::warn!("Cannot stop plug {instance_key}: {e}");
                }

                log::info!("Destroyed shared plug {}", instance_key);

                // Emit inactive event
                emit_plug_status(
                    event_sink,
                    plug_key.clone(),
                    display_name.clone(),
                    PlugScope::Execution,
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
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<u16, String> {
        // The maps are held only to check and reserve the key. The
        // spawn below waits on the plug's `__init__` — a slow port can
        // sit there for the whole 30s readiness window — and holding
        // the instance lock across it made every other manual plug's
        // start and stop queue behind this one (TP-1189). The entry in
        // `manual_plugs` without a matching instance is what "starting"
        // looks like to the rest of this manager.
        {
            let instances = self.plug_instances.read().await;
            let mut manual_plugs = self.manual_plugs.write().await;

            // Check if plug is already managed by orchestrator
            if instances.contains_key(&plug_name) && !manual_plugs.contains(&plug_name) {
                return Err(format!(
                    "Plug {}: in use by a running procedure; stop it first",
                    plug_name
                ));
            }

            // Check if already manually started, or still starting
            if !manual_plugs.insert(plug_name.clone()) {
                return Err(format!("Plug {}: already running", plug_name));
            }
        }

        // Emit initializing event
        emit_plug_status(
            event_sink,
            plug_name.clone(),
            plug_name.clone(),
            PlugScope::Execution,
            None,
            PlugStage::Manual,
            PlugStatusValue::Initializing,
        );

        // Start the plug service using the same service manager.
        // For manual plugs, use the plug name as display name (user hasn't set a custom name).
        // `slot_id = None` here preserves the prior behavior: the
        // suffix-strip resolution this replaced returned `None` for
        // manual plugs (instance_key == plug_key, no `_<slot>`
        // suffix), so PlugLogEvent shape stays identical for
        // downstream consumers. The `PlugInstance.slot_id = "manual"`
        // tag below is internal bookkeeping in the manager's instance
        // map, not part of the wire event.
        let started = self
            .plug_service_manager
            .start_plug_service(
                plug_name.clone(),
                plug_name.clone(),
                plug_name.clone(),
                plug_config,
                None,
                event_sink,
            )
            .await;
        let port = match started {
            Ok(port) => port,
            Err(e) => {
                // Give the key back: nothing is running under it, and
                // the next Connect must not read "already running".
                self.manual_plugs.write().await.remove(&plug_name);
                return Err(e);
            }
        };

        // Track in the same instances map. The reservation taken above
        // is what makes this insert safe without re-checking: no other
        // start could have claimed the key in between.
        let mut instances = self.plug_instances.write().await;
        instances.insert(
            plug_name.clone(),
            PlugInstance {
                port,
                plug_key: plug_name.clone(),
                // A manual plug is started from the UI, not from a
                // procedure entry, so the key is the only name there is.
                display_name: plug_name.clone(),
                slot_id: Some("manual".to_string()), // Mark as manual
            },
        );
        drop(instances);

        log::debug!("Started manual plug '{}' on port {}", plug_name, port);

        // Emit ready event
        emit_plug_status(
            event_sink,
            plug_name.clone(),
            plug_name.clone(),
            PlugScope::Execution,
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
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        // Drop the bookkeeping under the locks, then run the shutdown
        // ladder — Cleanup RPC, SIGTERM, SIGKILL, up to seconds — with
        // nothing held, so another plug's start or stop never queues
        // behind this one's finalizer.
        {
            let mut instances = self.plug_instances.write().await;
            let mut manual_plugs = self.manual_plugs.write().await;

            // Check if this is actually a manual plug
            if !manual_plugs.contains(plug_name) {
                return Err(format!("Plug {}: not started manually", plug_name));
            }

            // Reserved by a start that has not registered its service
            // yet. There is no process to stop and nothing to hand
            // back; stopping it means letting the start finish first.
            if instances.remove(plug_name).is_none() {
                return Err(format!("Plug {}: still starting", plug_name));
            }

            manual_plugs.remove(plug_name);
        }

        // Emit stopping event
        emit_plug_status(
            event_sink,
            plug_name.to_string(),
            plug_name.to_string(),
            PlugScope::Execution,
            None,
            PlugStage::Manual,
            PlugStatusValue::Destructing,
        );

        let stopped = self.plug_service_manager.stop_plug_service(plug_name).await;
        if stopped.is_ok() {
            log::debug!("Stopped manual plug '{}'", plug_name);
        }

        // Emit inactive event. The maps no longer know this plug either
        // way: a service that refused a graceful stop was force-killed
        // inside the ladder, so idle is the truth even when `stopped`
        // carries an error.
        emit_plug_status(
            event_sink,
            plug_name.to_string(),
            plug_name.to_string(),
            PlugScope::Execution,
            None,
            PlugStage::Manual,
            PlugStatusValue::Idle,
        );

        stopped
    }

    /// Clean up all manual plugs (call on orchestrator start)
    pub async fn teardown_manual_plugs(
        &self,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        let manual_plugs: Vec<String> = {
            let plugs = self.manual_plugs.read().await;
            plugs.iter().cloned().collect()
        };

        let mut failures = Vec::new();
        for plug_name in manual_plugs {
            log::info!("Stopping manually started plug {plug_name}");
            if let Err(e) = self.stop_manual_plug(&plug_name, event_sink).await {
                // `e` names the plug already.
                failures.push(e);
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// `force_destroy_all_plugs` with no cleanup at all: a shutdown with
    /// seconds left (a console closing) cannot wait for `__del__`.
    pub async fn force_destroy_all_plugs_now(
        &self,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        self.force_destroy_plugs(event_sink, std::time::Duration::ZERO)
            .await
    }

    /// Force destroy all plugs (slot and execution scope) without teardown
    /// Used during force kill operations
    pub async fn force_destroy_all_plugs(
        &self,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<(), String> {
        self.force_destroy_plugs(event_sink, FORCE_CLEANUP_GRACE)
            .await
    }

    async fn force_destroy_plugs(
        &self,
        event_sink: &Arc<dyn EventSink>,
        cleanup_grace: std::time::Duration,
    ) -> Result<(), String> {
        let scopes = self.plug_scopes.read().await;
        let mut instances = self.plug_instances.write().await;
        // Same lock order as start/stop_manual_plug. A manual plug left
        // in this set with its instance gone reads as "still starting"
        // to a stop and "already running" to a start, forever.
        let mut manual_plugs = self.manual_plugs.write().await;

        let all_keys: Vec<String> = instances.keys().cloned().collect();

        log::info!("Force destroying {} plug instances", all_keys.len());

        // The phases are abandoned, not the instruments: each plug's
        // `__del__` (output off) gets a short, concurrent chance before
        // its process is killed. A wedged one costs the grace, no more.
        let cleaned: HashSet<String> = futures::future::join_all(
            all_keys
                .iter()
                .filter(|_| !cleanup_grace.is_zero())
                .filter(|k| !matches!(scopes.get(*k), Some(PlugScope::Station)))
                .map(|k| async move {
                    self.plug_service_manager
                        .try_cleanup(k, cleanup_grace)
                        .await
                        .then(|| k.clone())
                }),
        )
        .await
        .into_iter()
        .flatten()
        .collect();

        for instance_key in all_keys {
            // Station plugs are host-owned: their process is not in this
            // manager, must survive a run kill, and emitting a teardown
            // event for a plug that stays alive would lie to the
            // operator UI. Keep the instance entry too — the plug is
            // still live, so any phase dispatched after the force-kill
            // can still resolve its port. (Station instance keys are
            // always the bare plug key.)
            if matches!(scopes.get(&instance_key), Some(PlugScope::Station)) {
                continue;
            }

            if let Some(instance) = instances.remove(&instance_key) {
                manual_plugs.remove(&instance_key);
                let scope = scopes.get(&instance.plug_key).cloned().unwrap_or(
                    // An instance with no slot is a shared one, so
                    // execution is the right default for it; a per-slot
                    // instance defaults to slot.
                    if instance.slot_id.is_some() {
                        PlugScope::Slot
                    } else {
                        PlugScope::Execution
                    },
                );
                let slot_id = instance.slot_id.clone();

                if let Err(e) = self
                    .plug_service_manager
                    .force_kill_plug_service(&instance_key)
                    .await
                {
                    log::debug!("Failed to force kill plug service {}: {}", instance_key, e);
                }

                log::info!("Force destroyed plug {}", instance_key);

                // The plug's key, NOT the instance key: a slot plug's
                // instance key (`dmm_slot_1`) matched no `plug_key` any
                // consumer had been told about at `RunStarted`. `idle`
                // (after `destructing`, as a normal close) when its
                // cleanup ran, `skipped` when it was killed without it.
                if cleaned.contains(&instance_key) {
                    emit_plug_status(
                        event_sink,
                        instance.plug_key.clone(),
                        instance.display_name.clone(),
                        scope,
                        slot_id.clone(),
                        PlugStage::Teardown,
                        PlugStatusValue::Destructing,
                    );
                }
                emit_plug_status(
                    event_sink,
                    instance.plug_key.clone(),
                    instance.display_name.clone(),
                    scope,
                    slot_id,
                    PlugStage::Teardown,
                    if cleaned.contains(&instance_key) {
                        PlugStatusValue::Idle
                    } else {
                        PlugStatusValue::Skipped
                    },
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NullSink;

    fn sink() -> Arc<dyn EventSink> {
        Arc::new(NullSink)
    }

    /// Keeps every `plug_status` event so a test can read what teardown
    /// actually put on the wire.
    #[derive(Default)]
    struct RecordingSink(std::sync::Mutex<Vec<PlugStatusUpdateEvent>>);

    impl EventSink for RecordingSink {
        fn emit(&self, event: &ExecutionEvent) {
            if let ExecutionEvent::PlugStatus(e) = event {
                self.0.lock().unwrap().push(e.clone());
            }
        }
    }

    /// A slot plug's teardown must name the plug the way the procedure
    /// does, under the plug's own key — not `power_supply` under
    /// `power_supply_slot_1`, which is what the instance map is keyed by.
    #[tokio::test]
    async fn slot_teardown_events_carry_key_and_display_name() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.set_plug_scopes(HashMap::from([(
            "power_supply".to_string(),
            PlugScope::Slot,
        )]))
        .await;
        rm.plug_instances.write().await.insert(
            "power_supply_slot_1".to_string(),
            PlugInstance {
                port: 45010,
                plug_key: "power_supply".to_string(),
                display_name: "Power Supply".to_string(),
                slot_id: Some("slot_1".to_string()),
            },
        );

        let recorder: Arc<RecordingSink> = Arc::new(RecordingSink::default());
        let sink: Arc<dyn EventSink> = recorder.clone();
        assert!(rm.has_slot_scope_plugs("slot_1").await);
        rm.destroy_slot_scope_plugs("slot_1".to_string(), &sink)
            .await
            .unwrap();

        let events = recorder.0.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "destructing then idle");
        for event in &events {
            assert_eq!(event.plug_key, "power_supply");
            assert_eq!(event.plug_name, "Power Supply");
            assert_eq!(event.slot_id.as_deref(), Some("slot_1"));
        }
        assert!(rm.plug_instances.read().await.is_empty());
    }

    /// Same for the force-kill path, which used to put the INSTANCE key
    /// on the wire as `plug_key`.
    #[tokio::test]
    async fn force_destroy_events_carry_the_plug_key() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.set_plug_scopes(HashMap::from([("dmm".to_string(), PlugScope::Slot)]))
            .await;
        rm.plug_instances.write().await.insert(
            "dmm_slot_2".to_string(),
            PlugInstance {
                port: 45011,
                plug_key: "dmm".to_string(),
                display_name: "Multimeter".to_string(),
                slot_id: Some("slot_2".to_string()),
            },
        );

        let recorder: Arc<RecordingSink> = Arc::new(RecordingSink::default());
        let sink: Arc<dyn EventSink> = recorder.clone();
        rm.force_destroy_all_plugs(&sink).await.unwrap();

        let events = recorder.0.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].plug_key, "dmm");
        assert_eq!(events[0].plug_name, "Multimeter");
        assert_eq!(events[0].slot_id.as_deref(), Some("slot_2"));
    }

    /// A force-kill destroys a manual plug's service: the key must be
    /// free again, for a new start and against a stop that would read
    /// it as still starting.
    #[tokio::test]
    async fn force_destroy_frees_a_manual_plug() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.manual_plugs.write().await.insert("camera".to_string());
        rm.plug_instances.write().await.insert(
            "camera".to_string(),
            PlugInstance {
                port: 45021,
                plug_key: "camera".to_string(),
                display_name: "camera".to_string(),
                slot_id: Some("manual".to_string()),
            },
        );

        rm.force_destroy_all_plugs(&sink()).await.unwrap();

        assert!(rm.manual_plugs.read().await.is_empty());
        assert_eq!(
            rm.stop_manual_plug("camera", &sink()).await,
            Err("Plug camera: not started manually".to_string())
        );
    }

    /// A manual plug whose start has reserved its key but not yet
    /// registered a service is "starting": a stop must neither claim to
    /// have stopped it nor take the reservation away from the start
    /// that holds it.
    #[tokio::test]
    async fn stopping_a_starting_manual_plug_refuses_and_keeps_the_reservation() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.manual_plugs.write().await.insert("camera".to_string());

        let recorder: Arc<RecordingSink> = Arc::new(RecordingSink::default());
        let sink: Arc<dyn EventSink> = recorder.clone();
        let result = rm.stop_manual_plug("camera", &sink).await;

        assert_eq!(result, Err("Plug camera: still starting".to_string()));
        assert!(
            recorder.0.lock().unwrap().is_empty(),
            "no status event for a refusal"
        );
        assert!(rm.manual_plugs.read().await.contains("camera"));
        // The same start, seen from another start: the key is taken.
        let again = rm
            .start_manual_plug("camera".to_string(), serde_json::json!({}), &sink)
            .await;
        assert_eq!(again, Err("Plug camera: already running".to_string()));
    }

    /// Stopping a registered manual plug drops it from both maps BEFORE
    /// the shutdown ladder runs, and the wire still reads destructing
    /// then idle — here with no live process behind the entry, so the
    /// ladder itself reports the service missing.
    #[tokio::test]
    async fn stopping_a_manual_plug_clears_the_maps_and_rings_idle() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.manual_plugs.write().await.insert("camera".to_string());
        rm.plug_instances.write().await.insert(
            "camera".to_string(),
            PlugInstance {
                port: 45020,
                plug_key: "camera".to_string(),
                display_name: "camera".to_string(),
                slot_id: Some("manual".to_string()),
            },
        );

        let recorder: Arc<RecordingSink> = Arc::new(RecordingSink::default());
        let sink: Arc<dyn EventSink> = recorder.clone();
        let result = rm.stop_manual_plug("camera", &sink).await;

        assert!(
            result.is_err(),
            "no service was ever spawned for this entry"
        );
        assert!(rm.plug_instances.read().await.is_empty());
        assert!(rm.manual_plugs.read().await.is_empty());
        let events = recorder.0.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "destructing then idle");
        assert!(matches!(events[0].status, PlugStatusValue::Destructing));
        assert!(matches!(events[1].status, PlugStatusValue::Idle));
        // And nothing is left under the key for a second stop to find.
        assert_eq!(
            rm.stop_manual_plug("camera", &sink).await,
            Err("Plug camera: not started manually".to_string())
        );
    }

    async fn manager_with_station_plug() -> ResourceManager {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.set_plug_scopes(HashMap::from([("psu".to_string(), PlugScope::Station)]))
            .await;
        rm.register_station_plug("psu".to_string(), "PSU".to_string(), 45001)
            .await;
        rm
    }

    #[tokio::test]
    async fn station_plug_resolves_by_bare_key_across_slots() {
        let rm = manager_with_station_plug().await;

        let configs = HashMap::from([("psu".to_string(), serde_json::json!({}))]);

        // Two phases in different slots share the single station instance.
        let ports_a = rm
            .start_plug_services_for_slot(&configs, Some("slot_1".into()))
            .await
            .unwrap();
        let ports_b = rm
            .start_plug_services_for_slot(&configs, Some("slot_2".into()))
            .await
            .unwrap();
        assert_eq!(ports_a.get("psu"), Some(&45001));
        assert_eq!(ports_b.get("psu"), Some(&45001));
    }

    #[tokio::test]
    async fn station_plug_survives_run_owned_teardown() {
        let rm = manager_with_station_plug().await;

        // Neither run-owned bucket claims the station instance...
        assert!(!rm.has_execution_scope_plugs().await);
        assert!(!rm.has_slot_scope_plugs("slot_1").await);

        // ...and neither destroy path touches it.
        rm.destroy_slot_scope_plugs("slot_1".to_string(), &sink())
            .await
            .unwrap();
        rm.destroy_execution_scope_plugs(&sink()).await.unwrap();

        let configs = HashMap::from([("psu".to_string(), serde_json::json!({}))]);
        let ports = rm
            .start_plug_services_for_slot(&configs, Some("slot_1".into()))
            .await
            .expect("station instance must still be allocatable after run teardown");
        assert_eq!(ports.get("psu"), Some(&45001));
    }

    #[tokio::test]
    async fn force_destroy_spares_station_plugs_and_kills_run_plugs() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.set_plug_scopes(HashMap::from([
            ("psu".to_string(), PlugScope::Station),
            ("dmm".to_string(), PlugScope::Execution),
        ]))
        .await;
        rm.register_station_plug("psu".to_string(), "PSU".to_string(), 45001)
            .await;
        // Fake an execution-scope instance directly (no live process needed for
        // map semantics; force_kill on the absent service just warns).
        rm.plug_instances.write().await.insert(
            "dmm".to_string(),
            PlugInstance {
                port: 45002,
                plug_key: "dmm".to_string(),
                display_name: "Multimeter".to_string(),
                slot_id: None,
            },
        );

        rm.force_destroy_all_plugs(&sink()).await.unwrap();

        // The run plug is evicted; the station plug must still resolve —
        // its process belongs to the station host and survives run kill.
        let station_cfg = HashMap::from([("psu".to_string(), serde_json::json!({}))]);
        let ports = rm
            .start_plug_services_for_slot(&station_cfg, None)
            .await
            .expect("station plug must survive force destroy");
        assert_eq!(ports.get("psu"), Some(&45001));

        let run_cfg = HashMap::from([("dmm".to_string(), serde_json::json!({}))]);
        assert!(
            rm.start_plug_services_for_slot(&run_cfg, None)
                .await
                .is_err(),
            "run plug must be gone after force destroy"
        );
    }

    #[tokio::test]
    async fn missing_plug_instance_errors() {
        let rm = ResourceManager::new(std::env::temp_dir());
        rm.set_plug_scopes(HashMap::from([("psu".to_string(), PlugScope::Execution)]))
            .await;
        let configs = HashMap::from([("psu".to_string(), serde_json::json!({}))]);
        assert!(rm
            .start_plug_services_for_slot(&configs, None)
            .await
            .is_err());
    }
}
