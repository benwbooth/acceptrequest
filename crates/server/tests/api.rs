use acceptrequest_common::{DowntimeWindow, Policy, UsageReport};
use acceptrequest_server::{AppState, auth, config::Config, db, open_db, router};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Local, NaiveTime, Weekday};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct TestServer {
    app: Router,
    state: AppState,
    _dir: tempfile::TempDir,
}

async fn server(config: Config) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_db(&format!("sqlite://{}", dir.path().join("t.db").display())).await.unwrap();
    let state = AppState::new(pool, Config { default_allowance_minutes: 120, ..config });
    TestServer { app: router(state.clone()), state, _dir: dir }
}

impl TestServer {
    async fn call(&self, method: &str, uri: &str, auth: Option<&str>, body: Option<Value>) -> (StatusCode, Vec<(String, String)>, String) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(a) = auth {
            req = if a.starts_with("acceptrequest_session=") {
                req.header(header::COOKIE, a)
            } else {
                req.header(header::AUTHORIZATION, format!("Bearer {a}"))
            };
        }
        let body = match body {
            Some(v) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn policy(&self, token: &str, user: &str) -> Policy {
        let (status, _, body) = self.call("GET", &format!("/api/client/policy/{user}"), Some(token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    async fn admin_cookie(&self) -> String {
        let (status, headers, _) =
            self.call("POST", "/api/login", None, Some(json!({"password": "correct horse"}))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cookie = headers.iter().find(|(k, _)| k == "set-cookie").unwrap().1.clone();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        cookie.split(';').next().unwrap().to_owned()
    }
}

fn with_password() -> Config {
    Config { admin_password_hash: Some(auth::hash_password("correct horse").unwrap()), ..Config::default() }
}

#[tokio::test]
async fn client_api_needs_a_valid_token() {
    let s = server(Config::default()).await;
    assert_eq!(s.call("GET", "/api/client/policy/kid", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.call("GET", "/api/client/policy/kid", Some("nope"), None).await.0, StatusCode::UNAUTHORIZED);
    let token = db::add_client(&s.state.db, "pc1").await.unwrap();
    assert_eq!(s.call("GET", "/api/client/policy/kid", Some(&token), None).await.0, StatusCode::OK);
    assert_eq!(s.call("GET", "/api/client/policy/..%2Fx", Some(&token), None).await.0, StatusCode::BAD_REQUEST);
    // Re-registering a machine revokes its old token.
    let new = db::add_client(&s.state.db, "pc1").await.unwrap();
    assert_eq!(s.call("GET", "/api/client/policy/kid", Some(&token), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(s.call("GET", "/api/client/policy/kid", Some(&new), None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn allowance_is_shared_across_machines() {
    let s = server(Config::default()).await;
    let pc1 = db::add_client(&s.state.db, "pc1").await.unwrap();
    let pc2 = db::add_client(&s.state.db, "pc2").await.unwrap();
    let today = Local::now().date_naive();

    let p = s.policy(&pc1, "kid").await;
    assert_eq!((p.daily_allowance_minutes, p.bonus_minutes, p.day), (Some(120), 0, today));

    let report = UsageReport { username: "kid".into(), host: "ignored".into(), day: today, active_minutes: 50, local_bonus_minutes: 0 };
    let (status, _, _) = s.call("POST", "/api/client/usage", Some(&pc1), Some(serde_json::to_value(&report).unwrap())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // pc1 counts its own 50 minutes itself; pc2 sees them taken off the allowance.
    assert_eq!(s.policy(&pc1, "kid").await.daily_allowance_minutes, Some(120));
    assert_eq!(s.policy(&pc2, "kid").await.daily_allowance_minutes, Some(70));
}

#[tokio::test]
async fn notification_link_grants_once() {
    let s = server(Config::default()).await;
    let pc1 = db::add_client(&s.state.db, "pc1").await.unwrap();
    let uid = db::ensure_user(&s.state.db, "kid", 120).await.unwrap();
    let (row, token) = db::create_request(&s.state.db, uid, "pc1", Some(30), Some("<b>homework</b>")).await.unwrap();

    let (status, _, page) = s.call("GET", &format!("/r/{}/{token}", row.id), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("&lt;b&gt;homework&lt;/b&gt;") && !page.contains("<b>homework"));
    assert!(page.contains("+45 min") && page.contains("Custom minutes"));

    assert_eq!(s.call("GET", &format!("/r/{}/wrong", row.id), None, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(s.call("POST", &format!("/act/{}/wrong/30", row.id), None, None).await.0, StatusCode::NOT_FOUND);

    let (status, _, body) = s.call("POST", &format!("/act/{}/{token}/30", row.id), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Added 30 minutes"));
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 30);

    let (_, _, body) = s.call("POST", &format!("/act/{}/{token}/60", row.id), None, None).await;
    assert!(body.contains("already answered"));
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 30);
}

#[tokio::test]
async fn admin_login_and_settings() {
    let s = server(with_password()).await;
    assert_eq!(s.call("GET", "/api/overview", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        s.call("GET", "/api/overview", Some("acceptrequest_session=forged"), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.call("POST", "/api/login", None, Some(json!({"password": "wrong"}))).await.0,
        StatusCode::UNAUTHORIZED
    );
    let cookie = s.admin_cookie().await;
    let admin = Some(cookie.as_str());

    assert_eq!(s.call("POST", "/api/users", admin, Some(json!({"username": "kid"}))).await.0, StatusCode::CREATED);
    let allowances = json!([60, 60, 60, 60, 90, 180, null]);
    assert_eq!(s.call("PUT", "/api/users/kid/allowances", admin, Some(allowances)).await.0, StatusCode::NO_CONTENT);
    let downtime = vec![DowntimeWindow {
        days: vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Sun],
        start: NaiveTime::from_hms_opt(21, 0, 0).unwrap(),
        end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
    }];
    let (status, _, _) = s.call("PUT", "/api/users/kid/downtime", admin, Some(serde_json::to_value(&downtime).unwrap())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(s.call("POST", "/api/users/kid/bonus", admin, Some(json!({"minutes": 45}))).await.0, StatusCode::NO_CONTENT);

    let pc1 = db::add_client(&s.state.db, "pc1").await.unwrap();
    let p = s.policy(&pc1, "kid").await;
    let expected = [Some(60), Some(60), Some(60), Some(60), Some(90), Some(180), None];
    use chrono::Datelike;
    assert_eq!(p.daily_allowance_minutes, expected[Local::now().weekday().num_days_from_monday() as usize]);
    assert_eq!(p.bonus_minutes, 45);
    assert_eq!(p.downtime, downtime);

    // A request from the machine shows up for the admin and can be answered there.
    let (status, _, _) = s
        .call("POST", "/api/client/requests", Some(&pc1), Some(json!({"username": "kid", "host": "x", "requested_minutes": 15, "reason": null})))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, _, body) = s.call("GET", "/api/overview", admin, None).await;
    let overview: Value = serde_json::from_str(&body).unwrap();
    let pending = &overview["pending_requests"][0];
    assert_eq!((pending["username"].as_str(), pending["host"].as_str()), (Some("kid"), Some("pc1")));
    let id = pending["id"].as_i64().unwrap();
    let (status, _, _) = s.call("POST", &format!("/api/requests/{id}/resolve"), admin, Some(json!({"minutes": 0}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        s.call("POST", &format!("/api/requests/{id}/resolve"), admin, Some(json!({"minutes": 15}))).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 45);

    // Machines can be added from the UI, and logging out ends the session.
    let (status, _, body) = s.call("POST", "/api/clients", admin, Some(json!({"hostname": "pc2"}))).await;
    assert_eq!(status, StatusCode::OK);
    let token: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(s.policy(token["token"].as_str().unwrap(), "kid").await.username, "kid");
    assert_eq!(s.call("POST", "/api/logout", admin, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(s.call("GET", "/api/overview", admin, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_is_throttled() {
    let s = server(with_password()).await;
    for _ in 0..5 {
        assert_eq!(s.call("POST", "/api/login", None, Some(json!({"password": "nope"}))).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        s.call("POST", "/api/login", None, Some(json!({"password": "correct horse"}))).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn no_password_means_no_login() {
    let s = server(Config::default()).await;
    assert_eq!(
        s.call("POST", "/api/login", None, Some(json!({"password": ""}))).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

/// A stand-in ntfy server: publishing stores a message on a topic, and `/{topic}/json` returns
/// the stored messages as JSON lines and closes, which the server treats as a reconnect.
/// (topic, headers, body) for each published message.
type Published = Vec<(String, Vec<(String, String)>, String)>;

#[derive(Clone, Default)]
struct FakeNtfy {
    topics: Arc<Mutex<Published>>,
}

impl FakeNtfy {
    async fn start(&self) -> std::net::SocketAddr {
        let publish = self.clone();
        let subscribe = self.clone();
        let app = Router::new()
            .route(
                "/{topic}",
                axum::routing::post(move |axum::extract::Path(topic): axum::extract::Path<String>, headers: axum::http::HeaderMap, body: String| {
                    let me = publish.clone();
                    async move {
                        let h = headers.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned())).collect();
                        me.topics.lock().unwrap().push((topic, h, body));
                        "{}"
                    }
                }),
            )
            .route(
                "/{topic}/json",
                axum::routing::get(move |axum::extract::Path(topic): axum::extract::Path<String>| {
                    let me = subscribe.clone();
                    async move {
                        let topics = me.topics.lock().unwrap();
                        topics
                            .iter()
                            .enumerate()
                            .filter(|(_, (t, _, _))| *t == topic)
                            .map(|(i, (_, _, body))| json!({"id": format!("m{i}"), "event": "message", "message": body}).to_string() + "\n")
                            .collect::<String>()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    fn on(&self, topic: &str) -> Vec<(Vec<(String, String)>, String)> {
        self.topics.lock().unwrap().iter().filter(|(t, _, _)| t == topic).map(|(_, h, b)| (h.clone(), b.clone())).collect()
    }

    async fn wait_for(&self, topic: &str, count: usize) {
        for _ in 0..200 {
            if self.on(topic).len() >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {count} message(s) on {topic}");
    }
}

#[tokio::test]
async fn ntfy_buttons_answer_through_ntfy() {
    let ntfy = FakeNtfy::default();
    let addr = ntfy.start().await;
    let s = server(Config {
        ntfy_url: Some(format!("http://{addr}/topic")),
        ntfy_token: Some("tk".into()),
        public_url: "https://screentime.example".into(),
        ..Config::default()
    })
    .await;
    tokio::spawn(acceptrequest_server::notify::follow_replies(s.state.clone()));
    let pc1 = db::add_client(&s.state.db, "pc1").await.unwrap();
    let (status, _, _) = s
        .call("POST", "/api/client/requests", Some(&pc1), Some(json!({"username": "kid", "host": "pc1", "requested_minutes": 45, "reason": "finish the level"})))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    ntfy.wait_for("topic", 1).await;

    let (headers, body) = ntfy.on("topic")[0].clone();
    let get = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone()).unwrap_or_default();
    assert_eq!(get("title"), "kid wants more screen time");
    assert_eq!(get("authorization"), "Bearer tk");
    assert!(body.contains("45 more minutes on pc1") && body.contains("finish the level"), "{body}");
    assert!(get("click").starts_with("https://screentime.example/r/1/"));
    // Three buttons, each posting back to the answers topic, never to the server itself.
    let actions = get("actions");
    let buttons: Vec<&str> = actions.split("; ").collect();
    assert_eq!(buttons.len(), 3, "{actions}");
    assert!(buttons[0].starts_with(&format!("http, +45 min, http://{addr}/topic-answers, method=POST, body=grant:1:")), "{actions}");
    assert!(buttons[0].contains("headers.Authorization=Bearer tk") && !actions.contains("screentime.example"), "{actions}");

    // A forged answer is ignored; the phone tapping "+45 min" grants it.
    let real = buttons[0].split("body=").nth(1).unwrap().split(',').next().unwrap().to_owned();
    let client = reqwest::Client::new();
    client.post(format!("http://{addr}/topic-answers")).body("grant:1:not-the-token:600").send().await.unwrap();
    client.post(format!("http://{addr}/topic-answers")).body(real.clone()).send().await.unwrap();
    ntfy.wait_for("topic", 2).await;
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 45);
    let (confirm_headers, _) = ntfy.on("topic")[1].clone();
    assert!(confirm_headers.iter().any(|(k, v)| k == "title" && v == "Gave kid 45 more minutes"));

    // Replaying the same answer does nothing more.
    client.post(format!("http://{addr}/topic-answers")).body(real).send().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 45);
    assert_eq!(ntfy.on("topic").len(), 2);
}

#[tokio::test]
async fn request_sends_encrypted_web_push_with_grant_buttons() {
    use base64ct::{Base64UrlUnpadded, Encoding};
    use web_push_native::p256::{SecretKey, elliptic_curve::sec1::ToEncodedPoint};

    // A stand-in push service (what FCM or Apple would be) recording what it receives.
    type Received = Vec<(Vec<(String, String)>, Vec<u8>)>;
    let seen: Arc<Mutex<Received>> = Arc::default();
    let record = seen.clone();
    let fake = Router::new().route(
        "/push/{id}",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let record = record.clone();
            async move {
                let h = headers.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned())).collect();
                record.lock().unwrap().push((h, body.to_vec()));
                StatusCode::CREATED
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let s = server(Config { public_url: "https://screentime.example".into(), ..with_password() }).await;
    // The browser's side of the subscription.
    let ua_secret = SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
    let auth: [u8; 16] = rand::random();
    let p256dh = Base64UrlUnpadded::encode_string(ua_secret.public_key().to_encoded_point(false).as_bytes());
    // Real endpoints must be https; insert directly to point at the local stand-in.
    sqlx::query("INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_at) VALUES (?, ?, ?, 'now')")
        .bind(format!("http://{addr}/push/1"))
        .bind(&p256dh)
        .bind(Base64UrlUnpadded::encode_string(&auth))
        .execute(&s.state.db)
        .await
        .unwrap();

    let pc1 = db::add_client(&s.state.db, "pc1").await.unwrap();
    let (status, _, _) = s
        .call("POST", "/api/client/requests", Some(&pc1), Some(json!({"username": "kid", "host": "pc1", "requested_minutes": 30, "reason": null})))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    for _ in 0..50 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (headers, body) = seen.lock().unwrap()[0].clone();
    let get = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone()).unwrap_or_default();
    assert_eq!(get("content-encoding"), "aes128gcm");
    let cookie = s.admin_cookie().await;
    let (_, _, info) = s.call("GET", "/api/push", Some(&cookie), None).await;
    let key: Value = serde_json::from_str(&info).unwrap();
    assert!(get("authorization").contains(&format!("k={}", key["public_key"].as_str().unwrap())), "{}", get("authorization"));

    let plain = web_push_native::decrypt(body, &ua_secret, &web_push_native::Auth::clone_from_slice(&auth)).unwrap();
    let payload: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(payload["title"], "kid wants more screen time");
    assert_eq!(payload["actions"][0]["title"], "+30 min");
    let act = payload["actions"][0]["url"].as_str().unwrap();
    assert!(act.starts_with("https://screentime.example/act/1/"));

    // The notification button's URL grants the time.
    let path = act.trim_start_matches("https://screentime.example");
    assert_eq!(s.call("POST", path, None, None).await.0, StatusCode::OK);
    assert_eq!(s.policy(&pc1, "kid").await.bonus_minutes, 30);
}
