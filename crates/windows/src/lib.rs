//! Windows client for acceptrequest. The enforcement logic is the shared agent from
//! `acceptrequest-client`; this crate supplies the Windows platform pieces.

pub mod logon_hours;
#[cfg(windows)]
pub mod win;
