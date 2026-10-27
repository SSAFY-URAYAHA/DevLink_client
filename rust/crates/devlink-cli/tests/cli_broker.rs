use devlink_broker::{
    central::ConnectionStatus,
    config::{self, Config},
    local::{Health, Runtime},
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
        let dir = tempfile::Builder::new()
            .prefix("devlink test 한글 ")
            .tempdir()
            .unwrap();
        let path = dir.path().join("config.json");
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
