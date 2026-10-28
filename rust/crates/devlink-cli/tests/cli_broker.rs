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
        ["config", "set", "localPort", "80"],
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

fn ensure_mcp_reader_verifier() -> PathBuf {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mcp_root = manifest_dir.join("../../../packages/mcp-server");
    let reader_dir = manifest_dir.join("../../target/mcp-reader-check");

    // Remove previous src, dist, and verify.mjs so stale sources or build outputs never mix in
    let _ = std::fs::remove_dir_all(reader_dir.join("src"));
    let _ = std::fs::remove_dir_all(reader_dir.join("dist"));
    let _ = std::fs::remove_file(reader_dir.join("verify.mjs"));

    let broker_src = reader_dir.join("src/broker");
    std::fs::create_dir_all(&broker_src).expect("Failed to create mcp-reader-check/src/broker");

    for file in ["package.json", "package-lock.json", "tsconfig.json"] {
        std::fs::copy(mcp_root.join(file), reader_dir.join(file))
            .unwrap_or_else(|e| panic!("Failed to copy {file} from packages/mcp-server: {e}"));
    }
    for entry in std::fs::read_dir(mcp_root.join("src/broker"))
        .expect("Failed to read packages/mcp-server/src/broker")
    {
        let entry = entry.expect("Failed to read directory entry in src/broker");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("ts") {
            let file_name = entry.file_name();
            std::fs::copy(&path, broker_src.join(&file_name)).unwrap_or_else(|e| {
                panic!("Failed to copy src/broker/{}: {e}", file_name.display())
            });
        }
    }

    if !reader_dir.join("node_modules/zod").exists()
        || !reader_dir.join("node_modules/typescript/bin/tsc").exists()
    {
        let npm_bin = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let out = Command::new(npm_bin)
            .args(["ci", "--prefer-offline", "--no-audit", "--no-fund"])
            .current_dir(&reader_dir)
            .output()
            .expect("Failed to execute npm ci for MCP reader verification");
        assert!(
            out.status.success(),
            "npm ci failed in mcp-reader-check: {}",
            text(&out)
        );
    }

    let out = Command::new("node")
        .arg(reader_dir.join("node_modules/typescript/bin/tsc"))
        .args([
            "--ignoreConfig",
            "--target",
            "ES2022",
            "--module",
            "NodeNext",
            "--moduleResolution",
            "NodeNext",
            "--rootDir",
            "src",
            "--outDir",
            "dist",
            "--strict",
            "--noUncheckedIndexedAccess",
            "--exactOptionalPropertyTypes",
            "--verbatimModuleSyntax",
            "--forceConsistentCasingInFileNames",
            "--types",
            "node",
            "src/broker/settings.ts",
        ])
        .current_dir(&reader_dir)
        .output()
        .expect("Failed to execute tsc for MCP reader verification");
    assert!(
        out.status.success(),
        "tsc compilation of MCP reader failed: {}",
        text(&out)
    );
    assert!(
        reader_dir.join("dist/broker/settings.js").exists(),
        "dist/broker/settings.js was not generated by tsc"
    );

    let verify_script = reader_dir.join("verify.mjs");
    const VERIFY_MJS: &str = r#"import { readBrokerSettings } from './dist/broker/settings.js';

const credPath = process.argv[2];
if (!credPath) {
  console.error('ERROR: Missing credential file path');
  process.exit(1);
}

try {
  const settings = await readBrokerSettings(credPath);
  if (settings.schemaVersion === 1) {
    console.log('MCP_CREDENTIAL_READ_OK');
    process.exit(0);
  } else {
    console.error('ERROR: Unexpected schemaVersion');
    process.exit(2);
  }
} catch (err) {
  const errCode = err?.code || err?.message || 'UNKNOWN_ERROR';
  console.error(`ERROR: ${errCode}`);
  process.exit(3);
}
"#;
    std::fs::write(&verify_script, VERIFY_MJS).expect("Failed to write verify.mjs");
    assert!(
        verify_script.exists(),
        "MCP reader verify.mjs was not prepared"
    );
    verify_script
}

#[test]
fn cli_and_mcp_reader_enforce_consistent_port_rules() {
    let f = Fixture::new();
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.path).unwrap()).unwrap();
    raw["localPort"] = serde_json::json!(80);
    std::fs::write(&f.path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

    // Existing port 80 config must be rejected by config show and start
    assert!(!f.run(&["config"]).status.success());
    assert!(!f.run(&["start"]).status.success());
    // Updating an unrelated field while localPort remains 80 must still be rejected
    assert!(
        !f.run(&["config", "set", "agent", "other-agent"])
            .status
            .success()
    );
    assert!(
        devlink_broker::credentials::publish(&f.path, "http://127.0.0.1:80").is_err(),
        "credentials::publish must reject port 80"
    );
    assert!(!devlink_broker::credentials::path(&f.path).exists());
    assert!(!Runtime::path(&f.path).exists());

    // CLI config set must be able to repair an existing localPort: 80 config without manual editing
    f.ok(&["config", "set", "localPort", "43127"]);
    assert_eq!(Config::load(&f.path).unwrap().local_port, 43127);
    assert!(f.ok(&["config"]).contains("43127"));

    let verify_script = ensure_mcp_reader_verifier();
    let cred_path = devlink_broker::credentials::path(&f.path);

    // Supported ports (1, 79, 81, 43127, 65535) must pass both Rust load and real MCP reader
    for port in [1u16, 79, 81, 43127, 65535] {
        let url = format!("http://127.0.0.1:{port}");
        let creds = devlink_broker::credentials::publish(&f.path, &url)
            .unwrap_or_else(|_| panic!("publish must succeed for port {port}"));
        let loaded = devlink_broker::credentials::load(&f.path)
            .unwrap_or_else(|_| panic!("load must succeed for port {port}"));
        assert!(
            loaded == creds,
            "loaded credentials must match for port {port}"
        );

        let out = Command::new("node")
            .arg(&verify_script)
            .arg(&cred_path)
            .output()
            .expect("Failed to run node verify.mjs");
        assert!(
            out.status.success() && text(&out).contains("MCP_CREDENTIAL_READ_OK"),
            "MCP reader rejected supported port {port}: {}",
            text(&out)
        );
    }

    let write_broker_url = |url: &str| {
        let mut cred_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&cred_path).unwrap()).unwrap();
        cred_json["brokerUrl"] = serde_json::json!(url);
        let mut bytes = serde_json::to_vec_pretty(&cred_json).unwrap();
        bytes.push(b'\n');
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&cred_path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    };

    // Malformed port strings ("+81", leading zero "081", "0", "65536") must be rejected by both
    // Rust load and the real MCP reader, and repaired by `devlink mcp rotate`
    for bad_url in [
        "http://127.0.0.1:+81",
        "http://127.0.0.1:081",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
    ] {
        write_broker_url(bad_url);

        assert!(
            devlink_broker::credentials::load(&f.path).is_err(),
            "credentials::load must reject malformed brokerUrl {bad_url}"
        );
        let bad_out = Command::new("node")
            .arg(&verify_script)
            .arg(&cred_path)
            .output()
            .expect("Failed to run node verify.mjs");
        assert!(
            !bad_out.status.success(),
            "MCP reader must reject malformed brokerUrl {bad_url}"
        );

        // Rotate via CLI must repair brokerUrl to the valid configured default (43127)
        f.ok(&["mcp", "rotate"]);
        let repaired = devlink_broker::credentials::load(&f.path)
            .expect("credentials::load must succeed after rotating corrupted file");
        assert_eq!(repaired.broker_url, "http://127.0.0.1:43127");

        let ok_out = Command::new("node")
            .arg(&verify_script)
            .arg(&cred_path)
            .output()
            .expect("Failed to run node verify.mjs after rotate");
        assert!(
            ok_out.status.success() && text(&ok_out).contains("MCP_CREDENTIAL_READ_OK"),
            "MCP reader rejected rotated credential after repairing {bad_url}: {}",
            text(&ok_out)
        );
    }

    // Explicit port 80 is accepted by current MCP credentialSchema, while Rust policy rejects it;
    // rotating a port 80 file via CLI must still recover it to the configured default (43127)
    write_broker_url("http://127.0.0.1:80");
    assert!(
        devlink_broker::credentials::load(&f.path).is_err(),
        "Rust credentials::load must reject port 80 per Rust policy"
    );
    let port80_reader_out = Command::new("node")
        .arg(&verify_script)
        .arg(&cred_path)
        .output()
        .expect("Failed to run node verify.mjs on port 80");
    assert!(
        port80_reader_out.status.success()
            && text(&port80_reader_out).contains("MCP_CREDENTIAL_READ_OK"),
        "Current MCP reader should accept explicit port 80 literal: {}",
        text(&port80_reader_out)
    );

    f.ok(&["mcp", "rotate"]);
    let repaired_from_80 = devlink_broker::credentials::load(&f.path)
        .expect("credentials::load must succeed after rotating port 80 file");
    assert_eq!(repaired_from_80.broker_url, "http://127.0.0.1:43127");
    let ok_out_80 = Command::new("node")
        .arg(&verify_script)
        .arg(&cred_path)
        .output()
        .expect("Failed to run node verify.mjs after rotating port 80 file");
    assert!(
        ok_out_80.status.success() && text(&ok_out_80).contains("MCP_CREDENTIAL_READ_OK"),
        "MCP reader rejected rotated credential after repairing port 80: {}",
        text(&ok_out_80)
    );
}

#[cfg(windows)]
#[test]
fn cli_rejects_junction_state_dir_and_preserves_target_files() {
    use std::os::windows::process::CommandExt;

    fn get_sddl(path: &std::path::Path) -> String {
        const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$p = $env:DEVLINK_SDDL_PATH
$acl = if ([System.IO.Directory]::Exists($p)) { [System.IO.Directory]::GetAccessControl($p) } else { [System.IO.File]::GetAccessControl($p) }
[Console]::Write($acl.GetSecurityDescriptorSddlForm('All'))
"#;
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .env("DEVLINK_SDDL_PATH", path)
            .stdin(std::process::Stdio::null())
            .creation_flags(0x08000000)
            .output()
            .expect("Failed to get SDDL");
        assert!(output.status.success(), "powershell SDDL query failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    let temp_root = tempfile::tempdir().unwrap();
    let target_dir = temp_root.path().join("target_dir");
    std::fs::create_dir_all(&target_dir).unwrap();

    let target_runtime = target_dir.join("runtime.json");
    let target_sentinel = target_dir.join("sentinel.txt");
    std::fs::write(&target_runtime, b"{\"existing\":\"runtime\"}").unwrap();
    std::fs::write(&target_sentinel, b"original-content").unwrap();

    let target_dir_sddl_before = get_sddl(&target_dir);
    let runtime_sddl_before = get_sddl(&target_runtime);
    let sentinel_sddl_before = get_sddl(&target_sentinel);

    let config_dir = temp_root.path().join("config_dir");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.json");
    config::atomic_json(&config_path, &Config::default()).unwrap();

    let junction_path = config::state_directory(&config_path);
    let status = std::process::Command::new("cmd")
        .args([
            "/c",
            "mklink",
            "/J",
            junction_path.to_str().unwrap(),
            target_dir.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::null())
        .creation_flags(0x08000000)
        .status()
        .expect("mklink /J execution failed");
    assert!(status.success(), "Failed to create directory junction");

    struct JunctionCleanup(PathBuf);
    impl Drop for JunctionCleanup {
        fn drop(&mut self) {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("cmd")
                .args(["/c", "rmdir", self.0.to_str().unwrap()])
                .stdin(std::process::Stdio::null())
                .creation_flags(0x08000000)
                .status();
        }
    }
    let _cleanup = JunctionCleanup(junction_path);

    let f = Fixture {
        _dir: temp_root,
        path: config_path,
    };

    for args in [
        &["start"][..],
        &["status"][..],
        &["stop"][..],
        &["mcp", "rotate"][..],
        &["broker"][..],
    ] {
        let out = f.run(args);
        assert!(
            !out.status.success(),
            "CLI command {args:?} must fail when .devlink-runtime is a junction"
        );
    }

    assert!(
        target_runtime.exists(),
        "existing runtime.json in junction target must not be deleted"
    );
    assert_eq!(
        std::fs::read(&target_runtime).unwrap(),
        b"{\"existing\":\"runtime\"}",
        "existing runtime.json content in junction target must be preserved"
    );
    assert_eq!(
        std::fs::read(&target_sentinel).unwrap(),
        b"original-content",
        "existing sentinel.txt content in junction target must be preserved"
    );
    assert!(
        !target_dir.join("control.lock").exists(),
        "control.lock must not be created in junction target"
    );
    assert!(
        !target_dir.join("broker.lock").exists(),
        "broker.lock must not be created in junction target"
    );
    assert!(
        !target_dir.join("broker.json").exists(),
        "broker.json must not be created in junction target"
    );

    assert_eq!(
        get_sddl(&target_dir),
        target_dir_sddl_before,
        "target directory permissions must remain untouched"
    );
    assert_eq!(
        get_sddl(&target_runtime),
        runtime_sddl_before,
        "existing runtime.json permissions must remain untouched"
    );
    assert_eq!(
        get_sddl(&target_sentinel),
        sentinel_sddl_before,
        "existing sentinel.txt permissions must remain untouched"
    );
}
