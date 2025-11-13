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
//! - [`runs`]: Test run reporting and artifact management
//!
//! # Lock Ordering (Critical for Deadlock Prevention)
//!
//! Always acquire locks in this order:
//! 1. `OrchestratorState` (outermost)
//! 2. `ResourceManager`
//! 3. Individual `Worker` (innermost)
//!
//! Never acquire locks in reverse order.

pub mod cli_output;
pub mod constants;
pub mod job;
pub mod log;
pub mod orchestrator;
pub mod process_group;
pub mod runs;
pub mod resource_tracker;
pub mod state;
pub mod ui_types;
pub mod worker;
pub mod worker_ipc;
pub mod worker_process;
pub mod worker_protocol;
pub mod worker_state;
pub mod worker_types;

pub use log::LogEntry;
