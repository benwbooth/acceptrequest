//! Admin API used by the web UI, behind the password-protected session cookie.

use crate::auth::{Admin, SESSION_COOKIE, SESSION_DAYS, verify_password};
use crate::client_api::valid_username;
use crate::db::{self, Allowances, ClientRow, HostUsage, RequestRow};
use crate::error::{AppError, AppResult};
use crate::{AppState, push};
use screentime_common::{Decision, DowntimeWindow, evaluate};
use axum::extract::{Path, Query, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const LOGIN_MAX_FAILURES: usize = 5;
const LOGIN_LOCKOUT: Duration = Duration::from_secs(10 * 60);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/password", post(change_password))
        .route("/api/overview", get(overview))
        .route("/api/users", post(add_user))
        .route("/api/users/{username}", get(user))
        .route("/api/users/{username}/allowances", put(set_allowances))
        .route("/api/users/{username}/downtime", put(set_downtime))
        .route("/api/users/{username}/bonus", post(add_bonus))
        .route("/api/requests", get(requests))
        .route("/api/requests/{id}/resolve", post(resolve_request))
        .route("/api/clients", get(clients).post(add_client))
        .route("/api/clients/{hostname}", delete(remove_client))
        .route("/api/clients/{hostname}/accounts/{account}", put(set_account))
        .route("/api/users/{username}/name", put(set_name))
        .route("/api/push", get(push_info))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/unsubscribe", post(push_unsubscribe))
        .route("/api/push/test", post(push_test))
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

pub const MIN_PASSWORD_CHARS: usize = 7;

/// Check the admin password, with the shared throttle. A password changed in the web UI is
/// stored in the database and takes precedence over the configured hash.
async fn check_password(s: &AppState, password: String) -> AppResult<bool> {
    let Some(hash) = db::admin_password_hash(&s.db).await?.or_else(|| s.config.admin_password_hash.clone()) else {
        return Ok(false);
    };
    {
        let mut failures = s.login_failures.lock().await;
        failures.retain(|at| at.elapsed() < LOGIN_LOCKOUT);
        if failures.len() >= LOGIN_MAX_FAILURES {
            return Err(AppError::TooManyAttempts);
        }
        // Counted before checking so parallel guesses can't all slip under the limit.
        failures.push(Instant::now());
    }
    let ok = tokio::task::spawn_blocking(move || verify_password(&hash, &password)).await?;
    if ok {
        s.login_failures.lock().await.clear();
    } else {
        tracing::warn!("wrong admin password");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(ok)
}

fn session_cookie(s: &AppState, token: &str) -> String {
    let secure = if s.config.public_url.starts_with("https://") { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
        SESSION_DAYS * 24 * 3600
    )
}

async fn login(State(s): State<AppState>, Json(body): Json<LoginBody>) -> AppResult<Response> {
    if s.config.admin_password_hash.is_none() && db::admin_password_hash(&s.db).await?.is_none() {
        return Ok((StatusCode::SERVICE_UNAVAILABLE, "no admin password has been set").into_response());
    }
    if !check_password(&s, body.password).await? {
        return Err(AppError::Unauthorized);
    }
    let token = db::create_session(&s.db, SESSION_DAYS).await?;
    Ok(([(SET_COOKIE, session_cookie(&s, &token))], StatusCode::NO_CONTENT).into_response())
}

#[derive(Deserialize)]
struct PasswordChange {
    current: String,
    new: String,
}

/// Change the admin password. Signs out every other session and gives this one a fresh cookie.
async fn change_password(State(s): State<AppState>, _: Admin, Json(body): Json<PasswordChange>) -> AppResult<Response> {
    if body.new.chars().count() < MIN_PASSWORD_CHARS {
        return Err(AppError::BadRequest(format!("Use at least {MIN_PASSWORD_CHARS} characters.")));
    }
    if !check_password(&s, body.current).await? {
        return Err(AppError::BadRequest("The current password isn't right.".into()));
    }
    let hash = tokio::task::spawn_blocking(move || crate::auth::hash_password(&body.new)).await??;
    db::set_admin_password_hash(&s.db, &hash).await?;
    db::delete_all_sessions(&s.db).await?;
    let token = db::create_session(&s.db, SESSION_DAYS).await?;
    tracing::info!("admin password changed; other sessions signed out");
    Ok(([(SET_COOKIE, session_cookie(&s, &token))], StatusCode::NO_CONTENT).into_response())
}

async fn logout(State(s): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    let token = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_owned());
    if let Some(token) = token {
        db::delete_session(&s.db, &token).await?;
    }
    let clear = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    Ok(([(SET_COOKIE, clear)], StatusCode::NO_CONTENT).into_response())
}

#[derive(Serialize)]
pub struct UserOverview {
    pub username: String,
    /// What to call them in the UI; the username if no name was set.
    pub name: String,
    /// Monday first. `null` means no limit that day.
    pub allowances: Allowances,
    pub downtime: Vec<DowntimeWindow>,
    pub bonus_today: u32,
    pub usage_today: Vec<HostUsage>,
    pub used_today: u32,
    /// Where things stand across all machines, ignoring on-the-spot grants made at a machine.
    pub decision: Decision,
}

async fn user_overview(s: &AppState, id: i64, username: String, today: NaiveDate) -> AppResult<UserOverview> {
    let usage_today = db::usage(&s.db, id, today).await?;
    let used_today = usage_today.iter().map(|u| u.active_minutes).sum();
    // An empty host means every machine's usage counts against the allowance.
    let policy = db::policy_for(&s.db, id, &username, "", today).await?;
    Ok(UserOverview {
        name: db::display_name(&s.db, id).await?.unwrap_or_else(|| username.clone()),
        decision: evaluate(Some(&policy), Local::now().naive_local(), 0),
        allowances: db::allowances(&s.db, id).await?,
        downtime: policy.downtime,
        bonus_today: policy.bonus_minutes,
        usage_today,
        used_today,
        username,
    })
}

#[derive(Serialize)]
struct Overview {
    today: NaiveDate,
    users: Vec<UserOverview>,
    pending_requests: Vec<RequestRow>,
    clients: Vec<ClientRow>,
}

async fn overview(State(s): State<AppState>, _: Admin) -> AppResult<Json<Overview>> {
    let today = Local::now().date_naive();
    let mut users = Vec::new();
    for (id, name) in db::usernames(&s.db).await? {
        users.push(user_overview(&s, id, name, today).await?);
    }
    Ok(Json(Overview {
        today,
        users,
        pending_requests: db::list_requests(&s.db, true, 50).await?,
        clients: db::list_clients(&s.db).await?,
    }))
}

async fn find_user(s: &AppState, username: &str) -> AppResult<i64> {
    db::user_id(&s.db, username).await?.ok_or(AppError::NotFound)
}

async fn user(State(s): State<AppState>, _: Admin, Path(username): Path<String>) -> AppResult<Json<UserOverview>> {
    let id = find_user(&s, &username).await?;
    Ok(Json(user_overview(&s, id, username, Local::now().date_naive()).await?))
}

#[derive(Deserialize)]
struct NewUser {
    username: String,
}

async fn add_user(State(s): State<AppState>, _: Admin, Json(body): Json<NewUser>) -> AppResult<StatusCode> {
    if !valid_username(&body.username) {
        return Err(AppError::BadRequest("invalid username".into()));
    }
    db::ensure_user(&s.db, &body.username, s.config.default_allowance_minutes).await?;
    Ok(StatusCode::CREATED)
}

async fn set_allowances(
    State(s): State<AppState>,
    _: Admin,
    Path(username): Path<String>,
    Json(allowances): Json<Allowances>,
) -> AppResult<StatusCode> {
    if allowances.iter().flatten().any(|m| *m > 24 * 60) {
        return Err(AppError::BadRequest("at most 24 hours a day".into()));
    }
    let id = find_user(&s, &username).await?;
    db::set_allowances(&s.db, id, &allowances).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_downtime(
    State(s): State<AppState>,
    _: Admin,
    Path(username): Path<String>,
    Json(windows): Json<Vec<DowntimeWindow>>,
) -> AppResult<StatusCode> {
    if windows.len() > 50 || windows.iter().any(|w| w.days.is_empty()) {
        return Err(AppError::BadRequest("each window needs at least one day".into()));
    }
    let id = find_user(&s, &username).await?;
    db::set_downtime(&s.db, id, &windows).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Minutes {
    minutes: u32,
}

async fn add_bonus(
    State(s): State<AppState>,
    _: Admin,
    Path(username): Path<String>,
    Json(body): Json<Minutes>,
) -> AppResult<StatusCode> {
    if body.minutes == 0 || body.minutes > 24 * 60 {
        return Err(AppError::BadRequest("between 1 minute and 24 hours".into()));
    }
    let id = find_user(&s, &username).await?;
    db::add_bonus(&s.db, id, Local::now().date_naive(), body.minutes, "web").await?;
    tracing::info!(user = %username, minutes = body.minutes, "bonus added from web");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RequestsQuery {
    #[serde(default)]
    all: bool,
}

async fn requests(State(s): State<AppState>, _: Admin, Query(q): Query<RequestsQuery>) -> AppResult<Json<Vec<RequestRow>>> {
    Ok(Json(db::list_requests(&s.db, !q.all, 200).await?))
}

async fn resolve_request(
    State(s): State<AppState>,
    _: Admin,
    Path(id): Path<i64>,
    Json(body): Json<Minutes>,
) -> AppResult<Response> {
    if body.minutes > 24 * 60 {
        return Err(AppError::BadRequest("at most 24 hours".into()));
    }
    match db::resolve_request(&s.db, id, body.minutes, Local::now().date_naive()).await? {
        Some(r) => Ok(Json(r).into_response()),
        None if db::get_request(&s.db, id).await?.is_some() => {
            Ok((StatusCode::CONFLICT, "already answered").into_response())
        }
        None => Err(AppError::NotFound),
    }
}

async fn clients(State(s): State<AppState>, _: Admin) -> AppResult<Json<Vec<ClientRow>>> {
    Ok(Json(db::list_clients(&s.db).await?))
}

#[derive(Deserialize)]
struct NewClient {
    hostname: String,
}

#[derive(Serialize)]
struct NewClientToken {
    hostname: String,
    token: String,
}

async fn add_client(State(s): State<AppState>, _: Admin, Json(body): Json<NewClient>) -> AppResult<Json<NewClientToken>> {
    let ok = !body.hostname.is_empty()
        && body.hostname.len() <= 253
        && body.hostname.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if !ok {
        return Err(AppError::BadRequest("invalid hostname".into()));
    }
    let token = db::add_client(&s.db, &body.hostname).await?;
    Ok(Json(NewClientToken { hostname: body.hostname, token }))
}

async fn remove_client(State(s): State<AppState>, _: Admin, Path(hostname): Path<String>) -> AppResult<StatusCode> {
    if db::remove_client(&s.db, &hostname).await? { Ok(StatusCode::NO_CONTENT) } else { Err(AppError::NotFound) }
}

#[derive(Serialize)]
struct PushInfo {
    public_key: String,
    subscriptions: i64,
}

async fn push_info(State(s): State<AppState>, _: Admin) -> AppResult<Json<PushInfo>> {
    Ok(Json(PushInfo {
        public_key: push::public_key(&s.db).await?,
        subscriptions: push::subscription_count(&s.db).await?,
    }))
}

async fn push_subscribe(State(s): State<AppState>, _: Admin, Json(sub): Json<push::Subscription>) -> AppResult<StatusCode> {
    push::subscribe(&s.db, &sub).await.map_err(|e| AppError::BadRequest(format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Endpoint {
    endpoint: String,
}

async fn push_unsubscribe(State(s): State<AppState>, _: Admin, Json(body): Json<Endpoint>) -> AppResult<StatusCode> {
    push::unsubscribe(&s.db, &body.endpoint).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct Delivered {
    delivered: usize,
}

async fn push_test(State(s): State<AppState>, _: Admin) -> AppResult<Json<Delivered>> {
    let payload = push::Payload {
        title: "Notifications are working".into(),
        body: "You'll get a notification like this when someone asks for more time.".into(),
        url: format!("{}/", s.config.public_url),
        actions: Vec::new(),
    };
    Ok(Json(Delivered { delivered: s.notifier.send_push(&s.db, &payload).await? }))
}

#[derive(Deserialize)]
struct AccountKid {
    kid: Option<String>,
}

async fn set_account(
    State(s): State<AppState>,
    _: Admin,
    Path((hostname, account)): Path<(String, String)>,
    Json(body): Json<AccountKid>,
) -> AppResult<StatusCode> {
    let kid = match &body.kid {
        Some(k) => Some(find_user(&s, k).await?),
        None => None,
    };
    if db::set_account(&s.db, &hostname, &account, kid).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Deserialize)]
struct DisplayName {
    name: String,
}

async fn set_name(
    State(s): State<AppState>,
    _: Admin,
    Path(username): Path<String>,
    Json(body): Json<DisplayName>,
) -> AppResult<StatusCode> {
    let id = find_user(&s, &username).await?;
    let name = body.name.trim();
    if name.chars().count() > 60 {
        return Err(AppError::BadRequest("at most 60 characters".into()));
    }
    db::set_display_name(&s.db, id, (!name.is_empty()).then_some(name)).await?;
    Ok(StatusCode::NO_CONTENT)
}
