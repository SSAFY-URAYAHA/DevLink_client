use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use devlink_broker::{
    central::{CentralIdentity, CentralState, ConnectionStatus},
    session_api,
    sessions::{ReadyIdentityProvider, SessionManager},
    task_api,
    task_client::TaskClient,
    task_requests::TaskRequests,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const LOCAL: &str = "synthetic-local-mcp-token-1234";
const AGENT: &str = "synthetic-central-agent-token-1234";
const SECRET: &str = "untrusted-central-message-do-not-expose";

#[derive(Clone)]
struct MockState {
    mode: Arc<AtomicU8>,
    calls: Arc<Mutex<Vec<Value>>>,
    saved: Arc<Mutex<HashMap<String, u64>>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    redirect: String,
    log: std::path::PathBuf,
}

async fn central_create(
    Path(team): Path<u64>,
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(team, 1);
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {AGENT}")
    );
    assert!(!headers.contains_key("cookie") && !headers.contains_key("origin"));
    let disk: Value = serde_json::from_slice(&std::fs::read(&state.log).unwrap()).unwrap();
    assert!(
        disk["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["command"]["requestKey"] == body["requestKey"]),
        "key was not persisted before HTTP"
    );
    state.calls.lock().await.push(body.clone());
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_active.fetch_max(active, Ordering::SeqCst);
    let mode = state.mode.load(Ordering::SeqCst);
    let key = body["requestKey"].as_str().unwrap().to_string();
    let mut saved = state.saved.lock().await;
    let replayed = saved.contains_key(&key);
    let next = saved.len() as u64 + 101;
    let task_id = *saved.entry(key).or_insert(next);
    drop(saved);
    if mode == 1 {
        sleep(Duration::from_millis(500)).await;
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
    match mode {
        11 => (StatusCode::CREATED, Json(json!({"success":true,"data":{
            "taskId":task_id,"receiverAgentId":body["receiverAgentId"]},"code":null,"message":null}))).into_response(),
        9 => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success":false,"data":{
            "taskId":task_id,"receiverAgentId":body["receiverAgentId"]},"code":"INTERNAL_SERVER_ERROR","message":SECRET}))).into_response(),
        2 => (StatusCode::UNAUTHORIZED, SECRET).into_response(),
        3 => (StatusCode::FORBIDDEN, SECRET).into_response(),
        4 => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success":false,"code":"INTERNAL_SERVER_ERROR","message":SECRET}))).into_response(),
        5 => (StatusCode::FOUND, [("location", state.redirect)], SECRET).into_response(),
        6 => (StatusCode::CREATED, SECRET).into_response(),
        7 => (StatusCode::CREATED, "x".repeat(256 * 1024 + 1)).into_response(),
        _ => (if replayed {StatusCode::OK} else {StatusCode::CREATED}, Json(json!({
            "success":true,"data":{"taskId":task_id,"receiverAgentId":body["receiverAgentId"],
            "status":if mode == 12 {"FUTURE_STATUS"} else {"PENDING"},"version":1,"replayed":replayed,
            "delivery": if mode == 10 {json!({"status":"ACK_RECEIVED","reasonCode":SECRET})}
                else if mode == 8 {json!({"status":"DELIVERY_UNKNOWN","reasonCode":"ACK_TIMEOUT"})} else {json!({"status":"ACK_PENDING","reasonCode":null})}},
            "code":null,"message":null
        }))).into_response(),
    }
}

struct Central {
    state: MockState,
    address: std::net::SocketAddr,
    server: JoinHandle<()>,
    trap: JoinHandle<()>,
    trapped: Arc<AtomicUsize>,
}

impl Central {
    async fn start(config_path: &std::path::Path) -> Self {
        let trapped = Arc::new(AtomicUsize::new(0));
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = format!("http://{}/leak", target.local_addr().unwrap());
        let count = trapped.clone();
        let trap = tokio::spawn(async move {
            loop {
                if target.accept().await.is_ok() {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let state = MockState {
            mode: Arc::new(AtomicU8::new(0)),
            calls: Arc::default(),
            saved: Arc::default(),
            active: Arc::default(),
            max_active: Arc::default(),
            redirect,
            log: TaskRequests::path(config_path),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/api/v1/tasks/{team}", post(central_create))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            state,
            address,
            server,
            trap,
            trapped,
        }
    }
    async fn wait_calls(&self, count: usize) {
        timeout(Duration::from_secs(5), async {
            loop {
                if self.state.calls.lock().await.len() >= count {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
impl Drop for Central {
    fn drop(&mut self) {
        self.server.abort();
        self.trap.abort();
    }
}

struct Broker {
    url: String,
    client: reqwest::Client,
    central: CentralState,
    cancel: CancellationToken,
    server: JoinHandle<()>,
    expiration: JoinHandle<()>,
}
impl Broker {
    async fn start(path: &std::path::Path, remote: &Central, request_timeout: Duration) -> Self {
        let central = CentralState::new(Arc::new(RwLock::new(ConnectionStatus::Connecting)));
        let handle = central.begin_session().await;
        handle
            .set_identity(CentralIdentity {
                connection_id: "verified-test-connection".into(),
                agent_id: 12,
                user_id: Some(3),
                team_id: Some(1),
            })
            .await;
        handle.set_status(ConnectionStatus::Connected).await;
        let sessions = SessionManager::new(
            Arc::new(ReadyIdentityProvider::new(central.clone())),
            central.clone(),
        );
        let requests = TaskRequests::open(path).unwrap();
        let task_client =
            TaskClient::with_timeout(&format!("ws://{}", remote.address), AGENT, request_timeout)
                .unwrap();
        let cancel = CancellationToken::new();
        let app =
            session_api::router(Arc::new(LOCAL.into()), sessions.clone()).merge(task_api::router(
                Arc::new(LOCAL.into()),
                sessions.clone(),
                task_client,
                requests,
                cancel.clone(),
            ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = cancel.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let expiration = tokio::spawn(sessions.run_expiration(cancel.clone()));
        Self {
            url,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            central,
            cancel,
            server,
            expiration,
        }
    }
    async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}{path}", self.url))
            .bearer_auth(LOCAL)
            .json(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let result: Value = response.json().await.unwrap();
        assert!(
            !result.to_string().contains(AGENT)
                && !result.to_string().contains(LOCAL)
                && !result.to_string().contains(SECRET)
        );
        (status, result)
    }
    async fn claim(&self, instance: &str, bound: bool) -> Value {
        let mut body =
            json!({"requestId":format!("claim-{instance}"),"instanceId":instance,"mode":"fresh"});
        if bound {
            body["agentId"] = json!("12");
        }
        let (status, response) = self.post("/v1/sessions", &body).await;
        assert_eq!(status, 200);
        response["data"].clone()
    }
    async fn stop(self) {
        self.cancel.cancel();
        timeout(Duration::from_secs(3), self.server)
            .await
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(3), self.expiration)
            .await
            .unwrap()
            .unwrap();
    }
}

fn request(session: &Value, id: &str) -> Value {
    let mut context = session["principal"].clone();
    context["sessionId"] = session["sessionId"].clone();
    json!({"requestId":id,"mcpRequestId":27,"operation":"create_task",
        "arguments":{"recipientAgentId":"13","title":"로그인 수정","description":"오류 메시지 수정","taskContext":"작업 맥락"},"context":context})
}

#[tokio::test]
async fn known_task_identity_survives_response_validation_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    for mode in [10, 11, 12] {
        remote.state.mode.store(mode, Ordering::SeqCst);
        let (status, result) = broker
            .post(
                "/v1/tasks/create",
                &request(&session, &format!("invalid-response-{mode}")),
            )
            .await;
        assert_eq!(status, 502);
        assert_eq!(result["error"]["code"], "CENTRAL_PROTOCOL_ERROR");
        assert_eq!(result["error"]["outcome"], "unknown");
        assert!(
            result["error"]["taskId"]
                .as_str()
                .is_some_and(|id| id.parse::<u64>().is_ok()),
            "known task ID was lost"
        );
        assert!(result["requestKey"].is_string());
    }
    broker.stop().await;
}

#[tokio::test]
async fn oversized_prompt_is_rejected_before_persistence_or_central_creation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    for (index, description, context) in [
        (1, "한".repeat(16_000), "글".repeat(16_000)),
        (2, "\u{0001}".repeat(16_000), "context".into()),
    ] {
        let mut body = request(&session, &format!("oversized-{index}"));
        body["arguments"]["description"] = json!(description);
        body["arguments"]["taskContext"] = json!(context);
        assert!(serde_json::to_vec(&body).unwrap().len() < 128 * 1024);
        let prompt = json!({"title":body["arguments"]["title"],"description":body["arguments"]["description"],
            "taskContext":body["arguments"]["taskContext"]}).to_string();
        let wire = json!({"type":"TASK_CREATED","event_id":"synthetic-event","payload":{
            "task_id":101,"team_id":1,"requester_agent_id":12,"receiver_agent_id":13,
            "prompt":prompt,"created_at":"2026-10-29T09:00:00"}});
        assert!(
            serde_json::to_vec(&wire).unwrap().len()
                > devlink_broker::protocol::MAX_CENTRAL_MESSAGE_BYTES
        );
        let (status, result) = broker.post("/v1/tasks/create", &body).await;
        assert_eq!(status, 400);
        assert_eq!(result["error"]["code"], "VALIDATION_FAILED");
    }
    assert!(remote.state.calls.lock().await.is_empty());
    assert!(!TaskRequests::path(&path).exists());
    let mut valid = request(&session, "escaped-but-deliverable");
    valid["arguments"]["description"] = json!("\"".repeat(16_000));
    valid["arguments"]["taskContext"] = json!("context");
    assert_eq!(broker.post("/v1/tasks/create", &valid).await.0, 200);
    broker.stop().await;
}

#[tokio::test]
async fn authenticated_creation_maps_actor_payload_ids_and_delivery_without_waiting_for_ack() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    let (status, result) = broker
        .post("/v1/tasks/create", &request(&session, "create-1"))
        .await;
    assert_eq!(status, 200);
    assert_eq!(result["data"]["taskId"], "101");
    assert_eq!(result["data"]["receiverAgentId"], "13");
    assert_eq!(result["data"]["delivery"]["status"], "ACK_PENDING");
    assert_eq!(result["requestKey"], result["data"]["requestKey"]);
    let sent = remote.state.calls.lock().await[0].clone();
    assert_eq!(sent["requesterAgentId"], 12);
    assert_eq!(sent["receiverAgentId"], 13);
    assert_ne!(sent["requestKey"], "create-1");
    let prompt: Value = serde_json::from_str(sent["prompt"].as_str().unwrap()).unwrap();
    assert_eq!(
        prompt,
        json!({"title":"로그인 수정","description":"오류 메시지 수정","taskContext":"작업 맥락"})
    );
    remote.state.mode.store(8, Ordering::SeqCst);
    let (status, unknown) = broker
        .post("/v1/tasks/create", &request(&session, "create-2"))
        .await;
    assert_eq!(status, 200);
    assert_eq!(unknown["data"]["status"], "PENDING");
    assert_eq!(unknown["data"]["delivery"]["status"], "DELIVERY_UNKNOWN");
    broker.stop().await;
}

#[tokio::test]
async fn repeated_request_survives_broker_restart_and_changed_payload_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    let body = request(&session, "stable-request");
    let (_, first) = broker.post("/v1/tasks/create", &body).await;
    let (_, second) = broker.post("/v1/tasks/create", &body).await;
    assert_eq!(first["requestKey"], second["requestKey"]);
    assert_eq!(second["data"]["replayed"], true);
    let mut conflict = body;
    conflict["arguments"]["description"] = json!("different");
    let (status, error) = broker.post("/v1/tasks/create", &conflict).await;
    assert_eq!(status, 409);
    assert_eq!(error["error"]["code"], "REQUEST_KEY_CONFLICT");
    assert_eq!(remote.state.calls.lock().await.len(), 2);
    broker.stop().await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("restarted-host", true).await;
    let (_, third) = broker
        .post("/v1/tasks/create", &request(&session, "stable-request"))
        .await;
    assert_eq!(first["requestKey"], third["requestKey"]);
    assert_eq!(third["data"]["taskId"], "101");
    assert_eq!(remote.state.saved.lock().await.len(), 1);
    broker.stop().await;
}

#[tokio::test]
async fn spoofed_unbound_and_replaced_sessions_never_reach_central() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let unbound = broker.claim("unbound", false).await;
    let (status, error) = broker
        .post("/v1/tasks/create", &request(&unbound, "unbound"))
        .await;
    assert_eq!(status, 409);
    assert_eq!(error["error"]["code"], "AGENT_UNREGISTERED");
    let session = broker.claim("host", true).await;
    for field in ["userId", "teamId", "agentId"] {
        let mut body = request(&session, field);
        body["context"][field] = json!("999");
        assert_eq!(broker.post("/v1/tasks/create", &body).await.0, 403);
    }
    let replacement = broker.claim("replacement", true).await;
    let (_, error) = broker
        .post("/v1/tasks/create", &request(&session, "old-session"))
        .await;
    assert_eq!(error["error"]["code"], "SESSION_REPLACED");
    let mut invalid = request(&replacement, "wrong-operation");
    invalid["operation"] = json!("complete_task");
    assert_eq!(broker.post("/v1/tasks/create", &invalid).await.0, 400);
    assert!(remote.state.calls.lock().await.is_empty());
    broker.stop().await;
}

#[tokio::test]
async fn timeout_is_unknown_without_retry_and_explicit_replay_recovers_saved_task() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    remote.state.mode.store(1, Ordering::SeqCst);
    let broker = Broker::start(&path, &remote, Duration::from_millis(100)).await;
    let session = broker.claim("host", true).await;
    let body = request(&session, "timed-out");
    let (status, error) = broker.post("/v1/tasks/create", &body).await;
    assert_eq!(status, 502);
    assert_eq!(error["error"]["code"], "CENTRAL_TIMEOUT");
    assert_eq!(error["error"]["outcome"], "unknown");
    assert!(error["requestKey"].is_string());
    sleep(Duration::from_millis(150)).await;
    assert_eq!(
        remote.state.calls.lock().await.len(),
        1,
        "transport retried a mutation"
    );
    remote.state.mode.store(0, Ordering::SeqCst);
    let (_, recovered) = broker.post("/v1/tasks/create", &body).await;
    assert_eq!(recovered["requestKey"], error["requestKey"]);
    assert_eq!(recovered["data"]["replayed"], true);
    assert_eq!(broker.central.status().await, ConnectionStatus::Connected);
    broker.stop().await;
}

#[tokio::test]
async fn central_denials_protocol_errors_and_redirects_keep_local_session_alive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    for (mode, expected) in [
        (2, "CENTRAL_UNAUTHENTICATED"),
        (3, "FORBIDDEN"),
        (4, "CENTRAL_UNAVAILABLE"),
        (5, "CENTRAL_PROTOCOL_ERROR"),
        (6, "CENTRAL_PROTOCOL_ERROR"),
        (7, "CENTRAL_PROTOCOL_ERROR"),
        (9, "CENTRAL_RESULT_UNKNOWN"),
    ] {
        remote.state.mode.store(mode, Ordering::SeqCst);
        let (_, error) = broker
            .post(
                "/v1/tasks/create",
                &request(&session, &format!("error-{mode}")),
            )
            .await;
        assert_eq!(error["error"]["code"], expected);
        if mode == 9 {
            assert!(error["error"]["taskId"].is_string());
        }
        assert_ne!(error["error"]["scope"], "local_auth");
        let (status, _) = broker
            .post(
                &format!(
                    "/v1/sessions/{}/heartbeat",
                    session["sessionId"].as_str().unwrap()
                ),
                &json!({"requestId":"renew","instanceId":"host"}),
            )
            .await;
        assert_eq!(status, 200);
    }
    assert_eq!(
        remote.trapped.load(Ordering::SeqCst),
        0,
        "redirect leaked a credential"
    );
    broker.stop().await;
}

#[tokio::test]
async fn released_session_waiting_for_duplicate_gate_is_not_sent_late() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    remote.state.mode.store(1, Ordering::SeqCst);
    let broker = Broker::start(&path, &remote, Duration::from_secs(2)).await;
    let session = broker.claim("host", true).await;
    let body = request(&session, "concurrent");
    let url = format!("{}/v1/tasks/create", broker.url);
    let first = broker
        .client
        .post(&url)
        .bearer_auth(LOCAL)
        .json(&body)
        .send();
    let second = broker
        .client
        .post(&url)
        .bearer_auth(LOCAL)
        .json(&body)
        .send();
    let release = async {
        remote.wait_calls(1).await;
        broker
            .client
            .delete(format!(
                "{}/v1/sessions/{}",
                broker.url,
                session["sessionId"].as_str().unwrap()
            ))
            .bearer_auth(LOCAL)
            .json(&json!({"requestId":"release","instanceId":"host"}))
            .send()
            .await
            .unwrap();
    };
    let (a, b, _) = tokio::join!(first, second, release);
    let statuses = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    assert!(statuses.contains(&200) && statuses.contains(&409));
    assert_eq!(remote.state.calls.lock().await.len(), 1);
    assert_eq!(remote.state.max_active.load(Ordering::SeqCst), 1);
    broker.stop().await;
}

#[tokio::test]
async fn bad_local_credentials_and_storage_failure_never_call_central() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_secs(1)).await;
    let session = broker.claim("host", true).await;
    let body = request(&session, "never-send");
    let response = broker
        .client
        .post(format!("{}/v1/tasks/create", broker.url))
        .bearer_auth(AGENT)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);
    let response = broker
        .client
        .post(format!("{}/v1/tasks/create", broker.url))
        .bearer_auth(LOCAL)
        .header("Origin", "https://example.test")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
    std::fs::create_dir(TaskRequests::path(&path)).unwrap();
    let (status, error) = broker.post("/v1/tasks/create", &body).await;
    assert_eq!(status, 503);
    assert_eq!(error["error"]["code"], "BROKER_STORAGE_FAILED");
    assert!(remote.state.calls.lock().await.is_empty());
    broker.stop().await;
}

// Opt-in: compile packages/mcp-server into a directory under rust/target and set
// DEVLINK_MCP_BUILD to that path. The harness uses its existing node_modules.
// Reuse the HTTP fixtures above; READY identity is prepared, not received over WS.
#[tokio::test]
#[ignore = "requires Node >=24 and current MCP build at DEVLINK_MCP_BUILD"]
async fn actual_mcp_tools_use_rust_sessions_and_mock_central() {
    let build = std::env::var_os("DEVLINK_MCP_BUILD").expect("set DEVLINK_MCP_BUILD");
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir_in(workspace.join("target")).unwrap();
    let path = dir.path().join("config.json");
    let remote = Central::start(&path).await;
    let broker = Broker::start(&path, &remote, Duration::from_millis(200)).await;
    for (scenario, mode, expected_calls) in [
        ("success", 0, 2),
        ("delivery-unknown", 8, 1),
        ("central-auth", 2, 1),
        ("timeout", 1, 1),
        ("local-auth", 0, 0),
    ] {
        remote.state.mode.store(mode, Ordering::SeqCst);
        let before = remote.state.calls.lock().await.len();
        let mut command = tokio::process::Command::new("node");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/mcp_task_creation.mjs"
            ))
            .arg(&build)
            .arg(&broker.url)
            .arg(scenario)
            .env("DEVLINK_TEST_LOCAL_TOKEN", LOCAL)
            .env("DEVLINK_TEST_AGENT_TOKEN", AGENT)
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let output = timeout(Duration::from_secs(20), command.output())
            .await
            .expect("MCP harness timed out")
            .expect("cannot start Node");
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["scenario"], scenario);
        assert_eq!(report["passed"], true);
        // The real MCP transport must not retry a central mutation implicitly.
        let calls = remote.state.calls.lock().await;
        assert_eq!(calls.len() - before, expected_calls, "{scenario}");
        for sent in &calls[before..] {
            assert_eq!(sent["requesterAgentId"], 12);
            assert_eq!(sent["receiverAgentId"], 13);
            let prompt: Value = serde_json::from_str(sent["prompt"].as_str().unwrap()).unwrap();
            assert_eq!(prompt["title"], "MCP integration");
            assert_eq!(prompt["description"], "실제 MCP 입력");
            assert_eq!(prompt["taskContext"], "mock only");
        }
        if scenario == "success" {
            assert_ne!(calls[before]["requestKey"], calls[before + 1]["requestKey"]);
            assert_eq!(remote.state.saved.lock().await.len(), 2);
        }
        println!("actual MCP: {scenario} passed ({expected_calls} central calls)");
    }
    broker.stop().await;
}
