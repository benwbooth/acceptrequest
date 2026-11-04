//! screentime server: stores screen time settings in SQLite, serves the client daemons
//! and the admin web UI, and sends phone notifications for time requests.

pub mod admin;
pub mod auth;
pub mod certs;
pub mod client_api;
pub mod config;
pub mod db;
pub mod error;
pub mod notify;
pub mod push;
pub mod request_page;

use axum::Router;
use config::Config;
use notify::Notifier;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub notifier: Arc<Notifier>,
    /// Recent failed admin logins, for throttling.
    pub login_failures: Arc<Mutex<Vec<Instant>>>,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Config) -> AppState {
        let notifier = Notifier::new(&config);
        AppState {
            db,
            config: Arc::new(config),
            notifier: Arc::new(notifier),
            login_failures: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub fn router(state: AppState) -> Router {
    use axum::http::{HeaderName, HeaderValue, header};
    use tower_http::set_header::SetResponseHeaderLayer;

    let web_dir = state.config.web_dir.clone();
    let mut app = Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .merge(client_api::routes())
        .merge(admin::routes())
        .merge(request_page::routes())
        .with_state(state);
    match web_dir {
        Some(dir) => {
            let index = tower_http::services::ServeFile::new(dir.join("index.html"));
            app = app.fallback_service(tower_http::services::ServeDir::new(dir).fallback(index));
        }
        None => tracing::warn!("no web UI found; set SCREENTIME_WEB_DIR to serve it"),
    }
    let header = |name: HeaderName, value: &'static str| {
        SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
    };
    app.layer(header(header::CONTENT_SECURITY_POLICY, CSP))
        .layer(header(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        // Notification links carry tokens in the path, so never leak them in a Referer.
        .layer(header(header::REFERRER_POLICY, "no-referrer"))
        .layer(header(header::CACHE_CONTROL, "no-cache"))
        .layer(tower_http::compression::CompressionLayer::new())
}

/// Open (creating if needed) the database and bring its schema up to date.
pub async fn open_db(url: &str) -> anyhow::Result<SqlitePool> {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}
