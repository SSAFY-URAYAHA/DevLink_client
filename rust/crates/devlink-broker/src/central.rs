//! # DevLink Central WebSocket Client
//!
//! 중앙 서버(DevLink Server)와의 WebSocket 연결, 인증, 하트비트 및 지수 백오프 기반 재연결을 담당하는 모듈입니다.

use crate::{
    config::Config,
    protocol::{ClientMessage, ServerMessage},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::RwLock,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_util::sync::CancellationToken;

/// 중앙 웹소켓 서버와의 연결 상태
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// 웹소켓 TCP/TLS 핸드셰이크 진행 중
    Connecting,
    /// 서버와 연결 후 인증 토큰 전달 및 응답 대기 중
    Authenticating,
    /// 인증 성공 후 정상 연결 유지 상태
    Connected,
    /// 일시적 연결 해제 후 재연결 시도 중
    Reconnecting,
    /// 잘못된 토큰 등으로 인한 인증 실패 (재시도 중단)
    AuthFailed,
    /// 정상 또는 강제 종료 후 연결 해제 상태
    Disconnected,
}

/// 스레드/비동기 태스크 간 안전하게 공유되는 연결 상태 타입
pub type SharedStatus = Arc<RwLock<ConnectionStatus>>;

/// 단일 웹소켓 세션 종료 결과
enum Outcome {
    /// 재연결 필요 (네트워크 단절, 타임아웃 등)
    Retry,
    /// 인증 실패로 인한 영구 중단
    AuthFailed,
    /// 브로커 종료 시그널(cancel)에 의한 정상 종료
    Stopped,
}

/// 중앙 서버 연결 관리의 최상위 루프
/// - 설정된 `auto_reconnect` 옵션 및 지수 백오프(exponential backoff)에 따라 지속적으로 재연결을 시도합니다.
pub async fn run(config: Config, token: String, status: SharedStatus, cancel: CancellationToken) {
    let mut delay = config.reconnect_initial_ms;
    loop {
        // 단일 웹소켓 세션 실행
        let outcome = session(&config, &token, &status, &cancel).await;
        match outcome {
            Outcome::Stopped => break,
            Outcome::AuthFailed => {
                *status.write().await = ConnectionStatus::AuthFailed;
                return;
            }
            Outcome::Retry if !config.auto_reconnect => break,
            Outcome::Retry => {}
        }

        // 성공적으로 연결된 적이 있다면 다음 재연결 지연시간을 초기값으로 리셋
        if *status.read().await == ConnectionStatus::Connected {
            delay = config.reconnect_initial_ms;
        }
        *status.write().await = ConnectionStatus::Reconnecting;

        // 취소 토큰 감지 또는 지연 대기
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = sleep(Duration::from_millis(delay)) => {}
        }

        // 지수 백오프: 재연결 지연 시간을 2배씩 늘려 최대 reconnect_max_ms까지 제한
        delay = delay.saturating_mul(2).min(config.reconnect_max_ms);
    }
    *status.write().await = ConnectionStatus::Disconnected;
}

/// 단일 WebSocket 연결 세션을 수립하고 인증 및 메시지 루프를 실행합니다.
async fn session(
    config: &Config,
    token: &str,
    status: &SharedStatus,
    cancel: &CancellationToken,
) -> Outcome {
    if *status.read().await != ConnectionStatus::Reconnecting {
        *status.write().await = ConnectionStatus::Connecting;
    }

    // 웹소켓 프레임 및 메시지 최대 크기 제한 (64KB)
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));

    // 중앙 서버에 비동기 웹소켓 연결 (5초 타임아웃)
    let connection = tokio::select! {
        _ = cancel.cancelled() => return Outcome::Stopped,
        connection = timeout(Duration::from_secs(5), tokio_tungstenite::connect_async_with_config(config.server_url.as_str(), Some(ws_config), false)) => connection,
    };
    let Ok(Ok((mut socket, _))) = connection else {
        return Outcome::Retry;
    };

    // 1단계: 서버로 인증 토큰 전송 (2초 타임아웃)
    *status.write().await = ConnectionStatus::Authenticating;
    let auth =
        serde_json::to_string(&ClientMessage::Auth { token }).expect("serializable protocol");
    let sent = tokio::select! {
        _ = cancel.cancelled() => return Outcome::Stopped,
        sent = timeout(Duration::from_secs(2), socket.send(Message::Text(auth.into()))) => sent,
    };
    if !matches!(sent, Ok(Ok(()))) {
        return Outcome::Retry;
    }

    // 인증 응답 대기 타임아웃 설정
    let auth_deadline = sleep(Duration::from_millis(config.auth_timeout_ms));
    tokio::pin!(auth_deadline);

    // 5초 간격 하트비트(Ping) 및 응답 타임아웃(15초) 측정
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    let mut last_seen = Instant::now();
    let mut authenticated = false;

    // 2단계: 양방향 웹소켓 통신 루프
    loop {
        tokio::select! {
            // 브로커 종료 시 정상적으로 웹소켓 Close 프레임 전송 후 종료
            _ = cancel.cancelled() => {
                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                return Outcome::Stopped;
            }

            // 인증 제한 시간 초과 체크
            _ = &mut auth_deadline, if !authenticated => return Outcome::Retry,

            // 주기적 하트비트(Ping) 전송 및 15초 이상 서버 응답 부재 시 재연결
            _ = heartbeat.tick(), if authenticated => {
                if last_seen.elapsed() > Duration::from_secs(15) { return Outcome::Retry; }
                if !matches!(timeout(Duration::from_secs(1), socket.send(Message::Ping(Vec::new().into()))).await, Ok(Ok(()))) { return Outcome::Retry; }
            }

            // 서버로부터 메시지 수신 처리
            message = socket.next() => {
                match message {
                    // 서버에서 Close 프레임 수신 (4001: 인증 실패 코드)
                    Some(Ok(Message::Close(frame))) => {
                        return if frame.is_some_and(|frame| u16::from(frame.code) == 4001) { Outcome::AuthFailed } else { Outcome::Retry };
                    }
                    None | Some(Err(_)) => return Outcome::Retry,

                    // 텍스트(JSON) 메시지 수신 처리
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        match serde_json::from_str::<ServerMessage>(&text) {
                            // 인증 성공 응답 수신
                            Ok(ServerMessage::AuthOk) if !authenticated => {
                                authenticated = true;
                                *status.write().await = ConnectionStatus::Connected;
                                let hello = serde_json::to_string(&ClientMessage::Hello { message: "DevLink Broker Connected" }).expect("serializable protocol");
                                if !matches!(timeout(Duration::from_secs(1), socket.send(Message::Text(hello.into()))).await, Ok(Ok(()))) { return Outcome::Retry; }
                            }
                            // 인증 실패 응답 수신
                            Ok(ServerMessage::AuthError) => return Outcome::AuthFailed,
                            _ => {} // 보안을 위해 임의의 서버 페이로드는 로그에 남기지 않음
                        }
                    }

                    // 서버로부터 Ping 수신 시 Pong 응답
                    Some(Ok(Message::Ping(payload))) => {
                        last_seen = Instant::now();
                        if !matches!(timeout(Duration::from_secs(1), socket.send(Message::Pong(payload))).await, Ok(Ok(()))) { return Outcome::Retry; }
                    }

                    // 서버로부터 Pong 수신 시 last_seen 갱신
                    Some(Ok(Message::Pong(_))) => last_seen = Instant::now(),
                    _ => {}
                }
            }
        }
    }
}
