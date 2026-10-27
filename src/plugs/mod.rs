//! Plug resource management and lifecycle control.
//!
//! Starts plug processes at their scope boundary, resolves their ports for
//! the phases that use them, and tears them down at the end of the scope.
//!
//! # Components
//!
//! - [`manager`]: plug instances by scope and slot, port resolution, teardown
//! - [`plug_service`]: Python subprocess management for persistent plugs
//! - [`station_host`]: station-process-scoped ownership of `scope: station` plugs

pub mod manager;
pub mod plug_service;
pub mod process;
pub mod station_host;
