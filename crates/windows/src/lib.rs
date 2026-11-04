//! Windows client for screentime. The enforcement logic is the shared agent from
//! `screentime-client`; this crate supplies the Windows platform pieces.

pub mod logon_hours;
#[cfg(windows)]
pub mod win;
