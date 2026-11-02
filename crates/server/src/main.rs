use anyhow::Result;
use axum::{Router, routing::get};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let db_url = std::env::var("ACCEPTREQUEST_DB").unwrap_or_else(|_| "sqlite://acceptrequest.db".into());
    let pool = SqlitePoolOptions::new()
        .connect_with(SqliteConnectOptions::from_str(&db_url)?.create_if_missing(true))
        .await?;
    sqlx::migrate!().run(&pool).await?;

    // TODO: rustls termination, argon2 password login, policy/request API, Leptos SSR routes, ntfy.
    let app = Router::new().route("/healthz", get(|| async { "ok" }));

    let addr = std::env::var("ACCEPTREQUEST_LISTEN").unwrap_or_else(|_| "127.0.0.1:8443".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
