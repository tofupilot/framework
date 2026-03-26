//! Test execution engine with parallel orchestration and worker pool.
//!
//! Coordinates execution of test phases across multiple slots with resource
//! management, retry logic, and comprehensive reporting.
//!
//! # Architecture
//!
//! - [`orchestrator`]: Main coordinator for job scheduling and completion
//! - [`worker`]: Worker threads that execute individual test phases
//! - [`job`]: Job definitions, status tracking, and results
//! - [`state`]: Orchestrator state (job queue, completed jobs, statistics)
//! - [`reports`]: Test run reporting and artifact management
//!
//! # Lock Ordering (Critical for Deadlock Prevention)
//!
//! Always acquire locks in this order:
//! 1. `OrchestratorState` (outermost)
//! 2. `ResourceManager`
//! 3. Individual `Worker` (innermost)
//!
//! Never acquire locks in reverse order.

pub mod commands;
pub mod constants;
pub mod events;
pub mod job;
pub mod log;
pub mod monitoring;
pub mod orchestrator;
pub mod reports;
pub mod runtime;
pub mod state;
pub mod types;
pub mod worker;

use orchestrator::orchestrator::Orchestrator;
use types::PendingUnitInput;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex as TokioMutex};

/// Result of a background initialization task
pub struct InitializationResult {
    pub orchestrator: Orchestrator,
    pub python_cmd: String,
}

/// Pending initialization task for an execution.
/// The handle is wrapped in Arc<Mutex<Option<>>> so it can be taken for awaiting
/// while keeping the entry in the map. This prevents a race condition where
/// stop_execution can't find the execution during the gap between awaiting
/// the handle and storing orchestrator refs.
pub struct PendingInitialization {
    pub handle: Arc<TokioMutex<Option<tokio::task::JoinHandle<Result<InitializationResult, String>>>>>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub struct OrchestratorState {
    pub orchestrators: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Orchestrator>>>>>,
    pub state_refs:
        Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<state::OrchestratorState>>>>>,
    pub worker_refs:
        Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<Vec<worker::Worker>>>>>>,
    pub resource_manager_refs:
        Arc<TokioMutex<HashMap<String, Arc<tokio::sync::RwLock<crate::plugs::manager::ResourceManager>>>>>,
    pub pending_unit_inputs: Arc<TokioMutex<HashMap<String, HashMap<String, PendingUnitInput>>>>,
    pub pending_initializations: Arc<TokioMutex<HashMap<String, PendingInitialization>>>,
}

impl Default for OrchestratorState {
    fn default() -> Self {
        Self {
            orchestrators: Arc::new(TokioMutex::new(HashMap::new())),
            state_refs: Arc::new(TokioMutex::new(HashMap::new())),
            worker_refs: Arc::new(TokioMutex::new(HashMap::new())),
            resource_manager_refs: Arc::new(TokioMutex::new(HashMap::new())),
            pending_unit_inputs: Arc::new(TokioMutex::new(HashMap::new())),
            pending_initializations: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }
}

pub struct UIResponseState {
    pub senders:
        Arc<TokioMutex<HashMap<usize, oneshot::Sender<HashMap<String, serde_json::Value>>>>>,
    pub completed_requests: Arc<TokioMutex<HashSet<String>>>,
}

impl Default for UIResponseState {
    fn default() -> Self {
        Self {
            senders: Arc::new(TokioMutex::new(HashMap::new())),
            completed_requests: Arc::new(TokioMutex::new(HashSet::new())),
        }
    }
}

pub struct StandalonePlugServiceState {
    pub services: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<crate::plugs::plug_service::PlugService>>>>>,
}

impl Default for StandalonePlugServiceState {
    fn default() -> Self {
        Self {
            services: Arc::new(TokioMutex::new(HashMap::new())),
        }
    }
}
