//! Station-scope plug ownership.
//!
//! A [`StationPlugHost`] owns `scope: station` plug processes across run
//! boundaries. The per-run [`Orchestrator`](crate::orchestrator) borrows
//! instances from the host instead of spawning them, and never registers
//! them in its own [`PlugServiceManager`] — so every run-owned teardown
//! path (auto-teardown, shutdown sweep, `Drop`) stays oblivious to them
//! and the run-leak invariant from #1293 holds unchanged for run-owned
//! processes.
//!
//! The host's own `PlugServiceManager` drops with the host, which lives
//! for the station process — that is the intended teardown point, and
//! `Drop for PlugServiceManager` / `Drop for ChildProcess` guarantee the
//! plug interpreters die with it even on panic.
//!
//! Staleness: a held instance is keyed to the procedure context
//! (procedure dir + resolved Python) and a per-plug fingerprint of its
//! spawn identity: a SHA-256 of its file, class, `with:` as resolved for
//! the run (config and secrets expanded), and the environment it is spawned
//! with (the procedure's `env:` resolved, and the run's credentials). A
//! procedure switch changes the dir, which swaps the whole context; a plug
//! definition edit, a changed config value, a rotated secret or a station re-login
//! changes the fingerprint, which respawns that plug only. Reuse
//! additionally requires a live `GetStatus` probe.
//!
//! A plug that stops being declared — renamed key, deleted entry — is
//! addressed by [`StationPlugHost::release_absent`], which every run
//! calls at initialization. Without it nothing would ever reach that
//! instance again: the fingerprint check is per-key, so the old key
//! simply stops being asked for and its interpreter keeps the
//! instrument open for the station's lifetime — while the new key
//! spawns a second instance onto a device the first still holds.
//!
//! Deployment updates are INVISIBLE to those checks: the bundle is
//! swapped in place (same directory, new contents — see the CLI's
//! `pull::sync`), and the fingerprint does not hash the plug's Python
//! code. The station loop therefore calls [`StationPlugHost::shutdown`]
//! right after applying a staged swap, so the next run respawns every
//! plug from the new bundle instead of reusing instances built from
//! code that no longer exists on disk.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::event_sink::EventSink;
use crate::events::{PlugScope, PlugStage, PlugStatusValue};
use crate::plugs::plug_service::{probe_plug_health, PlugServiceManager};

/// Spawn identity of a station plug instance. Two definitions with equal
/// fingerprints (within one procedure context) may share a live process;
/// anything else forces a respawn. A hash, not the config itself: the
/// resolved `with:` and the environment hold secrets, which the host must
/// not keep around in clear between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlugFingerprint(String);

impl PlugFingerprint {
    /// SHA-256 of the plug's resolved spawn config (`{file, class, config}`
    /// as `PlugDefinition::to_resolved_config_json` builds it) and the
    /// environment it is spawned with, order-independent. The JSON is
    /// serialized with sorted keys by construction (`serde_json::Value`
    /// maps are BTree-backed), so equal configs yield equal hashes.
    /// `init_timeout_ms` is left out: how long an open may take is no
    /// reason to reopen an instrument that is open already.
    pub fn new(config_json: &serde_json::Value, env: &[(String, String)]) -> Self {
        use sha2::{Digest, Sha256};
        let env: std::collections::BTreeMap<&str, &str> =
            env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut spawn = config_json.clone();
        if let Some(config) = spawn.as_object_mut() {
            config.remove("init_timeout_ms");
        }
        let identity = serde_json::json!({ "spawn": spawn, "env": env });
        let digest = Sha256::digest(identity.to_string().as_bytes());
        Self(digest.iter().map(|b| format!("{b:02x}")).collect())
    }
}

#[derive(Debug)]
struct StationPlugEntry {
    port: u16,
    fingerprint: PlugFingerprint,
    display_name: String,
}

/// The procedure context a set of held plugs belongs to. Any change
/// swaps the context: a different procedure lives in a different
/// directory with its own venv, and plug code loaded from the old one
/// must not be reused. The environment (credentials, the procedure's
/// `env:`) is per plug, in its fingerprint: a station re-login or a
/// changed value respawns the plugs it reaches, not the whole set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostContext {
    procedure_dir: PathBuf,
    python_path: Option<PathBuf>,
}

#[derive(Default)]
struct HostInner {
    context: Option<HostContext>,
    manager: Option<Arc<PlugServiceManager>>,
    live: HashMap<String, StationPlugEntry>,
}

/// Event sink handed to station plug processes. A plug's stderr reader
/// lives as long as the process, i.e. the station, so it must not hold
/// the sink of the run that happened to spawn it: that run's sink owns
/// its event broadcast, which then never closes, and its publisher drain
/// waits out its full cap after every such run. Later runs' plug lines
/// also landed on that dead broadcast. This forwards to whichever run
/// borrowed the plugs last, through a `Weak` so it pins nothing. Between
/// runs a plug's stderr still reaches the CLI log (`start_plug_service`
/// logs every line); only the operator surfaces, which have no run to
/// show it under, lose it.
struct HostSink {
    current: std::sync::RwLock<std::sync::Weak<dyn EventSink>>,
}

impl HostSink {
    fn new() -> Self {
        Self {
            current: std::sync::RwLock::new(std::sync::Weak::<crate::NullSink>::new()),
        }
    }

    fn route_to(&self, sink: &Arc<dyn EventSink>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(sink);
    }
}

impl EventSink for HostSink {
    fn emit(&self, event: &crate::event_sink::ExecutionEvent) {
        let target = self
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade();
        if let Some(sink) = target {
            sink.emit(event);
        }
    }
}

/// Owns station-scope plug processes across runs. Create one per
/// station process (the CLI station loop), share it with every run via
/// `Orchestrator::with_station_plug_host`.
pub struct StationPlugHost {
    inner: Mutex<HostInner>,
    sink: Arc<HostSink>,
}

impl Default for StationPlugHost {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            sink: Arc::new(HostSink::new()),
        }
    }
}

impl std::fmt::Debug for StationPlugHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StationPlugHost").finish_non_exhaustive()
    }
}

impl StationPlugHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow a live instance for `plug_key`, spawning or respawning as
    /// needed. Returns the loopback port phases connect to.
    ///
    /// Reuse requires all three: same procedure context, same
    /// fingerprint, passing health probe. A context change tears down
    /// every held plug first (the old procedure's code and venv must
    /// not leak into the new one); a fingerprint change or failed probe
    /// respawns that plug only.
    ///
    /// One run at a time: the host routes every held plug's stderr to
    /// the sink of the latest `acquire`, so two runs borrowing plugs
    /// concurrently would see each other's plug lines. The CLI station
    /// loop never overlaps two runs' engines (the next run starts after
    /// the previous engine returned); a future concurrent caller needs
    /// per-plug routing first.
    #[allow(clippy::too_many_arguments)]
    pub async fn acquire(
        &self,
        procedure_dir: &std::path::Path,
        python_path: &Option<PathBuf>,
        procedure_env: &[(String, String)],
        plug_key: &str,
        display_name: &str,
        config_json: serde_json::Value,
        event_sink: &Arc<dyn EventSink>,
    ) -> Result<u16, String> {
        // Reused plugs' stderr lines follow the run that borrows them.
        self.sink.route_to(event_sink);
        let plug_sink: Arc<dyn EventSink> = self.sink.clone();

        let mut inner = self.inner.lock().await;

        let context = HostContext {
            procedure_dir: procedure_dir.to_path_buf(),
            python_path: python_path.clone(),
        };

        if inner.context.as_ref() != Some(&context) {
            if let Some(manager) = &inner.manager {
                if !inner.live.is_empty() {
                    log::info!(
                        "Station plug context changed (procedure switch); \
                         releasing {} held plug(s)",
                        inner.live.len()
                    );
                }
                // Graceful stop; a partially-failed stop is still safe
                // because stop_plug_service removes the entry BEFORE
                // shutting down, so the ChildProcess drops locally and
                // its Drop SIGKILLs the process group. That Drop is the
                // real backstop here — the manager replaced below drops
                // with an already-empty map.
                manager.stop_all_services().await.ok();
            }
            inner.live.clear();
            let manager = PlugServiceManager::new_with_python(
                procedure_dir.to_path_buf(),
                python_path.clone(),
            );
            inner.manager = Some(Arc::new(manager));
            inner.context = Some(context);
        }

        let manager = Arc::clone(
            inner
                .manager
                .as_ref()
                .expect("manager set with context above"),
        );
        // The run's environment for whatever this acquire spawns. Every
        // plug of one run shares it, and runs never overlap (see above).
        manager.set_procedure_env(procedure_env.to_vec());

        let fingerprint = PlugFingerprint::new(&config_json, procedure_env);

        if let Some(entry) = inner.live.get(plug_key) {
            if entry.fingerprint == fingerprint {
                match probe_plug_health(entry.port).await {
                    Ok(()) => {
                        log::info!("Reusing station plug '{}' on port {}", plug_key, entry.port);
                        return Ok(entry.port);
                    }
                    Err(e) => {
                        log::warn!("Station plug {plug_key} unhealthy: {e}; restarting it");
                    }
                }
            } else {
                log::info!(
                    "Station plug '{}' definition, values or environment changed; respawning",
                    plug_key
                );
            }

            // Stale or dead — release before respawn. A dead process
            // makes the graceful path fail fast; fall back to kill.
            if manager.stop_plug_service(plug_key).await.is_err() {
                manager.force_kill_plug_service(plug_key).await.ok();
            }
            inner.live.remove(plug_key);
        }

        crate::plugs::manager::emit_plug_status(
            event_sink,
            plug_key.to_string(),
            display_name.to_string(),
            PlugScope::Station,
            None,
            PlugStage::Setup,
            PlugStatusValue::Initializing,
        );

        // Spawn without the host lock: a run borrowing several station
        // plugs acquires them concurrently, and holding the lock here
        // would serialise their spawns (200-400ms each) again. The
        // bookkeeping above decided reuse-or-respawn under the lock;
        // the insert below re-takes it and checks the host is still the
        // one it decided against.
        drop(inner);

        // A start cancelled between the service registering and the
        // host recording it (a force kill racing this acquire, see
        // below) leaves the manager holding a key the host does not.
        // Start over rather than fail on "already running".
        if manager.is_service_alive(plug_key).await.is_some()
            && manager.stop_plug_service(plug_key).await.is_err()
        {
            manager.force_kill_plug_service(plug_key).await.ok();
        }

        let port = match manager
            .start_plug_service(
                plug_key.to_string(),
                plug_key.to_string(),
                display_name.to_string(),
                config_json,
                None,
                &plug_sink,
            )
            .await
        {
            Ok(port) => port,
            Err(e) => {
                crate::plugs::manager::emit_plug_status(
                    event_sink,
                    plug_key.to_string(),
                    display_name.to_string(),
                    PlugScope::Station,
                    None,
                    PlugStage::Setup,
                    PlugStatusValue::Error,
                );
                return Err(e);
            }
        };

        // The service is registered in `manager` from here on; this
        // await is the only point where dropping the future strands it
        // (next acquire of the key: "already running"). The scheduling
        // loop does drop this future on a force kill; the stranded
        // service is stopped by the check above on the next acquire.
        let mut inner = self.inner.lock().await;
        let same_host = inner
            .manager
            .as_ref()
            .is_some_and(|m| Arc::ptr_eq(m, &manager));
        if !same_host {
            // The host was shut down or switched context while this
            // plug was starting; its manager already stopped everything
            // it knew about, so stop this straggler ourselves — without
            // the lock, a graceful stop can take seconds and the other
            // acquires of this run are queued on it.
            drop(inner);
            if manager.stop_plug_service(plug_key).await.is_err() {
                manager.force_kill_plug_service(plug_key).await.ok();
            }
            crate::plugs::manager::emit_plug_status(
                event_sink,
                plug_key.to_string(),
                display_name.to_string(),
                PlugScope::Station,
                None,
                PlugStage::Setup,
                PlugStatusValue::Error,
            );
            return Err(format!(
                "Plug {}: station plug host reset during start",
                plug_key
            ));
        }

        inner.live.insert(
            plug_key.to_string(),
            StationPlugEntry {
                port,
                fingerprint,
                display_name: display_name.to_string(),
            },
        );

        log::info!("Created station plug '{}' on port {}", plug_key, port);

        crate::plugs::manager::emit_plug_status(
            event_sink,
            plug_key.to_string(),
            display_name.to_string(),
            PlugScope::Station,
            None,
            PlugStage::Setup,
            PlugStatusValue::Active,
        );

        Ok(port)
    }

    /// Release every held plug the current procedure no longer declares.
    ///
    /// Called once per run at initialization, with the keys of every
    /// `scope: station` plug in the procedure about to execute. A key
    /// absent from that set can never be acquired again, so holding its
    /// instrument is pure loss — and actively harmful when the plug was
    /// renamed, because the new key needs the device the old instance
    /// still has open.
    ///
    /// A context mismatch is a no-op on purpose: the next `acquire`
    /// releases every held plug when it swaps the context, and pruning
    /// against another procedure's plug list here would tear down
    /// instances that procedure is still using.
    pub async fn release_absent(
        &self,
        procedure_dir: &PathBuf,
        python_path: &Option<PathBuf>,
        declared: &HashSet<String>,
        event_sink: &Arc<dyn EventSink>,
    ) {
        let mut inner = self.inner.lock().await;

        let same_procedure = inner
            .context
            .as_ref()
            .is_some_and(|c| c.procedure_dir == *procedure_dir && c.python_path == *python_path);
        if !same_procedure {
            return;
        }

        let absent: Vec<String> = inner
            .live
            .keys()
            .filter(|key| !declared.contains(*key))
            .cloned()
            .collect();
        if absent.is_empty() {
            return;
        }

        let manager = Arc::clone(
            inner
                .manager
                .as_ref()
                .expect("manager is set whenever a context is"),
        );

        for key in absent {
            let Some(entry) = inner.live.remove(&key) else {
                continue;
            };
            log::info!("Station plug '{}' is no longer declared; releasing it", key);
            crate::plugs::manager::emit_plug_status(
                event_sink,
                key.clone(),
                entry.display_name.clone(),
                PlugScope::Station,
                None,
                PlugStage::Teardown,
                PlugStatusValue::Destructing,
            );
            // Same fallback as a stale respawn: a wedged process makes
            // the graceful path fail, and the kill is what guarantees
            // the device is actually free for the run that follows.
            if manager.stop_plug_service(&key).await.is_err() {
                manager.force_kill_plug_service(&key).await.ok();
            }
            crate::plugs::manager::emit_plug_status(
                event_sink,
                key.clone(),
                entry.display_name,
                PlugScope::Station,
                None,
                PlugStage::Teardown,
                PlugStatusValue::Idle,
            );
        }
    }

    /// Gracefully release every held plug. Call at station shutdown; the
    /// `Drop` chain covers abnormal exits.
    pub async fn shutdown(&self, event_sink: Option<&Arc<dyn EventSink>>) {
        let mut inner = self.inner.lock().await;
        if let Some(manager) = &inner.manager {
            if let Some(sink) = event_sink {
                for (key, entry) in &inner.live {
                    crate::plugs::manager::emit_plug_status(
                        sink,
                        key.clone(),
                        entry.display_name.clone(),
                        PlugScope::Station,
                        None,
                        PlugStage::Teardown,
                        PlugStatusValue::Destructing,
                    );
                }
            }
            manager.stop_all_services().await.ok();
        }
        inner.live.clear();
        inner.context = None;
        inner.manager = None;
    }

    /// Number of currently held plug instances (test/diagnostic aid).
    pub async fn held_count(&self) -> usize {
        self.inner.lock().await.live.len()
    }
}
