//! Phone notifications through ntfy, with one-tap grant buttons.

use crate::config::Config;
use crate::db::RequestRow;
use std::time::Duration;

pub struct Notifier {
    http: reqwest::Client,
    url: Option<String>,
    token: Option<String>,
    public_url: String,
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
        }
    }

    pub fn request_page_url(&self, id: i64, token: &str) -> String {
        format!("{}/r/{id}/{token}", self.public_url)
    }

    pub async fn time_request(&self, row: &RequestRow, token: &str) {
        let Some(url) = &self.url else {
            tracing::info!(id = row.id, "no ntfy URL configured, not notifying");
            return;
        };
        let page = self.request_page_url(row.id, token);
        let mut actions: Vec<String> = quick_amounts(row.requested_minutes)
            .into_iter()
            .map(|m| format!("http, +{m} min, {}/act/{}/{token}/{m}, method=POST, clear=true", self.public_url, row.id))
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
            .header("Click", &page)
            .header("Actions", actions.join("; "))
            .body(body);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        match req.send().await.and_then(|r| r.error_for_status()) {
            Ok(_) => tracing::info!(id = row.id, "notification sent"),
            Err(e) => tracing::error!(id = row.id, "notification failed: {e:#}"),
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
