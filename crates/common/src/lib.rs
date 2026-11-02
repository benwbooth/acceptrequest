//! Types shared by the server, client daemon, PAM module and greeter.

use chrono::{NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

/// Bonus time choices offered in the UI and in phone notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BonusIncrement {
    Zero,
    Fifteen,
    Thirty,
    FortyFive,
    Sixty,
    Custom(u32),
}

impl BonusIncrement {
    pub const PRESETS: [BonusIncrement; 5] = [
        BonusIncrement::Zero,
        BonusIncrement::Fifteen,
        BonusIncrement::Thirty,
        BonusIncrement::FortyFive,
        BonusIncrement::Sixty,
    ];

    pub fn minutes(self) -> u32 {
        match self {
            BonusIncrement::Zero => 0,
            BonusIncrement::Fifteen => 15,
            BonusIncrement::Thirty => 30,
            BonusIncrement::FortyFive => 45,
            BonusIncrement::Sixty => 60,
            BonusIncrement::Custom(m) => m,
        }
    }
}

/// A window during which the account may not be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DowntimeWindow {
    pub days: Vec<Weekday>,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

/// Effective policy for one user on one machine, as pushed to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub username: String,
    pub daily_allowance_minutes: u32,
    pub bonus_minutes_today: u32,
    pub downtime: Vec<DowntimeWindow>,
}

/// A request for more time, raised from the greeter or the in-session applet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRequest {
    pub username: String,
    pub host: String,
    pub requested_minutes: Option<u32>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestStatus {
    Pending,
    Granted { minutes: u32 },
    Denied,
}

/// Path of the local socket the client daemon listens on for PAM and greeter queries.
pub const CLIENT_SOCKET: &str = "/run/acceptrequest/client.sock";
