mod config;
mod logind;
mod parent;
mod server;
mod state;

use acceptrequest_common::{
    Decision, SocketRequest, SocketResponse, TimeRequest, UsageReport, WARNING_THRESHOLDS,
};
use anyhow::{Context, Result};
use chrono::Local;
use config::Config;
use logind::{Logind, SessionInfo};
use server::ServerApi;
use state::State;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, broadcast};

/// How often sessions are checked. Usage is accumulated in seconds and stored in whole minutes.
const TICK: Duration = Duration::from_secs(15);
const SYNC: Duration = Duration::from_secs(60);
/// Minimum gap between two time requests from the same user.
const REQUEST_COOLDOWN: Duration = Duration::from_secs(120);
/// Upper bound on bytes read from one socket connection.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
/// Wrong parent passwords allowed within `PARENT_LOCKOUT` before the check is refused.
const PARENT_MAX_FAILURES: usize = 5;
const PARENT_LOCKOUT: Duration = Duration::from_secs(10 * 60);
const PARENT_FAILURE_DELAY: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Tracking {
    /// Active seconds not yet folded into a whole minute of usage.
    seconds: u64,
    /// When the user's sessions were first found out of time, for the lock-then-terminate grace.
    denied_since: Option<Instant>,
    /// The smallest warning threshold already shown, reset when time is added.
    last_warning: Option<u32>,
    last_request: Option<Instant>,
}

struct Daemon {
    config: Config,
    state: Mutex<State>,
    tracking: Mutex<HashMap<String, Tracking>>,
    logind: Logind,
    api: Option<ServerApi>,
    warnings: broadcast::Sender<(String, u32)>,
    /// Times of recent wrong "My parent is here" passwords, for the lockout.
    parent_failures: Mutex<Vec<Instant>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    std::fs::create_dir_all(&config.state_dir)
        .with_context(|| format!("creating {}", config.state_dir.display()))?;
    let state = State::load(&config.state_dir)?;
    let api = match &config.server {
        Some(base) => Some(ServerApi::new(base.clone(), config.token.clone(), config.ca_file.as_deref())?),
        None => {
            tracing::warn!("no ACCEPTREQUEST_SERVER set, running on cached policy only");
            None
        }
    };
    tracing::info!(users = ?config.users, host = %config.host, "acceptrequest client starting");

    let daemon = Arc::new(Daemon {
        logind: Logind::connect().await?,
        state: Mutex::new(state),
        tracking: Mutex::new(HashMap::new()),
        api,
        warnings: broadcast::channel(64).0,
        parent_failures: Mutex::new(Vec::new()),
        config,
    });

    let listener = bind_socket(&daemon.config.socket)?;
    tokio::spawn(daemon.clone().serve(listener));
    tokio::spawn(daemon.clone().sync_loop());

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = Instant::now();
    loop {
        ticker.tick().await;
        let elapsed = last.elapsed();
        last = Instant::now();
        if let Err(e) = daemon.tick(elapsed).await {
            tracing::error!("tick failed: {e:#}");
        }
    }
}

fn bind_socket(path: &std::path::Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    // PAM runs as root, but the greeter and the in-session overlay run as ordinary users.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// The smallest warning threshold that `remaining` has reached, if any.
fn warning_bucket(remaining: u32) -> Option<u32> {
    WARNING_THRESHOLDS.iter().copied().filter(|&t| remaining <= t).min()
}

impl Daemon {
    async fn tick(&self, elapsed: Duration) -> Result<()> {
        let now = Local::now().naive_local();
        let today = now.date();
        let sessions = self.logind.sessions().await?;

        for user in &self.config.users {
            let mine: Vec<&SessionInfo> =
                sessions.iter().filter(|s| &s.user == user && s.class == "user").collect();
            let in_use = mine.iter().any(|s| s.counts_as_use());

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
                state.decide(user, now).0
            };
            self.enforce(user, &mine, decision).await;
        }

        self.state.lock().await.save(&self.config.state_dir)
    }

    async fn enforce(&self, user: &str, sessions: &[&SessionInfo], decision: Decision) {
        let mut tracking = self.tracking.lock().await;
        let t = tracking.entry(user.to_owned()).or_default();

        let remaining = match decision {
            Decision::Allowed { remaining_minutes } => {
                if t.denied_since.take().is_some() {
                    tracing::info!(user, "time available again, enforcement lifted");
                }
                remaining_minutes
            }
            Decision::Denied { reason } => {
                if sessions.is_empty() {
                    t.denied_since = None;
                    return;
                }
                let since = *t.denied_since.get_or_insert_with(|| {
                    tracing::info!(user, %reason, "out of time, locking sessions");
                    Instant::now()
                });
                for s in sessions.iter().filter(|s| !s.locked) {
                    if let Err(e) = self.logind.lock_session(&s.id).await {
                        tracing::warn!(user, session = %s.id, "lock failed: {e:#}");
                    }
                }
                if since.elapsed() >= self.config.grace {
                    tracing::info!(user, "grace period over, terminating sessions");
                    if let Err(e) = self.logind.terminate_user(sessions[0].uid).await {
                        tracing::error!(user, "terminate failed: {e:#}");
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
            let today = Local::now().date_naive();
            for user in &self.config.users {
                match api.policy(user).await {
                    Ok(p) if &p.username == user => {
                        self.state.lock().await.policies.insert(user.clone(), p);
                    }
                    Ok(p) => tracing::warn!(user, got = %p.username, "server sent policy for wrong user"),
                    Err(e) => tracing::warn!(user, "policy fetch failed: {e:#}"),
                }
                let report = {
                    let state = self.state.lock().await;
                    UsageReport {
                        username: user.clone(),
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
        }
    }

    async fn serve(self: Arc<Self>, listener: UnixListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = daemon.handle_connection(stream).await {
                            tracing::debug!("socket connection ended: {e:#}");
                        }
                    });
                }
                Err(e) => tracing::error!("accept failed: {e:#}"),
            }
        }
    }

    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
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

    async fn stream_warnings(
        &self,
        username: &str,
        writer: &mut tokio::net::unix::OwnedWriteHalf,
    ) -> Result<()> {
        if !self.config.is_managed(username) {
            return write_line(writer, &SocketResponse::Unmanaged).await;
        }
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
        if !self.config.is_managed(username) {
            return SocketResponse::Unmanaged;
        }
        let (decision, used_minutes) = self.state.lock().await.decide(username, Local::now().naive_local());
        SocketResponse::Decision { decision, used_minutes }
    }

    async fn answer(&self, request: SocketRequest) -> SocketResponse {
        match request {
            SocketRequest::CheckLogin { username } | SocketRequest::Status { username } => {
                self.status(&username).await
            }
            SocketRequest::RequestTime { username, minutes, reason } => {
                if !self.config.is_managed(&username) {
                    return SocketResponse::Unmanaged;
                }
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
                    username,
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
            SocketRequest::Subscribe { .. } => unreachable!("handled by the connection loop"),
        }
    }

    async fn parent_grant(&self, username: String, minutes: u32, admin: String, password: String) -> SocketResponse {
        let error = |message: &str| SocketResponse::Error { message: message.to_owned() };
        if !self.config.is_managed(&username) {
            return SocketResponse::Unmanaged;
        }
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

        // A managed account or one outside the admin group fails exactly like a wrong password.
        let allowed_admin = !self.config.is_managed(&admin)
            && admin != "root"
            && parent::in_group(&admin, &self.config.admin_group);
        let verified = allowed_admin && {
            let service = self.config.pam_service.clone();
            let confdir = self.config.pam_confdir.clone();
            let admin = admin.clone();
            tokio::task::spawn_blocking(move || {
                parent::authenticate(&service, confdir.as_deref(), &admin, &password)
            })
            .await
            .map(|r| r.inspect_err(|e| tracing::warn!("parent check failed: {e}")).is_ok())
            .unwrap_or(false)
        };
        if !verified {
            tracing::warn!(%admin, user = %username, "refused \"my parent is here\" grant");
            tokio::time::sleep(PARENT_FAILURE_DELAY).await;
            return error("That name and password can't approve time.");
        }

        self.parent_failures.lock().await.clear();
        let mut state = self.state.lock().await;
        state.add_local_grant(&username, Local::now().naive_local(), minutes);
        if let Err(e) = state.save(&self.config.state_dir) {
            tracing::error!("saving state failed: {e:#}");
        }
        tracing::info!(%admin, user = %username, minutes, "parent granted time locally");
        SocketResponse::Granted { minutes }
    }
}

async fn write_line<W: AsyncWriteExt + Unpin>(writer: &mut W, response: &SocketResponse) -> Result<()> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::warning_bucket;

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
