use devlink_broker::{
    central::ConnectionStatus,
    config::{self, Config},
    local::{self, Health, Runtime},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    path::PathBuf,
    process::{Child, Command, Output},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const TOKEN: &str = "test-only-secret-never-print";

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::with_config_name("config.json")
    }
    fn with_config_name(name: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("devlink test 한글 ")
            .tempdir()
            .unwrap();
        let path = dir.path().join(name);
        config::init(&path).unwrap();
        let mut config = Config::load(&path).unwrap();
        config.local_port = 0;
        config.reconnect_initial_ms = 50;
        config.reconnect_max_ms = 100;
        config.auth_timeout_ms = 200;
        config::atomic_json(&path, &config).unwrap();
        Self { _dir: dir, path }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_devlink"));
        command
            .arg("--config")
            .arg(&self.path)
            .args(args)
            .env("DEVLINK_TOKEN", TOKEN);
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(!text(&output).contains(TOKEN), "secret leaked");
        output
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "{:?}: {}", args, text(&output));
        text(&output)
    }
    fn configure(&self, update: impl FnOnce(&mut Config)) {
        let mut config = Config::load(&self.path).unwrap();
        update(&mut config);
        config::atomic_json(&self.path, &config).unwrap();
    }
    async fn health(&self) -> Health {
        Runtime::load(&self.path).unwrap().health().await.unwrap()
    }
    async fn wait_status(&self, expected: ConnectionStatus) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let health = self.health().await;
            if health.central_server == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Expected {expected:?}, got {:?}",
                health.central_server
            );
            sleep(Duration::from_millis(30)).await;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command(&["stop"]).output();
    }
}
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

struct OwnedProcess(Child);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn config_create_show_update_and_validation() {
    let f = Fixture::new();
    assert!(f.ok(&["init"]).contains("already exists"));
    assert!(f.ok(&["config"]).contains("serverUrl"));
    f.ok(&["config", "set", "agent", "test-agent"]);
    f.ok(&["config", "set", "autoReconnect", "false"]);
    assert_eq!(Config::load(&f.path).unwrap().agent, "test-agent");
    assert!(!Config::load(&f.path).unwrap().auto_reconnect);
    let before = std::fs::read(&f.path).unwrap();
    for args in [
        ["config", "set", "localPort", "65536"],
        ["config", "set", "autoReconnect", "123"],
        ["config", "set", "serverUrl", "https://example.com"],
        ["config", "set", "serverUrl", "ws://user:password@localhost"],
        ["config", "set", "reconnectInitialMs", "0"],
        ["config", "set", "unknown", "value"],
    ] {
        assert!(!f.run(&args).status.success());
    }
    assert_eq!(std::fs::read(&f.path).unwrap(), before);
    assert!(!f.run(&["not-a-command"]).status.success());
    assert!(f.ok(&["--help"]).contains("start"));
}

#[test]
fn config_names_matching_internal_files_are_preserved() {
    for name in [
        "runtime.json",
        "Runtime.JSON",
        "broker.lock",
        "Broker.LOCK",
        "control.lock",
        "Control.LOCK",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture {
            path: dir.path().join(name),
            _dir: dir,
        };
        assert!(f.ok(&["init"]).contains("Configuration created"));
        assert!(f.ok(&["init"]).contains("already exists"));
        f.ok(&["config", "set", "agent", "custom-agent"]);
        let before = std::fs::read(&f.path).unwrap();
        let runtime = Runtime {
            api_version: local::API_VERSION,
            pid: std::process::id(),
            port: 43127,
            instance_id: "stale-instance".into(),
            control_token: "test-control-token".into(),
        };
        local::publish(&f.path, &runtime).unwrap();
        assert!(f.ok(&["status"]).contains("Stopped"));
        assert!(!Runtime::path(&f.path).exists());
        assert_eq!(std::fs::read(&f.path).unwrap(), before, "{name}: status");

        local::publish(&f.path, &runtime).unwrap();
        assert!(f.ok(&["stop"]).contains("already stopped"));
        assert!(!Runtime::path(&f.path).exists());
        assert_eq!(std::fs::read(&f.path).unwrap(), before, "{name}: stop");

        local::publish(&f.path, &runtime).unwrap();
        let output = f
            .command(&["start"])
            .env_remove("DEVLINK_TOKEN")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(text(&output).contains("DEVLINK_TOKEN"));
        assert!(!Runtime::path(&f.path).exists());
        assert_eq!(std::fs::read(&f.path).unwrap(), before, "{name}: start");
        assert!(
            config::state_directory(&f.path)
                .join("broker.lock")
                .exists()
        );
        assert!(
            config::state_directory(&f.path)
                .join("control.lock")
                .exists()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_names_matching_internal_files_survive_broker_lifecycle() {
    for name in [
        "runtime.json",
        "Runtime.JSON",
        "broker.lock",
        "control.lock",
    ] {
        let f = Fixture::with_config_name(name);
        let before = std::fs::read(&f.path).unwrap();
        f.ok(&["start"]);
        assert!(f.ok(&["status"]).contains("Running"));
        assert_eq!(std::fs::read(&f.path).unwrap(), before, "{name}: running");
        f.ok(&["stop"]);
        assert!(!Runtime::path(&f.path).exists());
        assert_eq!(std::fs::read(&f.path).unwrap(), before, "{name}: stopped");
    }
}

#[test]
fn malformed_config_does_not_echo_secrets() {
    let f = Fixture::new();
    std::fs::write(&f.path, format!(r#"{{"token":"{TOKEN}"}}"#)).unwrap();
    assert!(!f.run(&["config"]).status.success());
    std::fs::write(
        &f.path,
        format!(r#"{{"serverUrl":"ws://{TOKEN}@localhost","agent":"codex","autoReconnect":true}}"#),
    )
    .unwrap();
    assert!(!f.run(&["config"]).status.success());
    std::fs::write(
        &f.path,
        format!(r#"{{"serverUrl":"ws://localhost","agent":true,"autoReconnect":"{TOKEN}"}}"#),
    )
    .unwrap();
    assert!(!f.run(&["config"]).status.success());
}

#[test]
fn typescript_config_shape_is_accepted() {
    let config: Config = serde_json::from_str(
        r#"{"serverUrl":"ws://localhost:8080","agent":"codex","autoReconnect":true}"#,
    )
    .unwrap();
    config.validate().unwrap();
    assert_eq!(config.local_port, 43127);
}

#[test]
fn missing_token_is_reported_without_spawning() {
    let f = Fixture::new();
    let output = f
        .command(&["start"])
        .env_remove("DEVLINK_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(text(&output).contains("DEVLINK_TOKEN"));
    assert!(!Runtime::path(&f.path).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_lifecycle_duplicate_and_local_authentication() {
    let f = Fixture::new();
    f.ok(&["start"]); // Parent CLI has exited, broker must remain alive.
    let pid = f.health().await.pid;
    assert!(f.ok(&["status"]).contains("Running"));
    assert!(f.ok(&["start"]).contains("already running"));
    assert_eq!(f.health().await.pid, pid);
    let runtime = Runtime::load(&f.path).unwrap();
    let client = runtime.client().unwrap();
    assert_eq!(
        client
            .get(runtime.url("/health"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(runtime.url("/shutdown"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(runtime.url("/shutdown"))
            .bearer_auth(&runtime.control_token)
            .header("Origin", "https://example.org")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!f.ok(&["status"]).contains(&runtime.control_token));
    f.ok(&["stop"]);
    assert!(!Runtime::path(&f.path).exists());
    assert!(f.ok(&["status"]).contains("Stopped"));
    assert!(f.ok(&["stop"]).contains("already stopped"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_start_cannot_spawn_two_brokers() {
    let f = Fixture::new();
    let mut first = f
        .command(&["start"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = f.run(&["start"]);
    let first_exit = first.wait().unwrap();
    assert!(first_exit.success() || second.status.success());
    let pid = f.health().await.pid;
    f.ok(&["start"]);
    assert_eq!(f.health().await.pid, pid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_releases_os_lock_and_stale_runtime_is_recovered() {
    let f = Fixture::new();
    let mut child = OwnedProcess(f.command(&["broker"]).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(runtime) = Runtime::load(&f.path)
            && runtime.health().await.is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(30)).await;
    }
    let old_instance = Runtime::load(&f.path).unwrap().instance_id;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(Runtime::path(&f.path).exists());
    assert!(f.ok(&["status"]).contains("Stopped"));
    assert!(!Runtime::path(&f.path).exists());
    f.ok(&["start"]);
    assert_ne!(f.health().await.instance_id, old_instance);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupied_port_fails_and_releases_lock() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.configure(|config| config.local_port = listener.local_addr().unwrap().port());
    assert!(!f.run(&["start"]).status.success());
    assert!(!devlink_broker::local::running(&f.path).unwrap());
    drop(listener);
    f.ok(&["start"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unhealthy_live_broker_is_not_reported_as_stopped() {
    let f = Fixture::new();
    f.ok(&["start"]);
    let mut runtime = Runtime::load(&f.path).unwrap();
    let original = runtime.control_token.clone();
    runtime.control_token = "wrong-token".into();
    config::atomic_json(&Runtime::path(&f.path), &runtime).unwrap();
    let output = f.run(&["status"]);
    runtime.control_token = original;
    config::atomic_json(&Runtime::path(&f.path), &runtime).unwrap();
    assert!(!output.status.success());
    assert!(!text(&output).contains("Stopped"));
}

#[derive(Clone, Copy)]
enum Behavior {
    Authenticate,
    Deny,
    Silent,
    InvalidJson,
    CloseUnauthorized,
}
struct Mock {
    address: std::net::SocketAddr,
    task: JoinHandle<()>,
}
impl Mock {
    async fn start(address: &str, behavior: Behavior) -> Self {
        let listener = TcpListener::bind(address).await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                clients.spawn(async move {
                    let Ok(mut socket) = accept_async(stream).await else {
                        return;
                    };
                    let Some(Ok(Message::Text(text))) = socket.next().await else {
                        return;
                    };
                    let auth: serde_json::Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(auth["type"], "auth");
                    assert_eq!(auth["token"], TOKEN);
                    let message = match behavior {
                        Behavior::Authenticate => r#"{"type":"auth_ok"}"#,
                        Behavior::Deny => r#"{"type":"auth_error"}"#,
                        Behavior::Silent => {
                            std::future::pending::<()>().await;
                            return;
                        }
                        Behavior::InvalidJson => "malformed-json",
                        Behavior::CloseUnauthorized => {
                            let _ = socket
                                .close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                    code: 4001.into(),
                                    reason: "Unauthorized".into(),
                                }))
                                .await;
                            return;
                        }
                    };
                    if socket.send(Message::Text(message.into())).await.is_err() {
                        return;
                    }
                    while let Some(Ok(message)) = socket.next().await {
                        match message {
                            Message::Ping(payload) => {
                                if socket.send(Message::Pong(payload)).await.is_err() {
                                    break;
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                });
                while clients.try_join_next().is_some() {}
            }
        });
        Self { address, task }
    }
    async fn stop(self) {
        self.task.abort();
        // Await cancellation so both listening socket and accepted sockets have been dropped.
        let _ = self.task.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_auth_disconnect_restart_and_reconnect() {
    let server = Mock::start("127.0.0.1:0", Behavior::Authenticate).await;
    let f = Fixture::new();
    f.configure(|config| config.server_url = format!("ws://{}", server.address));
    f.ok(&["start"]);
    f.wait_status(ConnectionStatus::Connected).await;
    assert!(f.ok(&["status"]).contains("Connected"));
    let address = server.address;
    server.stop().await;
    f.wait_status(ConnectionStatus::Reconnecting).await;
    let server = Mock::start(&address.to_string(), Behavior::Authenticate).await;
    f.wait_status(ConnectionStatus::Connected).await;
    f.ok(&["stop"]);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authentication_rejection_and_4001_stop_retries() {
    for behavior in [Behavior::Deny, Behavior::CloseUnauthorized] {
        let server = Mock::start("127.0.0.1:0", behavior).await;
        let f = Fixture::new();
        f.configure(|config| config.server_url = format!("ws://{}", server.address));
        f.ok(&["start"]);
        f.wait_status(ConnectionStatus::AuthFailed).await;
        let address = server.address;
        server.stop().await;
        let server = Mock::start(&address.to_string(), Behavior::Authenticate).await;
        sleep(Duration::from_millis(400)).await;
        assert_eq!(
            f.health().await.central_server,
            ConnectionStatus::AuthFailed
        );
        f.ok(&["stop"]);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_or_malformed_auth_times_out_without_reconnect_when_disabled() {
    for behavior in [Behavior::Silent, Behavior::InvalidJson] {
        let server = Mock::start("127.0.0.1:0", behavior).await;
        let f = Fixture::new();
        f.configure(|config| {
            config.server_url = format!("ws://{}", server.address);
            config.auto_reconnect = false;
        });
        f.ok(&["start"]);
        f.wait_status(ConnectionStatus::Disconnected).await;
        f.ok(&["stop"]);
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_server_and_stop_during_backoff() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let f = Fixture::new();
    f.configure(|config| {
        config.server_url = format!("ws://{address}");
        config.reconnect_initial_ms = 30000;
        config.reconnect_max_ms = 30000;
    });
    f.ok(&["start"]);
    f.wait_status(ConnectionStatus::Reconnecting).await;
    let started = Instant::now();
    f.ok(&["stop"]);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_local_token_survives_normal_restart() {
    // 1. 기존 테스트 fixture를 활용해 임시 설정을 만든다.
    let f = Fixture::new();

    // 2. 사용 가능한 포트를 선택해 두 번의 실행에 동일하게 사용한다.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);

    f.configure(|config| {
        config.local_port = port;
    });

    // 3. 실제 CLI start로 브로커를 시작한다.
    f.ok(&["start"]);

    // 4. Rust가 발행한 MCP 자격에서 brokerUrl/localToken을 메모리에 보관한다.
    //    runtime의 instanceId도 보관한다.
    let creds1 = devlink_broker::credentials::load(&f.path).unwrap();
    let token1 = creds1.local_token;
    let broker_url1 = creds1.broker_url;

    let runtime1 = Runtime::load(&f.path).unwrap();
    let instance_id1 = runtime1.instance_id;

    // 5. 실제 CLI stop으로 종료한다.
    f.ok(&["stop"]);

    // 6. MCP 자격 파일이 남아 있고 토큰이 유지되는지 확인한다.
    let cred_path = devlink_broker::credentials::path(&f.path);
    assert!(
        cred_path.exists(),
        "MCP credential file must remain after stop"
    );
    let creds_after_stop = devlink_broker::credentials::load(&f.path).unwrap();
    assert!(
        creds_after_stop.local_token == token1,
        "MCP token changed after stop"
    );

    // 7. 같은 설정으로 다시 start한다.
    f.ok(&["start"]);

    // 8. 다음을 확인한다.
    let creds2 = devlink_broker::credentials::load(&f.path).unwrap();
    // - localToken 동일 (비밀값 노출 방지를 위해 assert! 사용)
    assert!(
        creds2.local_token == token1,
        "MCP token changed after restart"
    );
    // - brokerUrl 동일
    assert!(
        creds2.broker_url == broker_url1,
        "MCP brokerUrl changed after restart"
    );
    // - runtime instanceId 변경
    let runtime2 = Runtime::load(&f.path).unwrap();
    assert!(
        runtime2.instance_id != instance_id1,
        "Runtime instanceId must change after restart"
    );

    // 9. stop으로 종료하고 테스트 프로세스를 정리한다.
    f.ok(&["stop"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_local_token_survives_crash_restart() {
    // 1. 기존 Fixture와 OwnedProcess를 사용한다.
    let f = Fixture::new();

    // 2. 임시 설정에 사용 가능한 고정 포트를 지정한다.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);

    f.configure(|config| {
        config.local_port = port;
    });

    // 3. broker를 직접 자식 프로세스로 실행하고 Health 준비를 기다린다.
    let mut child = OwnedProcess(f.command(&["broker"]).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(runtime) = Runtime::load(&f.path)
            && runtime.health().await.is_ok()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Broker failed to become healthy before deadline"
        );
        sleep(Duration::from_millis(30)).await;
    }

    // 4. MCP 자격의 localToken·brokerUrl과 runtime instanceId를 보관한다.
    let creds1 = devlink_broker::credentials::load(&f.path).unwrap();
    let token1 = creds1.local_token;
    let broker_url1 = creds1.broker_url;

    let runtime1 = Runtime::load(&f.path).unwrap();
    let instance_id1 = runtime1.instance_id;

    // 5. 자신이 생성한 자식 프로세스를 kill 후 wait한다. (stop 호출 없음)
    child.0.kill().unwrap();
    child.0.wait().unwrap();

    // 6. 강제 종료 직후 MCP 자격 파일과 토큰이 유지되는지 확인한다.
    let cred_path = devlink_broker::credentials::path(&f.path);
    assert!(
        cred_path.exists(),
        "MCP credential file must remain after crash"
    );
    let creds_after_crash = devlink_broker::credentials::load(&f.path).unwrap();
    assert!(
        creds_after_crash.local_token == token1,
        "MCP token changed after crash"
    );

    // 7. 같은 설정으로 실제 CLI start를 실행한다.
    f.ok(&["start"]);

    // 8. localToken·brokerUrl은 동일하고 instanceId는 변경됐는지 확인한다.
    let creds2 = devlink_broker::credentials::load(&f.path).unwrap();
    assert!(
        creds2.local_token == token1,
        "MCP token changed after crash restart"
    );
    assert!(
        creds2.broker_url == broker_url1,
        "MCP brokerUrl changed after crash restart"
    );
    let runtime2 = Runtime::load(&f.path).unwrap();
    assert!(
        runtime2.instance_id != instance_id1,
        "Runtime instanceId must change after crash restart"
    );

    // 9. CLI stop으로 종료한다.
    f.ok(&["stop"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_token_allows_health_but_denies_shutdown() {
    // 1. 기존 Fixture로 임시 브로커를 start한다.
    let f = Fixture::new();
    f.ok(&["start"]);

    // 2. 실제 MCP 자격 파일에서 localToken을 읽는다.
    let creds = devlink_broker::credentials::load(&f.path).unwrap();
    let local_token = creds.local_token;

    // 3. localToken과 runtime.controlToken이 서로 다른지 확인한다.
    let runtime = Runtime::load(&f.path).unwrap();
    assert!(
        local_token != runtime.control_token,
        "MCP token and control token must differ"
    );

    let client = runtime.client().unwrap();

    // 4. MCP 토큰으로 GET /health → 200 확인.
    let health_res = client
        .get(runtime.url("/health"))
        .bearer_auth(&local_token)
        .send()
        .await
        .unwrap();
    assert_eq!(health_res.status(), 200);

    // 5. MCP 토큰으로 POST /shutdown → 현재 구현 기준 401 확인.
    let shutdown_res = client
        .post(runtime.url("/shutdown"))
        .bearer_auth(&local_token)
        .send()
        .await
        .unwrap();
    assert_eq!(shutdown_res.status(), 401);

    // 6. 종료 거부 후 Health를 다시 확인해 같은 instanceId의 브로커가 계속 실행 중인지 확인한다.
    let current_health = f.health().await;
    assert_eq!(current_health.instance_id, runtime.instance_id);

    // 7. MCP 토큰과 Origin 헤더로 GET /health → 401 확인.
    let origin_health_res = client
        .get(runtime.url("/health"))
        .bearer_auth(&local_token)
        .header("Origin", "https://example.org")
        .send()
        .await
        .unwrap();
    assert_eq!(origin_health_res.status(), 401);

    // 8. 실제 CLI stop으로 정상 종료한다.
    f.ok(&["stop"]);
    assert!(!Runtime::path(&f.path).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_token_rotation_requires_stopped_broker() {
    // 1. 임시 브로커를 start하고 MCP 토큰과 instanceId를 보관한다.
    let f = Fixture::new();
    f.ok(&["start"]);
    let creds1 = devlink_broker::credentials::load(&f.path).unwrap();
    let initial_token = creds1.local_token;
    let initial_instance_id = f.health().await.instance_id;

    // 2. 실행 중 CLI의 mcp rotate를 호출한다.
    //    실패 종료 코드, 기존 토큰 유지, 같은 브로커의 정상 Health를 확인한다.
    let rotate_running = f.run(&["mcp", "rotate"]);
    assert!(!rotate_running.status.success());
    let creds_after_failed_rotate = devlink_broker::credentials::load(&f.path).unwrap();
    assert!(
        creds_after_failed_rotate.local_token == initial_token,
        "Token must remain unchanged when rotation is rejected"
    );
    let health_after_failed_rotate = f.health().await;
    assert_eq!(health_after_failed_rotate.instance_id, initial_instance_id);

    // 3. CLI stop으로 종료한다.
    f.ok(&["stop"]);

    // 4. CLI mcp rotate를 호출해 성공과 토큰 변경을 확인한다.
    f.ok(&["mcp", "rotate"]);
    let creds_rotated = devlink_broker::credentials::load(&f.path).unwrap();
    let rotated_token = creds_rotated.local_token;
    assert!(
        rotated_token != initial_token,
        "Token must change after rotation"
    );

    // 5. 다시 start하고 회전한 토큰이 유지되는지 확인한다.
    f.ok(&["start"]);
    let creds_restarted = devlink_broker::credentials::load(&f.path).unwrap();
    assert!(
        creds_restarted.local_token == rotated_token,
        "Rotated token must survive restart"
    );

    // 6. /health에 회전한 토큰으로 요청하면 200,
    //    이전 토큰으로 요청하면 401인지 확인한다.
    let runtime = Runtime::load(&f.path).unwrap();
    let client = runtime.client().unwrap();

    let res_rotated = client
        .get(runtime.url("/health"))
        .bearer_auth(&rotated_token)
        .send()
        .await
        .unwrap();
    assert_eq!(res_rotated.status(), 200);

    let res_initial = client
        .get(runtime.url("/health"))
        .bearer_auth(&initial_token)
        .send()
        .await
        .unwrap();
    assert_eq!(res_initial.status(), 401);

    // 7. CLI stop으로 종료한다.
    f.ok(&["stop"]);
    assert!(!Runtime::path(&f.path).exists());
}
