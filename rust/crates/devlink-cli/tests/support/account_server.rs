use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use devlink_broker::config::{self, Config};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{net::TcpListener, task::JoinHandle};

pub const PASSWORD: &str = "test-password-do-not-print";
pub const COOKIE: &str = "post-login-cookie-do-not-print";
pub const CSRF: &str = "post-login-csrf-do-not-print";
pub const INVITE: &str = "synthetic-invite-code-read-from-stdin-only";

#[derive(Clone, Default)]
pub struct MockState {
    pub expired: Arc<AtomicBool>,
    pub logout_error: Arc<AtomicBool>,
    pub redirect_login: Arc<AtomicBool>,
    pub changed_user: Arc<AtomicBool>,
    pub me_failure: Arc<AtomicBool>,
    pub calls: Arc<Mutex<Vec<String>>>,
    pub joined: Arc<AtomicBool>,
    pub membership_missing: Arc<AtomicBool>,
    pub teams: Arc<Mutex<std::collections::BTreeMap<u64, Value>>>,
}

pub fn success(value: Value) -> Response {
    Json(json!({"success":true,"data":value,"code":null,"message":null})).into_response()
}
pub fn failure(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"success":false,"data":null,"code":code,"message":format!("{COOKIE} {PASSWORD}")}))).into_response()
}
fn cookie(headers: &HeaderMap, value: &str) -> bool {
    headers
        .get("cookie")
        .is_some_and(|v| v == &format!("JSESSIONID={value}"))
}
fn user() -> Value {
    json!({"userId":51,"email":"verified@example.test","name":"Verified\u{1b}[31m\u{202e}","nickname":null,"createdAt":"2026-10-29T00:00:00"})
}

async fn csrf(State(state): State<MockState>, headers: HeaderMap) -> Response {
    assert!(!headers.contains_key("authorization"));
    state.calls.lock().unwrap().push("csrf".into());
    let authenticated = cookie(&headers, COOKIE);
    let token = if authenticated {
        CSRF
    } else {
        "pre-login-csrf"
    };
    let mut response =
        success(json!({"headerName":"X-CSRF-TOKEN","parameterName":"_csrf","token":token}));
    if !authenticated {
        response.headers_mut().insert(
            "set-cookie",
            "JSESSIONID=pre-login-cookie; Path=/; HttpOnly"
                .parse()
                .unwrap(),
        );
    }
    response
}

async fn login(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.calls.lock().unwrap().push("login".into());
    assert!(cookie(&headers, "pre-login-cookie"));
    assert_eq!(headers.get("x-csrf-token").unwrap(), "pre-login-csrf");
    assert!(!headers.contains_key("authorization"));
    if state.redirect_login.load(Ordering::SeqCst) {
        return (StatusCode::FOUND, [("location", "/sink")]).into_response();
    }
    if body["password"] != PASSWORD {
        return failure(StatusCode::UNAUTHORIZED, "INVALID_CREDENTIALS");
    }
    state.expired.store(false, Ordering::SeqCst);
    let mut response = success(user());
    response.headers_mut().insert(
        "set-cookie",
        format!("JSESSIONID={COOKIE}; Path=/; HttpOnly")
            .parse()
            .unwrap(),
    );
    response
}

async fn me(State(state): State<MockState>, headers: HeaderMap) -> Response {
    state.calls.lock().unwrap().push("me".into());
    assert!(!headers.contains_key("authorization"));
    if !cookie(&headers, COOKIE) || state.expired.load(Ordering::SeqCst) {
        return failure(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    if state.me_failure.load(Ordering::SeqCst) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "UNTRUSTED_SECRET_RESPONSE");
    }
    let mut value = user();
    if state.changed_user.load(Ordering::SeqCst) {
        value["userId"] = json!(52);
    }
    success(value)
}

async fn logout(State(state): State<MockState>, headers: HeaderMap) -> Response {
    state.calls.lock().unwrap().push("logout".into());
    assert!(cookie(&headers, COOKIE));
    assert_eq!(headers.get("x-csrf-token").unwrap(), CSRF);
    if state.logout_error.load(Ordering::SeqCst) {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "UNTRUSTED_SECRET_RESPONSE");
    }
    state.expired.store(true, Ordering::SeqCst);
    success(Value::Null)
}

fn team_value(id: u64, name: &str, role: &str) -> Value {
    json!({"teamId":id,"name":name,"myRole":role,"createdAt":"2026-10-29T00:00:00"})
}
fn team_for(state: &MockState, id: u64) -> Option<Value> {
    match id {
        71 => Some(team_value(71, "Owned team", "OWNER")),
        72 if state.joined.load(Ordering::SeqCst) => Some(team_value(72, "Joined team", "MEMBER")),
        _ => state.teams.lock().unwrap().get(&id).cloned(),
    }
}
fn authorized(state: &MockState, headers: &HeaderMap, mutation: bool) -> bool {
    assert!(!headers.contains_key("authorization"));
    cookie(headers, COOKIE)
        && !state.expired.load(Ordering::SeqCst)
        && (!mutation || headers.get("x-csrf-token").is_some_and(|v| v == CSRF))
}
async fn team_list(State(state): State<MockState>, headers: HeaderMap) -> Response {
    state.calls.lock().unwrap().push("team_list".into());
    if !authorized(&state, &headers, false) {
        return failure(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    let mut teams = vec![team_for(&state, 71).unwrap()];
    if let Some(team) = team_for(&state, 72) {
        teams.push(team);
    }
    teams.extend(state.teams.lock().unwrap().values().cloned());
    success(json!(teams))
}
async fn team_create(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.calls.lock().unwrap().push("team_create".into());
    assert!(authorized(&state, &headers, true));
    assert_eq!(
        body.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["name"]
    );
    let mut teams = state.teams.lock().unwrap();
    let id = 101 + teams.len() as u64;
    let team = team_value(id, body["name"].as_str().unwrap(), "OWNER");
    teams.insert(id, team.clone());
    let mut created = team;
    created["inviteCode"] = json!(INVITE);
    success(created)
}
async fn team_detail(
    Path(id): Path<u64>,
    State(state): State<MockState>,
    headers: HeaderMap,
) -> Response {
    state.calls.lock().unwrap().push("team_detail".into());
    if !authorized(&state, &headers, false) {
        return failure(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    if id == 99 {
        return failure(StatusCode::FORBIDDEN, "ACCESS_DENIED");
    }
    team_for(&state, id)
        .map(success)
        .unwrap_or_else(|| failure(StatusCode::NOT_FOUND, "RESOURCE_NOT_FOUND"))
}
async fn team_members(
    Path(id): Path<u64>,
    State(state): State<MockState>,
    headers: HeaderMap,
) -> Response {
    state.calls.lock().unwrap().push("team_members".into());
    if !authorized(&state, &headers, false) {
        return failure(StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED");
    }
    if id == 99 {
        return failure(StatusCode::FORBIDDEN, "ACCESS_DENIED");
    }
    let Some(team) = team_for(&state, id) else {
        return failure(StatusCode::NOT_FOUND, "RESOURCE_NOT_FOUND");
    };
    let user = if state.membership_missing.load(Ordering::SeqCst) {
        777
    } else {
        51
    };
    success(
        json!([{"userId":user,"name":"Verified","nickname":null,"role":team["myRole"],"joinedAt":"2026-10-29T00:00:00"}]),
    )
}
async fn team_join(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.calls.lock().unwrap().push("team_join".into());
    assert!(authorized(&state, &headers, true));
    if body["inviteCode"] != INVITE {
        return failure(StatusCode::NOT_FOUND, "RESOURCE_NOT_FOUND");
    }
    let replayed = state.joined.swap(true, Ordering::SeqCst);
    success(
        json!({"teamId":72,"name":"Joined team","myRole":"MEMBER","joinedAt":"2026-10-29T00:00:00","replayed":replayed}),
    )
}

pub struct Fixture {
    pub _root: tempfile::TempDir,
    pub path: PathBuf,
    pub state: MockState,
    server: JoinHandle<()>,
}

impl Fixture {
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = MockState::default();
        let app = Router::new()
            .route("/api/v1/teams", get(team_list).post(team_create))
            .route("/api/v1/teams/join", post(team_join))
            .route("/api/v1/teams/{id}", get(team_detail))
            .route("/api/v1/teams/{id}/members", get(team_members))
            .route("/api/v1/auth/csrf", get(csrf))
            .route("/api/v1/auth/login", post(login))
            .route("/api/v1/auth/users/me", get(me))
            .route("/api/v1/auth/logout", post(logout))
            .route("/sink", get(|| async { StatusCode::IM_A_TEAPOT }))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        config::atomic_json(
            &path,
            &Config {
                server_url: format!("ws://{address}"),
                ..Config::default()
            },
        )
        .unwrap();
        Self {
            _root: root,
            path,
            state,
            server,
        }
    }

    pub fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_devlink"));
        command
            .arg("--config")
            .arg(&self.path)
            .args(args)
            .env_remove("DEVLINK_TOKEN")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    pub fn login(&self) -> Output {
        self.run(
            &[
                "auth",
                "login",
                "--email",
                "input@example.test",
                "--password-stdin",
            ],
            Some(&format!("{PASSWORD}\n")),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
pub fn assert_no_secrets(output: &Output) {
    let value = text(output);
    for secret in [PASSWORD, COOKIE, CSRF, "pre-login-cookie", "pre-login-csrf"] {
        assert!(!value.contains(secret));
    }
}
