//! Client side of screentime: the platform-independent agent plus the Linux pieces.
//! The Windows service lives in the `screentime-windows` crate and reuses the agent.

pub mod agent;
pub mod config;
#[cfg(target_os = "linux")]
pub mod logind;
#[cfg(target_os = "linux")]
pub mod parent;
pub mod pinning;
pub mod server;
pub mod state;
