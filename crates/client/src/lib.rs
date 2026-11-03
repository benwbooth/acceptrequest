//! Client side of acceptrequest: the platform-independent agent plus the Linux pieces.
//! The Windows service lives in the `acceptrequest-windows` crate and reuses the agent.

pub mod agent;
pub mod config;
#[cfg(target_os = "linux")]
pub mod logind;
#[cfg(target_os = "linux")]
pub mod parent;
pub mod pinning;
pub mod server;
pub mod state;
