//! acceptrequest server: stores screen time settings in SQLite, serves the client daemons
//! and the admin web UI, and sends phone notifications for time requests.

pub mod admin;
pub mod auth;
pub mod client_api;
pub mod config;
pub mod db;
pub mod error;
pub mod notify;
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

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .merge(client_api::routes())
        .merge(admin::routes())
        .merge(request_page::routes())
        .with_state(state)
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
