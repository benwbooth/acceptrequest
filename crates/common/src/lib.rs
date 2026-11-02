//! Types shared by the server, client daemon, PAM module and greeter.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
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

/// A window during which the account may not be used. `days` are the days the
/// window starts on; a window whose `end` is not after `start` runs past midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DowntimeWindow {
    pub days: Vec<Weekday>,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl DowntimeWindow {
    fn crosses_midnight(&self) -> bool {
        self.end <= self.start
    }

    /// The end of the occurrence of this window that contains `now`, if any.
    fn active_until(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let today = now.date();
        let t = now.time();
        if self.days.contains(&today.weekday()) && t >= self.start {
            return Some(if self.crosses_midnight() {
                (today + Duration::days(1)).and_time(self.end)
            } else if t < self.end {
                today.and_time(self.end)
            } else {
                return None;
            });
        }
        let yesterday = today - Duration::days(1);
        if self.crosses_midnight() && self.days.contains(&yesterday.weekday()) && t < self.end {
            return Some(today.and_time(self.end));
        }
        None
    }

    /// The next start of this window strictly after `now`, looking up to a week ahead.
    fn next_start(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        (0..=7)
            .map(|d| now.date() + Duration::days(d))
            .filter(|day| self.days.contains(&day.weekday()))
            .map(|day| day.and_time(self.start))
            .find(|start| *start > now)
    }
}

/// Effective policy for one user, as served to clients for a given day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub username: String,
    /// The local day `bonus_minutes` applies to.
    pub day: NaiveDate,
    /// `None` means no daily limit, only downtime.
    pub daily_allowance_minutes: Option<u32>,
    pub bonus_minutes: u32,
    pub downtime: Vec<DowntimeWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DenyReason {
    Downtime { until: NaiveDateTime },
    OutOfTime,
    NoPolicy,
}

impl std::fmt::Display for DenyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DenyReason::Downtime { until } => {
                write!(f, "Downtime until {}.", until.format("%a %-I:%M %p"))
            }
            DenyReason::OutOfTime => write!(f, "Screen time is used up for today."),
            DenyReason::NoPolicy => write!(f, "Screen time settings have not been received yet."),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// `remaining_minutes` is `None` when neither a limit nor an upcoming downtime applies.
    Allowed { remaining_minutes: Option<u32> },
    Denied { reason: DenyReason },
}

/// Decide whether a user may be signed in at `now`, having used `used_minutes` today.
/// A policy for another day keeps its schedule and allowance but loses its bonus.
pub fn evaluate(policy: Option<&Policy>, now: NaiveDateTime, used_minutes: u32) -> Decision {
    let Some(policy) = policy else {
        return Decision::Denied { reason: DenyReason::NoPolicy };
    };
    if let Some(until) = policy.downtime.iter().filter_map(|w| w.active_until(now)).max() {
        return Decision::Denied { reason: DenyReason::Downtime { until } };
    }

    let bonus = if policy.day == now.date() { policy.bonus_minutes } else { 0 };
    let by_allowance = policy
        .daily_allowance_minutes
        .map(|a| (a + bonus).saturating_sub(used_minutes));
    if by_allowance == Some(0) {
        return Decision::Denied { reason: DenyReason::OutOfTime };
    }

    let by_downtime = policy
        .downtime
        .iter()
        .filter_map(|w| w.next_start(now))
        .min()
        .map(|start| (start - now).num_minutes().max(0) as u32);

    let remaining_minutes = match (by_allowance, by_downtime) {
        (Some(a), Some(d)) => Some(a.min(d)),
        (a, d) => a.or(d),
    };
    Decision::Allowed { remaining_minutes }
}

/// Apply a parent's on-the-spot grant: until `override_until` the user may be signed in
/// even when out of time or in downtime.
pub fn apply_override(decision: Decision, now: NaiveDateTime, override_until: Option<NaiveDateTime>) -> Decision {
    let Some(until) = override_until.filter(|u| *u > now) else {
        return decision;
    };
    let by_override = (until - now).num_minutes().max(1) as u32;
    match decision {
        Decision::Denied { .. } => Decision::Allowed { remaining_minutes: Some(by_override) },
        Decision::Allowed { remaining_minutes } => Decision::Allowed {
            remaining_minutes: remaining_minutes.map(|r| r.max(by_override)),
        },
    }
}

/// Minutes-left thresholds at which the user sees an on-screen warning.
pub const WARNING_THRESHOLDS: [u32; 3] = [10, 5, 1];

/// A request for more time, raised from the greeter or the in-session applet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRequest {
    pub username: String,
    pub host: String,
    pub requested_minutes: Option<u32>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Granted { minutes: u32 },
    Denied,
}

/// Usage report a client sends to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    pub username: String,
    pub host: String,
    pub day: NaiveDate,
    pub active_minutes: u32,
    /// Minutes a parent granted on this machine today with "My parent is here".
    #[serde(default)]
    pub local_bonus_minutes: u32,
}

/// Path of the local socket the client daemon listens on for PAM and greeter queries.
pub const CLIENT_SOCKET: &str = "/run/acceptrequest/client.sock";

/// One JSON line sent to the client daemon socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SocketRequest {
    /// Asked by the PAM module before a session starts.
    CheckLogin { username: String },
    Status { username: String },
    RequestTime { username: String, minutes: Option<u32>, reason: Option<String> },
    /// Keep the connection open and receive `SocketResponse::Warning` lines.
    Subscribe { username: String },
    /// "My parent is here": an administrator signs in on the spot to add time.
    ParentGrant { username: String, minutes: u32, admin: String, password: String },
}

/// One JSON line sent back by the client daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum SocketResponse {
    Decision { decision: Decision, used_minutes: u32 },
    Unmanaged,
    RequestSent,
    RequestQueued,
    Warning { remaining_minutes: u32 },
    Granted { minutes: u32 },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use Weekday::*;

    fn at(date: &str, time: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    // 2026-11-02 is a Monday.
    fn policy() -> Policy {
        Policy {
            username: "kid".into(),
            day: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            daily_allowance_minutes: Some(120),
            bonus_minutes: 30,
            downtime: vec![DowntimeWindow {
                days: vec![Sun, Mon, Tue, Wed, Thu],
                start: t("21:00"),
                end: t("07:00"),
            }],
        }
    }

    #[test]
    fn no_policy_fails_closed() {
        assert_eq!(
            evaluate(None, at("2026-11-02", "12:00"), 0),
            Decision::Denied { reason: DenyReason::NoPolicy }
        );
    }

    #[test]
    fn allowance_plus_bonus() {
        let p = policy();
        assert_eq!(
            evaluate(Some(&p), at("2026-11-02", "12:00"), 100),
            Decision::Allowed { remaining_minutes: Some(50) }
        );
        assert_eq!(
            evaluate(Some(&p), at("2026-11-02", "12:00"), 150),
            Decision::Denied { reason: DenyReason::OutOfTime }
        );
    }

    #[test]
    fn bonus_expires_with_the_day() {
        let p = policy();
        assert_eq!(
            evaluate(Some(&p), at("2026-11-03", "12:00"), 100),
            Decision::Allowed { remaining_minutes: Some(20) }
        );
    }

    #[test]
    fn downtime_crossing_midnight() {
        let p = policy();
        assert_eq!(
            evaluate(Some(&p), at("2026-11-02", "22:00"), 0),
            Decision::Denied { reason: DenyReason::Downtime { until: at("2026-11-03", "07:00") } }
        );
        // Tuesday 06:00 is the tail of Monday night's window.
        assert!(matches!(
            evaluate(Some(&p), at("2026-11-03", "06:00"), 0),
            Decision::Denied { .. }
        ));
        // Saturday 06:00: Friday has no window.
        assert!(matches!(
            evaluate(Some(&p), at("2026-11-07", "06:00"), 0),
            Decision::Allowed { .. }
        ));
    }

    #[test]
    fn remaining_capped_by_next_downtime() {
        let p = policy();
        assert_eq!(
            evaluate(Some(&p), at("2026-11-02", "20:45"), 0),
            Decision::Allowed { remaining_minutes: Some(15) }
        );
    }

    #[test]
    fn unlimited_without_downtime() {
        let mut p = policy();
        p.daily_allowance_minutes = None;
        p.downtime.clear();
        assert_eq!(
            evaluate(Some(&p), at("2026-11-02", "12:00"), 999),
            Decision::Allowed { remaining_minutes: None }
        );
    }

    #[test]
    fn same_day_window() {
        let mut p = policy();
        p.downtime = vec![DowntimeWindow { days: vec![Mon], start: t("15:00"), end: t("17:00") }];
        assert!(matches!(evaluate(Some(&p), at("2026-11-02", "16:00"), 0), Decision::Denied { .. }));
        assert!(matches!(evaluate(Some(&p), at("2026-11-02", "17:00"), 0), Decision::Allowed { .. }));
    }

    #[test]
    fn parent_override_beats_downtime_and_extends_remaining() {
        let p = policy();
        let now = at("2026-11-02", "22:00");
        let denied = evaluate(Some(&p), now, 0);
        assert_eq!(
            apply_override(denied.clone(), now, Some(at("2026-11-02", "22:30"))),
            Decision::Allowed { remaining_minutes: Some(30) }
        );
        // An expired override changes nothing.
        assert_eq!(apply_override(denied.clone(), now, Some(at("2026-11-02", "21:30"))), denied);

        let now = at("2026-11-02", "20:45");
        assert_eq!(
            apply_override(evaluate(Some(&p), now, 0), now, Some(at("2026-11-02", "21:15"))),
            Decision::Allowed { remaining_minutes: Some(30) }
        );
    }

    #[test]
    fn protocol_round_trip() {
        let line = serde_json::to_string(&SocketRequest::CheckLogin { username: "kid".into() }).unwrap();
        assert_eq!(line, r#"{"op":"check_login","username":"kid"}"#);
        let resp = SocketResponse::Decision {
            decision: Decision::Denied { reason: DenyReason::OutOfTime },
            used_minutes: 120,
        };
        let back: SocketResponse = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
    }
}
