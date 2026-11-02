//! Admin password and sessions, per-machine client tokens, and token hashing.

use crate::AppState;
use crate::error::{AppError, AppResult};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;
use chrono::Utc;
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE: &str = "acceptrequest_session";
pub const SESSION_DAYS: i64 = 30;

/// A fresh random token, hex encoded.
pub fn new_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>()).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

fn cookie_value<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// A signed-in admin, from the session cookie.
pub struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> AppResult<Admin> {
        let token = cookie_value(parts, SESSION_COOKIE).ok_or(AppError::Unauthorized)?;
        let found: Option<(String,)> =
            sqlx::query_as("SELECT token_hash FROM sessions WHERE token_hash = ? AND expires_at > ?")
                .bind(hash_token(token))
                .bind(Utc::now().to_rfc3339())
                .fetch_optional(&state.db)
                .await?;
        found.map(|_| Admin).ok_or(AppError::Unauthorized)
    }
}

/// A machine running the client daemon, from its bearer token.
pub struct Client {
    pub id: i64,
    pub hostname: String,
}

impl FromRequestParts<AppState> for Client {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> AppResult<Client> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        let found: Option<(i64, String)> =
            sqlx::query_as("SELECT id, hostname FROM clients WHERE token_hash = ?")
                .bind(hash_token(token))
                .fetch_optional(&state.db)
                .await?;
        let (id, hostname) = found.ok_or(AppError::Unauthorized)?;
        sqlx::query("UPDATE clients SET last_seen = ? WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(&state.db)
            .await?;
        Ok(Client { id, hostname })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let h = hash_password("hunter2").unwrap();
        assert!(verify_password(&h, "hunter2"));
        assert!(!verify_password(&h, "hunter3"));
        assert!(!verify_password("not a hash", "hunter2"));
    }
}
