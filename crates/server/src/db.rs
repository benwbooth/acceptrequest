//! Queries. Everything takes the pool so handlers stay thin.

use crate::auth::{hash_token, new_token};
use acceptrequest_common::{DowntimeWindow, Policy, UsageReport};
use anyhow::Result;
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use serde::Serialize;
use sqlx::SqlitePool;

pub type Allowances = [Option<u32>; 7];

fn days_to_mask(days: &[Weekday]) -> i64 {
    days.iter().fold(0, |m, d| m | 1 << d.num_days_from_monday())
}

fn mask_to_days(mask: i64) -> Vec<Weekday> {
    (0..7u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| Weekday::try_from(i).ok())
        .collect()
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

pub async fn user_id(db: &SqlitePool, username: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(db)
        .await?)
}

/// The user's id, creating the user with `default_minutes` every day if they're new.
pub async fn ensure_user(db: &SqlitePool, username: &str, default_minutes: u32) -> Result<i64> {
    if let Some(id) = user_id(db, username).await? {
        return Ok(id);
    }
    let mut tx = db.begin().await?;
    let id: i64 = sqlx::query_scalar("INSERT INTO users (username) VALUES (?) RETURNING id")
        .bind(username)
        .fetch_one(&mut *tx)
        .await?;
    for weekday in 0..7 {
        sqlx::query("INSERT INTO daily_allowances (user_id, weekday, minutes) VALUES (?, ?, ?)")
            .bind(id)
            .bind(weekday)
            .bind(default_minutes)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::info!(username, "added new managed user");
    Ok(id)
}

pub async fn usernames(db: &SqlitePool) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query_as("SELECT id, username FROM users ORDER BY username").fetch_all(db).await?)
}

pub async fn allowances(db: &SqlitePool, user: i64) -> Result<Allowances> {
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT weekday, minutes FROM daily_allowances WHERE user_id = ?")
            .bind(user)
            .fetch_all(db)
            .await?;
    let mut out = [None; 7];
    for (weekday, minutes) in rows {
        out[weekday as usize] = Some(minutes as u32);
    }
    Ok(out)
}

pub async fn set_allowances(db: &SqlitePool, user: i64, allowances: &Allowances) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM daily_allowances WHERE user_id = ?").bind(user).execute(&mut *tx).await?;
    for (weekday, minutes) in allowances.iter().enumerate() {
        if let Some(minutes) = minutes {
            sqlx::query("INSERT INTO daily_allowances (user_id, weekday, minutes) VALUES (?, ?, ?)")
                .bind(user)
                .bind(weekday as i64)
                .bind(minutes)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn downtime(db: &SqlitePool, user: i64) -> Result<Vec<DowntimeWindow>> {
    let rows: Vec<(i64, chrono::NaiveTime, chrono::NaiveTime)> = sqlx::query_as(
        "SELECT days, start_time, end_time FROM downtime_windows WHERE user_id = ? ORDER BY id",
    )
    .bind(user)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(days, start, end)| DowntimeWindow { days: mask_to_days(days), start, end })
        .collect())
}

pub async fn set_downtime(db: &SqlitePool, user: i64, windows: &[DowntimeWindow]) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM downtime_windows WHERE user_id = ?").bind(user).execute(&mut *tx).await?;
    for w in windows {
        sqlx::query("INSERT INTO downtime_windows (user_id, days, start_time, end_time) VALUES (?, ?, ?, ?)")
            .bind(user)
            .bind(days_to_mask(&w.days))
            .bind(w.start)
            .bind(w.end)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn bonus(db: &SqlitePool, user: i64, day: NaiveDate) -> Result<u32> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(minutes), 0) FROM bonus_grants WHERE user_id = ? AND day = ?",
    )
    .bind(user)
    .bind(day)
    .fetch_one(db)
    .await?;
    Ok(total.max(0) as u32)
}

pub async fn add_bonus(db: &SqlitePool, user: i64, day: NaiveDate, minutes: u32, source: &str) -> Result<()> {
    sqlx::query("INSERT INTO bonus_grants (user_id, day, minutes, source, granted_at) VALUES (?, ?, ?, ?, ?)")
        .bind(user)
        .bind(day)
        .bind(minutes)
        .bind(source)
        .bind(now())
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostUsage {
    pub host: String,
    pub active_minutes: u32,
    pub local_bonus_minutes: u32,
}

pub async fn usage(db: &SqlitePool, user: i64, day: NaiveDate) -> Result<Vec<HostUsage>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT host, active_minutes, local_bonus_minutes FROM usage WHERE user_id = ? AND day = ? ORDER BY host",
    )
    .bind(user)
    .bind(day)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(host, a, l)| HostUsage { host, active_minutes: a as u32, local_bonus_minutes: l as u32 })
        .collect())
}

pub async fn record_usage(db: &SqlitePool, user: i64, host: &str, report: &UsageReport) -> Result<()> {
    sqlx::query(
        "INSERT INTO usage (user_id, host, day, active_minutes, local_bonus_minutes, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id, host, day) DO UPDATE SET
           active_minutes = MAX(active_minutes, excluded.active_minutes),
           local_bonus_minutes = MAX(local_bonus_minutes, excluded.local_bonus_minutes),
           updated_at = excluded.updated_at",
    )
    .bind(user)
    .bind(host)
    .bind(report.day)
    .bind(report.active_minutes)
    .bind(report.local_bonus_minutes)
    .bind(now())
    .execute(db)
    .await?;
    Ok(())
}

/// The policy a client on `host` should enforce today. The day's allowance is shared across
/// machines, so time used on other machines is taken off it.
pub async fn policy_for(db: &SqlitePool, user: i64, username: &str, host: &str, today: NaiveDate) -> Result<Policy> {
    let allowance = allowances(db, user).await?[today.weekday().num_days_from_monday() as usize];
    let used_elsewhere: u32 = usage(db, user, today)
        .await?
        .iter()
        .filter(|u| u.host != host)
        .map(|u| u.active_minutes)
        .sum();
    Ok(Policy {
        username: username.to_owned(),
        day: today,
        daily_allowance_minutes: allowance.map(|a| a.saturating_sub(used_elsewhere)),
        bonus_minutes: bonus(db, user, today).await?,
        downtime: downtime(db, user).await?,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, sqlx::FromRow)]
pub struct RequestRow {
    pub id: i64,
    pub username: String,
    pub host: String,
    pub requested_minutes: Option<i64>,
    pub reason: Option<String>,
    pub status: String,
    pub granted_minutes: Option<i64>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

const REQUEST_COLUMNS: &str = "r.id, u.username, r.host, r.requested_minutes, r.reason, r.status,
    r.granted_minutes, r.created_at, r.resolved_at
    FROM time_requests r JOIN users u ON u.id = r.user_id";

/// Store a new pending request and return it with the token for its notification links.
pub async fn create_request(
    db: &SqlitePool,
    user: i64,
    host: &str,
    minutes: Option<u32>,
    reason: Option<&str>,
) -> Result<(RequestRow, String)> {
    let token = new_token();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO time_requests (user_id, host, requested_minutes, reason, action_token_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user)
    .bind(host)
    .bind(minutes)
    .bind(reason)
    .bind(hash_token(&token))
    .bind(now())
    .fetch_one(db)
    .await?;
    let row = get_request(db, id).await?.expect("just inserted");
    Ok((row, token))
}

pub async fn get_request(db: &SqlitePool, id: i64) -> Result<Option<RequestRow>> {
    Ok(sqlx::query_as(&format!("SELECT {REQUEST_COLUMNS} WHERE r.id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?)
}

/// The request, if `token` is the one from its notification.
pub async fn request_by_action_token(db: &SqlitePool, id: i64, token: &str) -> Result<Option<RequestRow>> {
    Ok(sqlx::query_as(&format!("SELECT {REQUEST_COLUMNS} WHERE r.id = ? AND r.action_token_hash = ?"))
        .bind(id)
        .bind(hash_token(token))
        .fetch_optional(db)
        .await?)
}

pub async fn list_requests(db: &SqlitePool, pending_only: bool, limit: i64) -> Result<Vec<RequestRow>> {
    let filter = if pending_only { "WHERE r.status = 'pending'" } else { "" };
    Ok(sqlx::query_as(&format!("SELECT {REQUEST_COLUMNS} {filter} ORDER BY r.id DESC LIMIT ?"))
        .bind(limit)
        .fetch_all(db)
        .await?)
}

/// Grant `minutes` (0 denies) for a pending request, adding the bonus to `today`.
/// Returns `None` if the request was already handled, so each link works once.
pub async fn resolve_request(db: &SqlitePool, id: i64, minutes: u32, today: NaiveDate) -> Result<Option<RequestRow>> {
    let mut tx = db.begin().await?;
    let status = if minutes == 0 { "denied" } else { "granted" };
    let user: Option<i64> = sqlx::query_scalar(
        "UPDATE time_requests SET status = ?, granted_minutes = ?, resolved_at = ?
         WHERE id = ? AND status = 'pending' RETURNING user_id",
    )
    .bind(status)
    .bind(minutes)
    .bind(now())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = user else { return Ok(None) };
    if minutes > 0 {
        sqlx::query("INSERT INTO bonus_grants (user_id, day, minutes, source, granted_at) VALUES (?, ?, ?, ?, ?)")
            .bind(user)
            .bind(today)
            .bind(minutes)
            .bind(format!("request {id}"))
            .bind(now())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    get_request(db, id).await
}

/// Register a machine, or replace its token. Returns the new token, shown once.
pub async fn add_client(db: &SqlitePool, hostname: &str) -> Result<String> {
    let token = new_token();
    sqlx::query(
        "INSERT INTO clients (hostname, token_hash) VALUES (?, ?)
         ON CONFLICT (hostname) DO UPDATE SET token_hash = excluded.token_hash",
    )
    .bind(hostname)
    .bind(hash_token(&token))
    .execute(db)
    .await?;
    Ok(token)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ClientRow {
    pub hostname: String,
    pub last_seen: Option<String>,
}

pub async fn list_clients(db: &SqlitePool) -> Result<Vec<ClientRow>> {
    Ok(sqlx::query_as("SELECT hostname, last_seen FROM clients ORDER BY hostname").fetch_all(db).await?)
}

pub async fn remove_client(db: &SqlitePool, hostname: &str) -> Result<bool> {
    let r = sqlx::query("DELETE FROM clients WHERE hostname = ?").bind(hostname).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

pub async fn create_session(db: &SqlitePool, days: i64) -> Result<String> {
    let token = new_token();
    let created = Utc::now();
    sqlx::query("INSERT INTO sessions (token_hash, created_at, expires_at) VALUES (?, ?, ?)")
        .bind(hash_token(&token))
        .bind(created.to_rfc3339())
        .bind((created + chrono::Duration::days(days)).to_rfc3339())
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at <= ?").bind(created.to_rfc3339()).execute(db).await?;
    Ok(token)
}

const ADMIN_HASH_SETTING: &str = "admin_password_hash";

/// The admin password hash set from the web UI, if it has been changed there.
pub async fn admin_password_hash(db: &SqlitePool) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(ADMIN_HASH_SETTING).fetch_optional(db).await?)
}

pub async fn set_admin_password_hash(db: &SqlitePool, hash: &str) -> Result<()> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(ADMIN_HASH_SETTING)
        .bind(hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete_all_sessions(db: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM sessions").execute(db).await?;
    Ok(())
}

pub async fn delete_session(db: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?").bind(hash_token(token)).execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_mask_round_trip() {
        let days = vec![Weekday::Mon, Weekday::Wed, Weekday::Sun];
        assert_eq!(days_to_mask(&days), 0b1000101);
        assert_eq!(mask_to_days(days_to_mask(&days)), days);
    }
}
