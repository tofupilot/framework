//! Plug resource management and lifecycle control.
//!
//! Manages instrument/resource allocation across test phases with proper
//! setup/teardown sequencing and scope-based lifecycle management.
//!
//! # Components
//!
//! - [`manager`]: Resource pool management and allocation
//! - [`guard`]: RAII guards for automatic resource cleanup
//! - [`instance`]: Plug instance definitions and state
//! - [`plug_service`]: Python subprocess management for persistent plugs

pub mod grpc;
pub mod guard;
pub mod instance;
pub mod manager;
pub mod plug_service;
