//! Persistent daemon state: cached policies, today's usage and unsent time requests.

use acceptrequest_common::{Policy, TimeRequest};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub day: NaiveDate,
    pub minutes: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub policies: HashMap<String, Policy>,
    pub usage: HashMap<String, Usage>,
    pub pending_requests: Vec<TimeRequest>,
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
    fn corrupt_state_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(State::path(dir.path()), b"{not json").unwrap();
        assert!(State::load(dir.path()).is_err());
    }
}
