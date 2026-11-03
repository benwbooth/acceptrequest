//! A Credential Provider that adds a "Screen time" tile to the Windows sign-in and lock screens.
//! It never signs anyone in: it shows where each limited account stands, lets the kid ask for
//! more time, and lets a parent who is there add time with an administrator password. All of it
//! goes through the service's named pipe.
//!
//! This DLL runs inside LogonUI, so it must never panic or block: every entry point catches
//! panics, and pipe calls run on a worker thread with a timeout.

#[cfg(windows)]
mod provider;

#[cfg(windows)]
pub use provider::{DllCanUnloadNow, DllGetClassObject};

/// The provider's class ID, registered by install.ps1.
pub const CLSID_STR: &str = "{6f6b7a8e-2d1c-4b8a-9a53-7c0e2f4a91d3}";
