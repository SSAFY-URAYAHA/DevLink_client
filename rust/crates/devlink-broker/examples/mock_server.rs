//! Compatible with cli-pratice/src/mock-server.ts. Never prints authentication payloads.
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::main]
async fn main() -> Result<()> {
    let token = std::env::var("DEVLINK_TOKEN").context("Set DEVLINK_TOKEN for the mock server")?;
    let addr = std::env::var("DEVLINK_MOCK_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = TcpListener::bind(&addr).await?;
    println!(
        "Mock central server listening on {}",
        listener.local_addr()?
    );
    loop {
        let (stream, _) = tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            accepted = listener.accept() => accepted?,
        };
        let token = token.clone();
        tokio::spawn(async move {
            let Ok(Ok(mut socket)) =
                tokio::time::timeout(std::time::Duration::from_secs(5), accept_async(stream)).await
            else {
                return;
            };
            let message =
                tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await;
            let valid = match message {
                Ok(Some(Ok(Message::Text(text)))) => {
                    serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .is_some_and(|v| v["type"] == "auth" && v["token"].as_str() == Some(&token))
                }
                _ => false,
            };
            if !valid {
                let _ = socket
                    .send(Message::Text(r#"{"type":"auth_error"}"#.into()))
                    .await;
                let _ = socket.close(None).await;
                return;
            }
            if socket
                .send(Message::Text(r#"{"type":"auth_ok"}"#.into()))
                .await
                .is_err()
            {
                return;
            }
            while let Some(Ok(message)) = socket.next().await {
                let response = match message {
                    Message::Text(_) => Message::Text(
                        r#"{"type":"echo","message":"Authenticated message received"}"#.into(),
                    ),
                    Message::Ping(payload) => Message::Pong(payload),
                    Message::Close(_) => break,
                    _ => continue,
                };
                if socket.send(response).await.is_err() {
                    break;
                }
            }
        });
    }
}
