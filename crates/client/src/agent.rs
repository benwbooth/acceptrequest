//! The platform-independent heart of the client: counts usage, decides, warns, enforces through
//! a [`Platform`], syncs with the server, and answers the local socket/pipe protocol.

use crate::config::Config;
use crate::server::ServerApi;
use crate::state::State;
use screentime_common::{
    Decision, DenyReason, SocketRequest, SocketResponse, TimeRequest, UsageReport, UserStatus, WARNING_THRESHOLDS,
};
use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, TimeDelta, Utc};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast};

/// How often sessions are checked. Usage is accumulated in seconds and stored in whole minutes.
pub const TICK: Duration = Duration::from_secs(15);
const SYNC: Duration = Duration::from_secs(60);
/// Minimum gap between two time requests from the same user.
const REQUEST_COOLDOWN: Duration = Duration::from_secs(120);
/// Upper bound on bytes read from one socket connection.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// Wrong parent passwords allowed within `PARENT_LOCKOUT` before the check is refused.
const PARENT_MAX_FAILURES: usize = 5;
const PARENT_LOCKOUT: Duration = Duration::from_secs(10 * 60);
const PARENT_FAILURE_DELAY: Duration = Duration::from_secs(3);

/// A signed-in session of some user, as the platform sees it.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub user: String,
    /// Being used right now: unlocked and in front of someone.
    pub in_use: bool,
    pub locked: bool,
}

/// What each operating system provides.
pub trait Platform: Send + Sync + 'static {
    /// Interactive sessions that belong to people (not greeters or services).
    fn sessions(&self) -> impl Future<Output = Result<Vec<Session>>> + Send;
    fn lock(&self, session: &Session) -> impl Future<Output = Result<()>> + Send;
    /// End all of `user`'s sessions.
    fn end_sessions(&self, user: &str, sessions: &[Session]) -> impl Future<Output = Result<()>> + Send;
    /// Check that `admin` may approve time and that `password` is theirs. Blocking.
    fn verify_admin(&self, admin: &str, password: &str) -> Result<(), String>;
    /// Called each tick for a session whose user is allowed to be on, e.g. to make sure the
    /// warning app is running in it. Default: nothing.
    fn on_allowed_session(&self, _session: &Session) {}
    /// Called after every tick so the platform can mirror the rules into the OS itself
    /// (Windows logon hours). `now` is the trusted current time. Default: nothing.
    fn sync_os_restrictions(&self, _user: &str, _state: &State, _now: DateTime<Utc>) {}
}

#[derive(Default)]
struct Tracking {
    /// Active seconds not yet folded into a whole minute of usage.
    seconds: u64,
    /// When the user's sessions were first found out of time, for the lock-then-end grace.
    denied_since: Option<Instant>,
    /// The smallest warning threshold already shown, reset when time is added.
    last_warning: Option<u32>,
    last_request: Option<Instant>,
    /// Whether the user was signed in while still allowed at the last tick. The grace period is
    /// only for time running out mid-session; signing in while already out of time gets none.
    in_allowed_session: bool,
    /// Set after the first enforcement since the agent started. Sessions that already exist at
    /// that point (the agent was just installed or restarted) get the grace period.
    seen: bool,
}

pub struct Agent<P: Platform> {
    pub config: Config,
    pub platform: P,
    state: Mutex<State>,
    tracking: Mutex<HashMap<String, Tracking>>,
    api: Option<ServerApi>,
    warnings: broadcast::Sender<(String, u32)>,
    /// Times of recent wrong "My parent is here" passwords, for the lockout.
    parent_failures: Mutex<Vec<Instant>>,
    /// Set once the first sync with the server has been attempted. Nothing is enforced before
    /// then, so a fresh install or restart never acts on a missing or stale schedule.
    synced_once: std::sync::atomic::AtomicBool,
    /// Ticks early when poked, e.g. by Windows the moment someone signs in.
    wake: tokio::sync::Notify,
    /// How far this machine's clock is behind the server's. Setting the PC's clock back (in
    /// the BIOS, say) doesn't buy time while the server is reachable.
    clock_offset: std::sync::Mutex<TimeDelta>,
}

/// After an out-of-time sign-in is sent back to the sign-in screen, how long until it's ended.
const FRESH_SIGN_IN_GRACE: Duration = Duration::from_secs(10);

/// Clock differences smaller than this are network delay and rounding, not tampering.
const CLOCK_TOLERANCE: TimeDelta = TimeDelta::seconds(90);

/// The smallest warning threshold that `remaining` has reached, if any.
pub fn warning_bucket(remaining: u32) -> Option<u32> {
    WARNING_THRESHOLDS.iter().copied().filter(|&t| remaining <= t).min()
}

impl<P: Platform> Agent<P> {
    pub fn new(config: Config, platform: P) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&config.state_dir)?;
        let state = State::load(&config.state_dir)?;
        let api = match &config.server {
            Some(base) => Some(ServerApi::new(
                base.clone(),
                config.token.clone(),
                config.ca_file.as_deref(),
                config.server_fingerprint,
            )?),
            None => {
                tracing::warn!("no server configured, running on cached policy only");
                None
            }
        };
        tracing::info!(users = ?config.users, host = %config.host, "screentime client starting");
        // If the clock is now earlier than the last time we trusted, it was set back.
        let offset = match state.trusted_time {
            Some(trusted) if trusted - Utc::now() > CLOCK_TOLERANCE => {
                tracing::warn!(
                    seconds = (trusted - Utc::now()).num_seconds(),
                    "this computer's clock is behind the last time seen; not letting time run backwards"
                );
                trusted - Utc::now()
            }
            _ => TimeDelta::zero(),
        };
        let config_has_no_server = api.is_none();
        Ok(Arc::new(Agent {
            config,
            platform,
            state: Mutex::new(state),
            tracking: Mutex::new(HashMap::new()),
            api,
            warnings: broadcast::channel(64).0,
            parent_failures: Mutex::new(Vec::new()),
            synced_once: std::sync::atomic::AtomicBool::new(config_has_no_server),
            wake: tokio::sync::Notify::new(),
            clock_offset: std::sync::Mutex::new(offset),
        }))
    }

    /// Check sessions now instead of at the next regular tick.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The current time, corrected by the server's clock when it disagrees with this machine's.
    pub fn now_utc(&self) -> DateTime<Utc> {
        Utc::now() + *self.clock_offset.lock().unwrap()
    }

    /// The trusted current time on the local wall clock, which schedules are written in.
    pub fn now(&self) -> NaiveDateTime {
        self.now_utc().with_timezone(&Local).naive_local()
    }

    fn note_server_time(&self, server: DateTime<Utc>) {
        let offset = server - Utc::now();
        let offset = if offset.abs() < CLOCK_TOLERANCE { TimeDelta::zero() } else { offset };
        let mut current = self.clock_offset.lock().unwrap();
        if (offset - *current).abs() >= CLOCK_TOLERANCE {
            tracing::warn!(seconds = offset.num_seconds(), "this computer's clock disagrees with the server; using the server's time");
        }
        *current = offset;
    }

    /// Tick forever and keep syncing with the server in the background.
    pub async fn run(self: Arc<Self>) {
        tokio::spawn(self.clone().sync_loop());
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last = Instant::now();
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = self.wake.notified() => {}
            }
            let elapsed = last.elapsed();
            last = Instant::now();
            if let Err(e) = self.tick(elapsed).await {
                tracing::error!("tick failed: {e:#}");
            }
        }
    }

    async fn tick(&self, elapsed: Duration) -> Result<()> {
        let now_utc = self.now_utc();
        let now = now_utc.with_timezone(&Local).naive_local();
        let today = now.date();
        let sessions = self.platform.sessions().await?;

        for user in &self.config.users {
            let mine: Vec<Session> = sessions
                .iter()
                .filter(|s| self.config.managed_name(&s.user) == Some(user.as_str()))
                .cloned()
                .collect();
            let in_use = mine.iter().any(|s| s.in_use);

            let decision = {
                let mut state = self.state.lock().await;
                let mut tracking = self.tracking.lock().await;
                let t = tracking.entry(user.clone()).or_default();
                if in_use {
                    // Cap a single step so a suspend/resume gap is not billed as use.
                    t.seconds += elapsed.min(TICK * 2).as_secs();
                    while t.seconds >= 60 {
                        t.seconds -= 60;
                        state.add_minute(user, today);
                    }
                }
                self.platform.sync_os_restrictions(user, &state, now_utc);
                state.decide(user, now).0
            };
            if self.synced_once.load(std::sync::atomic::Ordering::Relaxed) {
                self.enforce(user, &mine, decision).await;
            }
        }

        let mut state = self.state.lock().await;
        state.trusted_time = state.trusted_time.max(Some(now_utc));
        state.save(&self.config.state_dir)
    }

    async fn enforce(&self, user: &str, sessions: &[Session], decision: Decision) {
        let mut tracking = self.tracking.lock().await;
        let t = tracking.entry(user.to_owned()).or_default();
        if !t.seen {
            t.seen = true;
            t.in_allowed_session = !sessions.is_empty();
        }

        let remaining = match decision {
            Decision::Allowed { remaining_minutes } => {
                if t.denied_since.take().is_some() {
                    tracing::info!(user, "time available again, enforcement lifted");
                }
                t.in_allowed_session = !sessions.is_empty();
                for s in sessions.iter().filter(|s| s.in_use) {
                    self.platform.on_allowed_session(s);
                }
                remaining_minutes
            }
            // Without any schedule yet, new sign-ins are refused (PAM, logon hours), but nobody
            // already signed in is thrown out over a missing download.
            Decision::Denied { reason: DenyReason::NoPolicy } => return,
            Decision::Denied { reason } => {
                if sessions.is_empty() {
                    t.denied_since = None;
                    t.in_allowed_session = false;
                    return;
                }
                if !t.in_allowed_session && t.denied_since.is_none_or(|at| at.elapsed() >= FRESH_SIGN_IN_GRACE) {
                    // Signed in (or back in) while already out of time: straight back to the
                    // sign-in screen, and signed out moments later; no grace for using it.
                    tracing::info!(user, %reason, "signed in while out of time, ending sessions");
                    for s in sessions.iter().filter(|s| !s.locked) {
                        if let Err(e) = self.platform.lock(s).await {
                            tracing::warn!(user, session = %s.id, "lock failed: {e:#}");
                        }
                    }
                    if t.denied_since.is_some() {
                        if let Err(e) = self.platform.end_sessions(user, sessions).await {
                            tracing::error!(user, "ending sessions failed: {e:#}");
                        }
                        t.denied_since = None;
                    } else {
                        t.denied_since = Some(Instant::now());
                    }
                    return;
                }
                t.in_allowed_session = false;
                let since = *t.denied_since.get_or_insert_with(|| {
                    tracing::info!(user, %reason, "out of time, locking sessions");
                    Instant::now()
                });
                for s in sessions.iter().filter(|s| !s.locked) {
                    if let Err(e) = self.platform.lock(s).await {
                        tracing::warn!(user, session = %s.id, "lock failed: {e:#}");
                    }
                }
                if since.elapsed() >= self.config.grace {
                    tracing::info!(user, "grace period over, ending sessions");
                    if let Err(e) = self.platform.end_sessions(user, sessions).await {
                        tracing::error!(user, "ending sessions failed: {e:#}");
                    }
                }
                return;
            }
        };

        match remaining.and_then(warning_bucket) {
            Some(bucket) if !sessions.is_empty() => {
                if t.last_warning.is_none_or(|last| bucket < last) {
                    t.last_warning = Some(bucket);
                    let minutes = remaining.unwrap_or(bucket);
                    tracing::info!(user, minutes, "warning");
                    let _ = self.warnings.send((user.to_owned(), minutes));
                }
            }
            Some(_) => {}
            None => t.last_warning = None,
        }
    }

    async fn sync_loop(self: Arc<Self>) {
        let Some(api) = &self.api else { return };
        let mut ticker = tokio::time::interval(SYNC);
        loop {
            ticker.tick().await;
            for user in &self.config.users {
                let server_user = self.config.server_name(user);
                match api.policy(server_user).await {
                    Ok((p, server_time)) if p.username == server_user => {
                        if let Some(t) = server_time {
                            self.note_server_time(t);
                        }
                        self.state.lock().await.policies.insert(user.clone(), p);
                    }
                    Ok((p, _)) => tracing::warn!(user, got = %p.username, "server sent policy for wrong user"),
                    Err(e) => tracing::warn!(user, "policy fetch failed: {e:#}"),
                }
                let today = self.now().date();
                let report = {
                    let state = self.state.lock().await;
                    UsageReport {
                        username: server_user.to_owned(),
                        host: self.config.host.clone(),
                        day: today,
                        active_minutes: state.used_minutes(user, today),
                        local_bonus_minutes: state.local_bonus(user, today),
                    }
                };
                if let Err(e) = api.report_usage(&report).await {
                    tracing::warn!(user, "usage report failed: {e:#}");
                }
            }

            let pending = std::mem::take(&mut self.state.lock().await.pending_requests);
            let mut unsent = Vec::new();
            for req in pending {
                if let Err(e) = api.request_time(&req).await {
                    tracing::warn!(user = %req.username, "queued time request still failing: {e:#}");
                    unsent.push(req);
                }
            }
            let mut state = self.state.lock().await;
            state.pending_requests.extend(unsent);
            if let Err(e) = state.save(&self.config.state_dir) {
                tracing::error!("saving state failed: {e:#}");
            }
            drop(state);
            self.synced_once.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Serve one client of the local socket or pipe: JSON lines in, JSON lines out.
    pub async fn handle_connection<R, W>(&self, reader: R, mut writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = BufReader::new(reader.take(MAX_REQUEST_BYTES)).lines();
        while let Some(line) = lines.next_line().await? {
            let response = match serde_json::from_str::<SocketRequest>(&line) {
                Ok(SocketRequest::Subscribe { username }) => {
                    return self.stream_warnings(&username, &mut writer).await;
                }
                Ok(request) => self.answer(request).await,
                Err(e) => SocketResponse::Error { message: format!("bad request: {e}") },
            };
            write_line(&mut writer, &response).await?;
        }
        Ok(())
    }

    async fn stream_warnings<W: AsyncWrite + Unpin>(&self, username: &str, writer: &mut W) -> Result<()> {
        let Some(username) = self.config.managed_name(username) else {
            return write_line(writer, &SocketResponse::Unmanaged).await;
        };
        let mut rx = self.warnings.subscribe();
        write_line(writer, &self.status(username).await).await?;
        loop {
            match rx.recv().await {
                Ok((user, minutes)) if user == username => {
                    write_line(writer, &SocketResponse::Warning { remaining_minutes: minutes }).await?;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }

    async fn status(&self, username: &str) -> SocketResponse {
        let Some(username) = self.config.managed_name(username) else {
            return SocketResponse::Unmanaged;
        };
        let (decision, used_minutes) = self.state.lock().await.decide(username, self.now());
        SocketResponse::Decision { decision, used_minutes }
    }

    async fn answer(&self, request: SocketRequest) -> SocketResponse {
        match request {
            SocketRequest::CheckLogin { username } | SocketRequest::Status { username } => {
                self.status(&username).await
            }
            SocketRequest::RequestTime { username, minutes, reason } => {
                let Some(username) = self.config.managed_name(&username).map(str::to_owned) else {
                    return SocketResponse::Unmanaged;
                };
                {
                    let mut tracking = self.tracking.lock().await;
                    let t = tracking.entry(username.clone()).or_default();
                    if t.last_request.is_some_and(|at| at.elapsed() < REQUEST_COOLDOWN) {
                        return SocketResponse::Error {
                            message: "A request was just sent. Please wait a couple of minutes.".into(),
                        };
                    }
                    t.last_request = Some(Instant::now());
                }
                let request = TimeRequest {
                    username: self.config.server_name(&username).to_owned(),
                    host: self.config.host.clone(),
                    requested_minutes: minutes.map(|m| m.min(24 * 60)),
                    reason: reason.map(|r| r.chars().take(500).collect()),
                };
                if let Some(api) = &self.api {
                    match api.request_time(&request).await {
                        Ok(()) => return SocketResponse::RequestSent,
                        Err(e) => tracing::warn!("time request failed, queueing: {e:#}"),
                    }
                }
                self.state.lock().await.pending_requests.push(request);
                SocketResponse::RequestQueued
            }
            SocketRequest::ParentGrant { username, minutes, admin, password } => {
                self.parent_grant(username, minutes, admin, password).await
            }
            SocketRequest::ListUsers => {
                let now = self.now();
                let state = self.state.lock().await;
                let users = self
                    .config
                    .users
                    .iter()
                    .map(|u| {
                        let (decision, used_minutes) = state.decide(u, now);
                        UserStatus { username: u.clone(), decision, used_minutes }
                    })
                    .collect();
                SocketResponse::Users { users }
            }
            SocketRequest::Subscribe { .. } => unreachable!("handled by the connection loop"),
        }
    }

    async fn parent_grant(&self, username: String, minutes: u32, admin: String, password: String) -> SocketResponse {
        let error = |message: &str| SocketResponse::Error { message: message.to_owned() };
        let Some(username) = self.config.managed_name(&username).map(str::to_owned) else {
            return SocketResponse::Unmanaged;
        };
        if minutes == 0 || minutes > 24 * 60 {
            return error("Choose between 1 minute and 24 hours.");
        }
        {
            let mut failures = self.parent_failures.lock().await;
            failures.retain(|at| at.elapsed() < PARENT_LOCKOUT);
            if failures.len() >= PARENT_MAX_FAILURES {
                return error("Too many wrong passwords. Try again in a few minutes.");
            }
            // Count the attempt up front so parallel guesses can't all slip under the limit.
            failures.push(Instant::now());
        }

        // A managed account fails exactly like a wrong password.
        let verified = !self.config.is_managed(&admin)
            && tokio::task::block_in_place(|| self.platform.verify_admin(&admin, &password))
                .inspect_err(|e| tracing::warn!("parent check failed: {e}"))
                .is_ok();
        if !verified {
            tracing::warn!(%admin, user = %username, "refused \"my parent is here\" grant");
            tokio::time::sleep(PARENT_FAILURE_DELAY).await;
            return error("That name and password can't approve time.");
        }

        self.parent_failures.lock().await.clear();
        let mut state = self.state.lock().await;
        state.add_local_grant(&username, self.now(), minutes);
        if let Err(e) = state.save(&self.config.state_dir) {
            tracing::error!("saving state failed: {e:#}");
        }
        tracing::info!(%admin, user = %username, minutes, "parent granted time locally");
        SocketResponse::Granted { minutes }
    }
}

async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, response: &SocketResponse) -> Result<()> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        log: std::sync::Mutex<Vec<String>>,
    }

    impl Platform for Fake {
        async fn sessions(&self) -> Result<Vec<Session>> {
            Ok(Vec::new())
        }
        async fn lock(&self, s: &Session) -> Result<()> {
            self.log.lock().unwrap().push(format!("lock {}", s.id));
            Ok(())
        }
        async fn end_sessions(&self, user: &str, _: &[Session]) -> Result<()> {
            self.log.lock().unwrap().push(format!("end {user}"));
            Ok(())
        }
        fn verify_admin(&self, _: &str, _: &str) -> Result<(), String> {
            Err("no".into())
        }
    }

    fn agent(dir: &std::path::Path, grace: Duration) -> Arc<Agent<Fake>> {
        let config = Config {
            server: None,
            token: None,
            ca_file: None,
            server_fingerprint: None,
            users: vec!["kid".into()],
            server_names: Default::default(),
            state_dir: dir.into(),
            socket: dir.join("s"),
            grace,
            host: "test".into(),
            admin_group: "wheel".into(),
            pam_service: "x".into(),
            pam_confdir: None,
        };
        Agent::new(config, Fake::default()).unwrap()
    }

    fn session() -> Vec<Session> {
        vec![Session { id: "1".into(), user: "kid".into(), in_use: true, locked: false }]
    }

    const OUT: Decision = Decision::Denied { reason: DenyReason::OutOfTime };
    const OK: Decision = Decision::Allowed { remaining_minutes: Some(30) };

    #[tokio::test]
    async fn time_running_out_mid_session_locks_then_ends_after_grace() {
        let dir = tempfile::tempdir().unwrap();
        let a = agent(dir.path(), Duration::from_millis(50));
        a.enforce("kid", &session(), OK).await;
        a.enforce("kid", &session(), OUT).await;
        assert_eq!(*a.platform.log.lock().unwrap(), ["lock 1"]);
        tokio::time::sleep(Duration::from_millis(60)).await;
        a.enforce("kid", &session(), OUT).await;
        assert_eq!(*a.platform.log.lock().unwrap(), ["lock 1", "lock 1", "end kid"]);
    }

    #[tokio::test]
    async fn already_signed_in_when_the_agent_starts_gets_the_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let a = agent(dir.path(), Duration::from_secs(120));
        a.enforce("kid", &session(), OUT).await;
        assert_eq!(*a.platform.log.lock().unwrap(), ["lock 1"]);
    }

    #[tokio::test]
    async fn no_schedule_yet_never_ends_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let a = agent(dir.path(), Duration::from_millis(1));
        let none = Decision::Denied { reason: DenyReason::NoPolicy };
        a.enforce("kid", &session(), none.clone()).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        a.enforce("kid", &session(), none).await;
        assert!(a.platform.log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn signing_in_while_out_of_time_gets_no_grace() {
        let dir = tempfile::tempdir().unwrap();
        let a = agent(dir.path(), Duration::from_secs(120));
        a.enforce("kid", &[], OUT).await;
        a.enforce("kid", &session(), OUT).await;
        // Straight back to the sign-in screen...
        assert_eq!(*a.platform.log.lock().unwrap(), ["lock 1"]);
        // ...and ended at the next tick after a short pause, not the 2-minute grace.
        a.tracking.lock().await.get_mut("kid").unwrap().denied_since =
            Some(Instant::now() - FRESH_SIGN_IN_GRACE);
        a.enforce("kid", &session(), OUT).await;
        assert_eq!(*a.platform.log.lock().unwrap(), ["lock 1", "lock 1", "end kid"]);
    }

    #[tokio::test]
    async fn signing_back_in_after_being_ended_gets_no_grace_either() {
        let dir = tempfile::tempdir().unwrap();
        let a = agent(dir.path(), Duration::from_millis(10));
        a.enforce("kid", &session(), OK).await;
        a.enforce("kid", &session(), OUT).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        a.enforce("kid", &session(), OUT).await; // grace over: ended
        a.enforce("kid", &[], OUT).await; // gone
        a.enforce("kid", &session(), OUT).await; // back again: sent straight back
        assert_eq!(a.platform.log.lock().unwrap().last().unwrap(), "lock 1");
        assert_eq!(a.platform.log.lock().unwrap().iter().filter(|l| *l == "end kid").count(), 1);
    }

    #[test]
    fn warning_buckets() {
        assert_eq!(warning_bucket(30), None);
        assert_eq!(warning_bucket(10), Some(10));
        assert_eq!(warning_bucket(7), Some(10));
        assert_eq!(warning_bucket(5), Some(5));
        assert_eq!(warning_bucket(1), Some(1));
        assert_eq!(warning_bucket(0), Some(1));
    }
}
