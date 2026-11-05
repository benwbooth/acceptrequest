//! Persistent daemon state: cached policies, today's usage and unsent time requests.

use screentime_common::{Decision, Policy, TimeRequest, apply_override, evaluate};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub day: NaiveDate,
    pub minutes: u32,
}

/// Time a parent granted on this machine with "My parent is here".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGrant {
    pub day: NaiveDate,
    /// Added to the day's allowance like a bonus from the server.
    pub bonus_minutes: u32,
    /// Lets the user stay on through downtime until this time.
    pub override_until: Option<NaiveDateTime>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub policies: HashMap<String, Policy>,
    pub usage: HashMap<String, Usage>,
    pub pending_requests: Vec<TimeRequest>,
    #[serde(default)]
    pub local_grants: HashMap<String, LocalGrant>,
    /// The latest trusted time seen. Time never runs backwards past it, so setting the clock
    /// back while offline doesn't reopen used-up time or downtime.
    /// Which local accounts are limited as which kid, as last received from the server.
    #[serde(default)]
    pub account_map: Option<Vec<screentime_common::AccountMapping>>,
    /// Hash of the parent PIN set in the web UI; "My parent is here" accepts it.
    #[serde(default)]
    pub parent_pin_hash: Option<String>,
    /// The administrator who last approved time here, offered first next time.
    #[serde(default)]
    pub last_admin: Option<String>,
    #[serde(default)]
    pub trusted_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl State {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("state.json")
    }

    /// Load saved state. A missing file is a fresh install; a corrupt one is an error
    /// rather than silently resetting today's usage to zero.
    pub fn load(state_dir: &Path) -> Result<State> {
        let path = Self::path(state_dir);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let path = Self::path(state_dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn used_minutes(&self, user: &str, today: NaiveDate) -> u32 {
        match self.usage.get(user) {
            Some(u) if u.day == today => u.minutes,
            _ => 0,
        }
    }

    pub fn add_minute(&mut self, user: &str, today: NaiveDate) {
        let used = self.used_minutes(user, today);
        self.usage.insert(user.to_owned(), Usage { day: today, minutes: used + 1 });
    }

    pub fn local_bonus(&self, user: &str, today: NaiveDate) -> u32 {
        match self.local_grants.get(user) {
            Some(g) if g.day == today => g.bonus_minutes,
            _ => 0,
        }
    }

    /// Record a parent's grant of `minutes` more time, starting at `now`.
    pub fn add_local_grant(&mut self, user: &str, now: NaiveDateTime, minutes: u32) {
        let today = now.date();
        let previous = self.local_grants.get(user).filter(|g| g.day == today);
        // Grants stack: a second grant while the first is running extends it.
        let start = previous.and_then(|g| g.override_until).filter(|u| *u > now).unwrap_or(now);
        self.local_grants.insert(
            user.to_owned(),
            LocalGrant {
                day: today,
                bonus_minutes: previous.map_or(0, |g| g.bonus_minutes) + minutes,
                override_until: Some(start + Duration::minutes(minutes.into())),
            },
        );
    }

    /// The user's decision at `now`, with any local grant folded in, and minutes used today.
    pub fn decide(&self, user: &str, now: NaiveDateTime) -> (Decision, u32) {
        let today = now.date();
        let used = self.used_minutes(user, today);
        let local = self.local_bonus(user, today);
        let policy = match (self.policies.get(user).cloned(), local) {
            (policy, 0) => policy,
            (Some(mut p), local) => {
                if p.day != today {
                    p.day = today;
                    p.bonus_minutes = 0;
                }
                p.bonus_minutes += local;
                Some(p)
            }
            // A parent can grant time before the machine has ever heard from the server.
            (None, local) => Some(Policy {
                username: user.to_owned(),
                day: today,
                daily_allowance_minutes: Some(0),
                bonus_minutes: local,
                downtime: Vec::new(),
                locked: false,
            }),
        };
        let override_until = self.local_grants.get(user).and_then(|g| g.override_until);
        (apply_override(evaluate(policy.as_ref(), now, used), now, override_until), used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_resets_each_day_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mon = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let tue = mon.succ_opt().unwrap();

        let mut s = State::load(dir.path()).unwrap();
        s.add_minute("kid", mon);
        s.add_minute("kid", mon);
        s.save(dir.path()).unwrap();

        let mut s = State::load(dir.path()).unwrap();
        assert_eq!(s.used_minutes("kid", mon), 2);
        assert_eq!(s.used_minutes("kid", tue), 0);
        s.add_minute("kid", tue);
        assert_eq!(s.used_minutes("kid", tue), 1);
    }

    #[test]
    fn parent_grant_without_policy_and_through_downtime() {
        let now = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap().and_hms_opt(22, 0, 0).unwrap();
        let mut s = State::default();
        assert!(matches!(s.decide("kid", now).0, Decision::Denied { .. }));

        s.add_local_grant("kid", now, 30);
        assert_eq!(s.decide("kid", now).0, Decision::Allowed { remaining_minutes: Some(30) });

        s.add_local_grant("kid", now, 15);
        assert_eq!(s.local_bonus("kid", now.date()), 45);
        assert_eq!(s.decide("kid", now).0, Decision::Allowed { remaining_minutes: Some(45) });
        assert_eq!(s.local_grants["kid"].override_until, Some(now + Duration::minutes(45)));

        let later = now + Duration::minutes(50);
        for _ in 0..50 {
            s.add_minute("kid", now.date());
        }
        assert!(matches!(s.decide("kid", later).0, Decision::Denied { .. }));
    }

    #[test]
    fn corrupt_state_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(State::path(dir.path()), b"{not json").unwrap();
        assert!(State::load(dir.path()).is_err());
    }
}
