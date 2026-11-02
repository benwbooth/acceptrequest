//! Phone notifications for time requests, with one-tap grant buttons: Web Push to every
//! subscribed browser, and ntfy as well if it's configured.

use crate::config::Config;
use crate::db::RequestRow;
use crate::push;
use sqlx::SqlitePool;
use std::time::Duration;

pub struct Notifier {
    http: reqwest::Client,
    url: Option<String>,
    token: Option<String>,
    public_url: String,
    /// VAPID subject: an https URL or mailto: address push services can use to reach the sender.
    vapid_subject: String,
}

/// Up to two quick-grant amounts for the notification (ntfy allows three buttons and the
/// third opens the page with every choice). The amount asked for comes first.
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
            token: config.ntfy_token.clone(),
            public_url: config.public_url.clone(),
            vapid_subject: config.vapid_subject.clone().unwrap_or_else(|| {
                if config.public_url.starts_with("https://") {
                    config.public_url.clone()
                } else {
                    "mailto:acceptrequest@example.invalid".to_owned()
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
        self.ntfy(row, &page, &quick).await;
    }

    async fn ntfy(&self, row: &RequestRow, page: &str, quick: &[(u32, String)]) {
        let Some(url) = &self.url else { return };
        let mut actions: Vec<String> = quick
            .iter()
            .map(|(m, act)| format!("http, +{m} min, {act}, method=POST, clear=true"))
            .collect();
        // Header values must be ASCII, so no ellipsis here.
        actions.push(format!("view, More options, {page}"));

        let asked = match row.requested_minutes {
            Some(m) => format!("{m} more minutes"),
            None => "more time".to_owned(),
        };
        let mut body = format!("{} is asking for {asked} on {}.", row.username, row.host);
        if let Some(reason) = &row.reason {
            body.push_str(&format!("\n“{reason}”"));
        }

        let mut req = self
            .http
            .post(url)
            .header("Title", format!("{} wants more screen time", row.username))
            .header("Priority", "high")
            .header("Tags", "hourglass_flowing_sand")
            .header("Click", page)
            .header("Actions", actions.join("; "))
            .body(body);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(_) => tracing::info!(id = row.id, "ntfy notification sent"),
            Err(e) => tracing::error!(id = row.id, "ntfy notification failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::quick_amounts;

    #[test]
    fn quick_amounts_put_the_ask_first() {
        assert_eq!(quick_amounts(None), vec![15, 30]);
        assert_eq!(quick_amounts(Some(45)), vec![45, 15]);
        assert_eq!(quick_amounts(Some(30)), vec![30, 15]);
        assert_eq!(quick_amounts(Some(0)), vec![15, 30]);
    }
}
