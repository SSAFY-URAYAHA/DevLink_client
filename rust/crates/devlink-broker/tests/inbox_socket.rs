use devlink_broker::{
    central::{self, CentralState, ConnectionStatus},
    config::{self, Config},
    inbox::Inbox,
    protocol::{ClientMessage, HeartbeatPing, ServerMessage, TaskAckPayload, TaskCreatedEvent},
};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::RwLock,
    time::timeout,
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use tokio_util::sync::CancellationToken;

fn event() -> TaskCreatedEvent {
    serde_json::from_value(
        serde_json::json!({"type":"TASK_CREATED","event_id":"event-private-100",
        "payload":{"task_id":100,"team_id":3,"requester_agent_id":11,"receiver_agent_id":22,
            "prompt":"명시적 사용자 수락 전 실행 금지","created_at":"2026-10-29T12:00:00"}}),
    )
    .unwrap()
}

async fn ready(socket: &mut WebSocketStream<TcpStream>, connection: usize) {
    socket.send(Message::Text(serde_json::json!({"type":"BROKER_READY","payload":{"connection_id":format!("conn-{connection}"),"agent_id":22}}).to_string().into())).await.unwrap();
}

async fn next_ack(socket: &mut WebSocketStream<TcpStream>) -> TaskAckPayload {
    loop {
        let frame = timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("ACK deadline")
            .unwrap()
            .unwrap();
        match frame {
            Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text).unwrap() {
                ClientMessage::Ack(payload) => return payload,
                ClientMessage::Heartbeat(HeartbeatPing::Ping { ping_id }) => {
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ServerMessage::heartbeat_pong(ping_id))
                                .unwrap()
                                .into(),
                        ))
                        .await
                        .unwrap();
                }
                _ => panic!("Inbox must not change presence or accept a task"),
            },
            _ => panic!("Expected ACK JSON"),
        }
    }
}

fn config(address: std::net::SocketAddr) -> Config {
    Config {
        server_url: format!("ws://{address}/ws/broker"),
        auto_reconnect: false,
        ..Config::default()
    }
}

async fn run(config: Config, inbox: Inbox, state: CentralState) {
    timeout(
        Duration::from_secs(25),
        central::run_with_inbox(
            config,
            "test-only-agent-token".into(),
            state,
            CancellationToken::new(),
            inbox,
        ),
    )
    .await
    .unwrap();
}

fn state() -> CentralState {
    CentralState::new(Arc::new(RwLock::new(ConnectionStatus::Connecting)))
}

#[tokio::test]
async fn task_is_private_and_durable_before_ack_and_replayed_event_survives_new_runner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observer = Inbox::for_config(&path);
    let server = tokio::spawn(async move {
        for connection in 1..=2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            ready(&mut socket, connection).await;
            let mut wrong = event();
            wrong.payload.receiver_agent_id = 23;
            socket
                .send(Message::Text(serde_json::to_string(&wrong).unwrap().into()))
                .await
                .unwrap();
            assert!(
                timeout(Duration::from_millis(120), socket.next())
                    .await
                    .is_err(),
                "Wrong receiver must not be ACKed"
            );
            for _ in 0..2 {
                let valid = event();
                socket
                    .send(Message::Text(serde_json::to_string(&valid).unwrap().into()))
                    .await
                    .unwrap();
                assert_eq!(next_ack(&mut socket).await, valid.acknowledgement());
                // Reading a fresh store after the ACK proves persistence preceded acknowledgement.
                assert_eq!(observer.list(20).await.unwrap(), vec![valid]);
            }
            socket.close(None).await.unwrap();
        }
    });
    for _ in 0..2 {
        run(config(address), Inbox::for_config(&path), state()).await;
    }
    server.await.unwrap();
    let raw = std::fs::read(config::state_directory(&path).join("inbox/tasks.json")).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(stored["events"].as_array().unwrap().len(), 1);
    devlink_broker::credentials::verify_private_permissions(
        &config::state_directory(&path).join("inbox/tasks.json"),
    )
    .unwrap();
}

#[tokio::test]
async fn failed_persistence_never_acks_and_does_not_block_socket_control_ping() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let inbox = Inbox::for_config(&path);
    inbox.record(event(), 22).await.unwrap();
    std::fs::write(
        config::state_directory(&path).join("inbox/tasks.json"),
        b"corrupt",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        ready(&mut socket, 1).await;
        socket
            .send(Message::Text(
                serde_json::to_string(&event()).unwrap().into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap();
        let pong = timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(pong, Message::Pong(vec![1, 2, 3].into()));
        assert!(
            timeout(Duration::from_secs(1), socket.next())
                .await
                .is_err(),
            "Failed storage must not generate ACK"
        );
        socket.close(None).await.unwrap();
    });
    run(config(address), inbox.clone(), state()).await;
    server.await.unwrap();
    assert!(inbox.diagnostics().rejected_events > 0);
    assert!(inbox.diagnostics().last_error.is_some());
    assert_eq!(
        std::fs::read(config::state_directory(&path).join("inbox/tasks.json")).unwrap(),
        b"corrupt"
    );
}

#[tokio::test]
async fn task_before_ready_is_rejected_without_creating_an_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&event()).unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Close(_)
        ));
    });
    let status = state();
    run(config(address), Inbox::for_config(&path), status.clone()).await;
    server.await.unwrap();
    assert_eq!(status.status().await, ConnectionStatus::ProtocolViolation);
    assert!(!config::state_directory(&path).join("inbox").exists());
}
