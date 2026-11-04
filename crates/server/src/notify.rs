//! Phone notifications for time requests, with one-tap grant buttons: Web Push to every
//! subscribed browser, and ntfy as well if it's configured.

use crate::config::Config;
use crate::db::{self, RequestRow};
use crate::{AppState, push};
use sqlx::SqlitePool;
use std::time::Duration;

pub struct Notifier {
    http: reqwest::Client,
    url: Option<String>,
    /// Topic the ntfy buttons post answers to; the server follows it.
    reply_url: Option<String>,
    token: Option<String>,
    public_url: String,
    /// VAPID subject: an https URL or mailto: address push services can use to reach the sender.
    vapid_subject: String,
}

/// Up to two grant buttons for Web Push (Android shows two). The amount asked for comes first.
pub fn quick_amounts(requested: Option<i64>) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for m in requested.and_then(|m| u32::try_from(m).ok()).into_iter().chain([15, 30, 60]) {
        if m > 0 && !out.contains(&m) {
            out.push(m);
        }
    }
    out.truncate(2);
    out
}

impl Notifier {
    pub fn new(config: &Config) -> Notifier {
        Notifier {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("building HTTP client"),
            url: config.ntfy_url.clone(),
            reply_url: config
                .ntfy_reply_url
                .clone()
                .or_else(|| config.ntfy_url.as_ref().map(|u| format!("{}-answers", u.trim_end_matches('/')))),
            token: config.ntfy_token.clone(),
            public_url: config.public_url.clone(),
            vapid_subject: config.vapid_subject.clone().unwrap_or_else(|| {
                if config.public_url.starts_with("https://") {
                    config.public_url.clone()
                } else {
                    "mailto:screentime@example.invalid".to_owned()
                }
            }),
        }
    }

    pub fn request_page_url(&self, id: i64, token: &str) -> String {
        format!("{}/r/{id}/{token}", self.public_url)
    }

    fn action_url(&self, id: i64, token: &str, minutes: u32) -> String {
        format!("{}/act/{id}/{token}/{minutes}", self.public_url)
    }

    pub async fn send_push(&self, db: &SqlitePool, payload: &push::Payload) -> anyhow::Result<usize> {
        push::send_all(db, &self.http, &self.vapid_subject, payload).await
    }

    pub async fn time_request(&self, db: &SqlitePool, row: &RequestRow, token: &str) {
        let page = self.request_page_url(row.id, token);
        let quick: Vec<(u32, String)> = quick_amounts(row.requested_minutes)
            .into_iter()
            .map(|m| (m, self.action_url(row.id, token, m)))
            .collect();
        match self.send_push(db, &push::request_payload(row, page.clone(), &quick)).await {
            Ok(n) => tracing::info!(id = row.id, delivered = n, "web push sent"),
            Err(e) => tracing::error!(id = row.id, "web push failed: {e:#}"),
        }
        self.ntfy(row, &page, token).await;
    }

    async fn ntfy(&self, row: &RequestRow, page: &str, token: &str) {
        let (Some(url), Some(reply_url)) = (&self.url, &self.reply_url) else { return };
        // Buttons answer through ntfy itself rather than calling this server, so they work from
        // anywhere and the phone never has to trust this server's certificate.
        let auth = self.token.as_ref().map(|t| format!(", headers.Authorization=Bearer {t}")).unwrap_or_default();
        let actions: Vec<String> = ntfy_amounts(row.requested_minutes)
            .into_iter()
            .map(|m| {
                format!(
                    "http, +{m} min, {reply_url}, method=POST, body={}{auth}, clear=true",
                    reply_body(row.id, token, m)
                )
            })
            .collect();

        let asked = match row.requested_minutes {
            Some(m) => format!("{m} more minutes"),
            None => "more time".to_owned(),
        };
        let mut body = format!("{} is asking for {asked} on {}.", row.username, row.host);
        if let Some(reason) = &row.reason {
            body.push_str(&format!("\n“{reason}”"));
        }
        body.push_str("\nTap for other amounts.");
        self.publish(
            url,
            &format!("{} wants more screen time", row.username),
            "high",
            "hourglass_flowing_sand",
            Some(page),
            Some(&actions.join("; ")),
            body,
        )
        .await;
    }

    /// Tell the parent's phone that an answer went through.
    pub async fn ntfy_confirm(&self, row: &RequestRow) {
        let Some(url) = &self.url else { return };
        let title = match (row.status.as_str(), row.granted_minutes) {
            ("granted", Some(m)) => format!("Gave {} {m} more minutes", row.username),
            _ => format!("Said no to {}", row.username),
        };
        self.publish(url, &title, "low", "white_check_mark", None, None, String::new()).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn publish(
        &self,
        url: &str,
        title: &str,
        priority: &str,
        tags: &str,
        click: Option<&str>,
        actions: Option<&str>,
        body: String,
    ) {
        let mut req = self
            .http
            .post(url)
            .header("Title", title)
            .header("Priority", priority)
            .header("Tags", tags)
            .body(body);
        if let Some(click) = click {
            req = req.header("Click", click);
        }
        if let Some(actions) = actions {
            req = req.header("Actions", actions);
        }
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(_) => tracing::info!(title, "ntfy notification sent"),
            Err(e) => tracing::error!(title, "ntfy notification failed: {e:#}"),
        }
    }
}

/// Up to three grant buttons for ntfy (its limit): the amount asked for first.
pub fn ntfy_amounts(requested: Option<i64>) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for m in requested.and_then(|m| u32::try_from(m).ok()).into_iter().chain([15, 30, 60]) {
        if m > 0 && m <= 24 * 60 && !out.contains(&m) {
            out.push(m);
        }
    }
    out.truncate(3);
    out
}

/// What a button sends back on the reply topic. No commas, so it fits in an ntfy action.
pub fn reply_body(id: i64, token: &str, minutes: u32) -> String {
    format!("grant:{id}:{token}:{minutes}")
}

pub fn parse_reply(body: &str) -> Option<(i64, String, u32)> {
    let mut parts = body.trim().strip_prefix("grant:")?.split(':');
    let id = parts.next()?.parse().ok()?;
    let token = parts.next()?.to_owned();
    let minutes = parts.next()?.parse().ok().filter(|m| (1..=24 * 60).contains(m))?;
    parts.next().is_none().then_some((id, token, minutes))
}

const REPLY_SINCE: &str = "ntfy_reply_since";

/// Follow the ntfy reply topic and apply grants from notification buttons. Runs forever.
pub async fn follow_replies(state: AppState) {
    let (Some(reply_url), Some(_)) = (state.notifier.reply_url.clone(), state.notifier.url.as_ref()) else {
        return;
    };
    let http = reqwest::Client::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        match follow_once(&state, &http, &reply_url).await {
            Ok(()) => backoff = Duration::from_secs(1),
            Err(e) => {
                tracing::warn!("ntfy reply stream: {e:#}");
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
        tokio::time::sleep(backoff).await;
    }
}

async fn follow_once(state: &AppState, http: &reqwest::Client, reply_url: &str) -> anyhow::Result<()> {
    let since: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(REPLY_SINCE)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "10m".to_owned());
    let mut req = http.get(format!("{reply_url}/json")).query(&[("since", since.as_str())]);
    if let Some(t) = &state.notifier.token {
        req = req.bearer_auth(t);
    }
    let mut resp = req.send().await?.error_for_status()?;
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            handle_reply_line(state, &line).await?;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct NtfyEvent {
    id: String,
    event: String,
    #[serde(default)]
    message: String,
}

async fn handle_reply_line(state: &AppState, line: &[u8]) -> anyhow::Result<()> {
    let Ok(ev) = serde_json::from_slice::<NtfyEvent>(line) else { return Ok(()) };
    if ev.event != "message" {
        return Ok(());
    }
    if let Some((id, token, minutes)) = parse_reply(&ev.message) {
        // The token proves the button came from our notification; each request is answered once.
        if db::request_by_action_token(&state.db, id, &token).await?.is_some() {
            let today = chrono::Local::now().date_naive();
            if let Some(row) = db::resolve_request(&state.db, id, minutes, today).await? {
                tracing::info!(id, user = %row.username, minutes, "request answered from ntfy");
                state.notifier.ntfy_confirm(&row).await;
            }
        } else {
            tracing::warn!(id, "ignoring ntfy reply with a wrong token");
        }
    }
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(REPLY_SINCE)
        .bind(&ev.id)
        .execute(&state.db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_amounts_put_the_ask_first() {
        assert_eq!(quick_amounts(None), vec![15, 30]);
        assert_eq!(quick_amounts(Some(45)), vec![45, 15]);
        assert_eq!(ntfy_amounts(Some(45)), vec![45, 15, 30]);
        assert_eq!(ntfy_amounts(Some(30)), vec![30, 15, 60]);
        assert_eq!(ntfy_amounts(Some(0)), vec![15, 30, 60]);
    }

    #[test]
    fn reply_round_trip() {
        let body = reply_body(7, "abc123", 30);
        assert_eq!(parse_reply(&body), Some((7, "abc123".to_owned(), 30)));
        assert_eq!(parse_reply("grant:7:abc:0"), None);
        assert_eq!(parse_reply("grant:7:abc:30:extra"), None);
        assert_eq!(parse_reply("hello"), None);
    }
}
