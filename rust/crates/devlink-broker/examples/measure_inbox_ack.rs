use devlink_broker::{
    central::{self, CentralState, ConnectionStatus},
    config::Config,
    inbox::Inbox,
};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::RwLock,
    time::{Instant, timeout},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use tokio_util::sync::CancellationToken;

async fn ack(socket: &mut WebSocketStream<TcpStream>) -> u64 {
    loop {
        let frame = timeout(Duration::from_secs(60), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            match json["type"].as_str() {
                Some("ACK")=>return json["payload"]["task_id"].as_u64().unwrap(),
                Some("HEARTBEAT")=>socket.send(Message::Text(serde_json::json!({"type":"HEARTBEAT","payload":{"kind":"PONG","ping_id":json["payload"]["ping_id"]}}).to_string().into())).await.unwrap(),
                _=>panic!("unexpected frame")
            }
        }
    }
}

async fn send(socket: &mut WebSocketStream<TcpStream>, id: u64) {
    socket.send(Message::Text(serde_json::json!({"type":"TASK_CREATED","event_id":format!("latency-{id}"),"payload":{"task_id":id,"team_id":3,"requester_agent_id":11,"receiver_agent_id":22,"prompt":"non-secret latency fixture","created_at":"2026-10-29T12:00:00"}}).to_string().into())).await.unwrap();
}

fn stats(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let rank =
        |percent: f64| sorted[((sorted.len() as f64 * percent).ceil() as usize).saturating_sub(1)];
    serde_json::json!({"samples":values.len(),"p50Ms":rank(0.5),"p95Ms":rank(0.95),"maxMs":sorted.last(),"over5000Ms":values.iter().filter(|ms|**ms>=5000.).count(),"latenciesMs":values})
}

#[tokio::main]
async fn main() {
    let temp = tempfile::tempdir().unwrap();
    let inbox = Inbox::for_config(&temp.path().join("config.json"));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.send(Message::Text(serde_json::json!({"type":"BROKER_READY","payload":{"connection_id":"latency-connection","agent_id":22}}).to_string().into())).await.unwrap();
        let mut single = vec![];
        for id in 1..=5 {
            let begin = Instant::now();
            send(&mut socket, id).await;
            assert_eq!(ack(&mut socket).await, id);
            single.push(begin.elapsed().as_secs_f64() * 1000.);
        }
        let mut burst = vec![];
        let begin = Instant::now();
        for id in 6..=21 {
            send(&mut socket, id).await;
        }
        let mut ids = std::collections::HashSet::new();
        for _ in 0..16 {
            ids.insert(ack(&mut socket).await);
            burst.push(begin.elapsed().as_secs_f64() * 1000.);
        }
        assert_eq!(ids.len(), 16);
        socket.close(None).await.unwrap();
        serde_json::json!({"environment":"Windows MSVC debug build, real private file ACL/flush + loopback fixture; not central DB","ackBudgetComparisonMs":5000,"single":stats(&single),"burst16":stats(&burst)})
    });
    let configuration = Config {
        server_url: format!("ws://{address}/ws/broker"),
        auto_reconnect: false,
        ..Config::default()
    };
    let state = CentralState::new(Arc::new(RwLock::new(ConnectionStatus::Connecting)));
    timeout(
        Duration::from_secs(120),
        central::run_with_inbox(
            configuration,
            "non-secret-fixture-token".into(),
            state,
            CancellationToken::new(),
            inbox,
        ),
    )
    .await
    .unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&server.await.unwrap()).unwrap()
    );
}
