//! # DevLink Central WebSocket Client
//!
//! 중앙 서버(DevLink Server)와의 WebSocket 연결, 인증, `BROKER_READY` 수신,
//! 확인된 로컬 Agent 접속 상태(`AGENT_PRESENCE`) 보고, 하트비트 및 지수 백오프 기반 재연결을 담당하는 모듈입니다.

use crate::{
    config::Config,
    protocol::{ClientMessage, PresenceStatus, ServerMessage},
};
use anyhow::{Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::RwLock,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::tungstenite::{
    Error as WsError, Message,
    client::IntoClientRequest,
    http::{HeaderValue, Request, StatusCode, header::AUTHORIZATION},
    protocol::WebSocketConfig,
};
pub use tokio_util::sync::CancellationToken;

/// 중앙 웹소켓 서버와의 연결 상태
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// 웹소켓 TCP/TLS 및 HTTP Upgrade 핸드셰이크 진행 중
    Connecting,
    /// Upgrade 성공 후 서버의 유효한 `BROKER_READY` 응답 대기 중
    Authenticating,
    /// 유효한 `BROKER_READY` 수신 완료 후 정상 연결 유지 상태
    /// (확인된 로컬 Agent 상태가 있는 경우 초기 `AGENT_PRESENCE` 전송까지 완료된 상태)
    Connected,
    /// 일시적 연결 해제 또는 타임아웃 후 재연결 시도 중
    Reconnecting,
    /// Upgrade 401/403 또는 Close 4001 등 인증 거부 (자동 재시도 중단)
    AuthFailed,
    /// Close 4002: 동일 Agent의 새 연결로 교체됨 (자동 재시도 중단)
    Replaced,
    /// Close 1008 또는 잘못된 `BROKER_READY` 등 메시지/정책 위반 (자동 재시도 중단)
    ProtocolViolation,
    /// 정상/강제 종료 또는 `auto_reconnect=false` 상태에서 연결 해제
    Disconnected,
}

/// 현재 활성 중앙 WebSocket 연결에 귀속되는 식별 정보 (`BROKER_READY` 수신 시 설정, 연결 종료/교체 시 무효화)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CentralIdentity {
    /// 현재 중앙 서버 WebSocket 세션 식별자
    pub connection_id: String,
    /// 인증된 Agent의 숫자 식별자
    pub agent_id: u64,
}

/// 스레드/비동기 태스크 간 안전하게 공유되는 연결 상태 타입
pub type SharedStatus = Arc<RwLock<ConnectionStatus>>;

/// 현재 활성 연결의 식별 정보(`connection_id`, `agent_id`) 공유 타입
pub type SharedIdentity = Arc<RwLock<Option<CentralIdentity>>>;

/// 로컬 Agent의 현재 접속 상태 공유 타입
/// - `None`: 로컬 Agent 상태 미확인 (추측하여 `ONLINE`/`OFFLINE`을 보고하지 않음)
/// - `Some(PresenceStatus::Offline)`: 로컬 Agent 연결이 없다고 확인된 상태
/// - `Some(PresenceStatus::Online)`: 로컬 Agent 연결이 있다고 확인된 상태
pub type SharedPresence = Arc<RwLock<Option<PresenceStatus>>>;

#[derive(Debug)]
struct StateInner {
    generation: u64,
    status: ConnectionStatus,
    identity: Option<CentralIdentity>,
}

/// 세대(generation) 번호를 통해 이전 연결의 지연 정리나 타이머가
/// 새 연결의 상태 및 식별 정보(`connection_id`, `agent_id`)를 덮어쓰지 못하도록 보호하는 상태 관리 구조체
#[derive(Clone)]
pub struct CentralState {
    inner: Arc<RwLock<StateInner>>,
    status: SharedStatus,
    identity: SharedIdentity,
    presence: SharedPresence,
}

impl CentralState {
    /// 기본 상태 저장소를 생성합니다.
    /// 2단계에서는 로컬 Agent 상태 감지 및 `/v1/sessions/*` API가 아직 연결되지 않았으므로
    /// 로컬 Agent 상태를 추측하지 않고 미확인(`None`)으로 초기화합니다.
    pub fn new(status: SharedStatus) -> Self {
        Self::with_details(
            status,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
        )
    }

    /// 외부에서 상태·식별자·확인된 로컬 Agent 접속 상태(`Option<PresenceStatus>`)를 주입하여 생성합니다.
    pub fn with_details(
        status: SharedStatus,
        identity: SharedIdentity,
        presence: SharedPresence,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(StateInner {
                generation: 0,
                status: ConnectionStatus::Connecting,
                identity: None,
            })),
            status,
            identity,
            presence,
        }
    }

    /// 새 연결 세션을 시작하며 세대(generation)를 증가시키고 이전 연결의 식별 정보를 즉시 무효화합니다.
    pub async fn begin_session(&self) -> SessionHandle {
        let mut guard = self.inner.write().await;
        guard.generation = guard.generation.saturating_add(1);
        guard.identity = None;
        *self.identity.write().await = None;
        SessionHandle {
            state: self.clone(),
            generation: guard.generation,
        }
    }

    /// 현재 연결 상태를 조회합니다.
    pub async fn status(&self) -> ConnectionStatus {
        *self.status.read().await
    }

    /// 현재 활성 연결의 식별 정보를 조회합니다.
    pub async fn identity(&self) -> Option<CentralIdentity> {
        self.identity.read().await.clone()
    }

    /// 현재 확인된 로컬 Agent 접속 상태를 조회합니다 (`None`이면 미확인).
    pub async fn presence(&self) -> Option<PresenceStatus> {
        *self.presence.read().await
    }
}

/// 특정 연결 세대(generation)에 귀속된 상태 변경 핸들.
/// 더 새로운 연결 세대가 시작된 이후에는 이전 핸들의 모든 상태/식별자 변경 요청이 무시됩니다.
#[derive(Clone)]
pub struct SessionHandle {
    state: CentralState,
    generation: u64,
}

impl SessionHandle {
    /// 세대 번호를 반환합니다.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 현재 핸들이 최신 연결 세대인 경우에만 연결 상태를 갱신합니다.
    pub async fn set_status(&self, next: ConnectionStatus) -> bool {
        let mut guard = self.state.inner.write().await;
        if guard.generation != self.generation {
            return false;
        }
        guard.status = next;
        *self.state.status.write().await = next;
        true
    }

    /// 현재 핸들이 최신 연결 세대인 경우에만 `BROKER_READY` 식별 정보를 기록합니다.
    pub async fn set_identity(&self, identity: CentralIdentity) -> bool {
        let mut guard = self.state.inner.write().await;
        if guard.generation != self.generation {
            return false;
        }
        guard.identity = Some(identity.clone());
        *self.state.identity.write().await = Some(identity);
        true
    }

    /// 현재 핸들이 최신 연결 세대인 경우에만 식별 정보를 무효화합니다.
    pub async fn clear_identity(&self) -> bool {
        let mut guard = self.state.inner.write().await;
        if guard.generation != self.generation {
            return false;
        }
        guard.identity = None;
        *self.state.identity.write().await = None;
        true
    }

    /// 현재 상태가 `Reconnecting`이 아니면 `Connecting`으로 전환합니다.
    pub async fn mark_connecting(&self) -> bool {
        let mut guard = self.state.inner.write().await;
        if guard.generation != self.generation {
            return false;
        }
        if guard.status != ConnectionStatus::Reconnecting {
            guard.status = ConnectionStatus::Connecting;
            *self.state.status.write().await = ConnectionStatus::Connecting;
        }
        true
    }

    /// 현재 확인된 로컬 Agent의 접속 상태(`Some(Online)` / `Some(Offline)` / 미확인 `None`)를 읽습니다.
    pub async fn current_presence(&self) -> Option<PresenceStatus> {
        *self.state.presence.read().await
    }
}

/// 단일 웹소켓 세션 종료 결과
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// 재연결 필요 (일시적 네트워크 단절, `BROKER_READY` 대기 타임아웃 등)
    Retry {
        /// 해당 세션에서 유효한 `BROKER_READY`를 받아 `Connected`에 도달했었는지 여부
        was_connected: bool,
    },
    /// 인증 거부로 인한 영구 중단 (HTTP 401/403, Close 4001)
    AuthFailed,
    /// 동일 Agent의 새 연결로 교체되어 영구 중단 (Close 4002)
    Replaced,
    /// 메시지/정책 위반 또는 잘못된 `BROKER_READY` 수신으로 인한 영구 중단 (Close 1008 등)
    ProtocolViolation,
    /// 브로커 종료 시그널(cancel)에 의한 정상 종료
    Stopped,
}

/// 설정된 `server_url`을 중앙 브로커 엔드포인트(`/ws/broker`) URL로 검증·정규화합니다.
/// - 루트 경로(`""` 또는 `"/"`)인 경우 `"/ws/broker"`로 정규화합니다.
/// - 명시적 `"/ws/broker"` 경로는 그대로 허용합니다.
/// - 그 외 임의 경로는 허용하지 않고 오류로 거부하여 다른 경로로 Bearer 토큰이 전송되지 않게 합니다.
/// - URL 쿼리, 프래그먼트, 사용자 자격 증명은 허용하지 않습니다.
pub fn endpoint_url(server_url: &str) -> Result<String> {
    let mut url = url::Url::parse(server_url).map_err(|_| anyhow::anyhow!("Invalid serverUrl"))?;
    ensure!(
        matches!(url.scheme(), "ws" | "wss") && url.has_host(),
        "serverUrl must use ws:// or wss://"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "serverUrl must not include credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "serverUrl must not include query parameters or fragments"
    );
    match url.path() {
        "" | "/" => url.set_path("/ws/broker"),
        "/ws/broker" => {}
        _ => bail!("serverUrl path must be empty, '/', or '/ws/broker'"),
    }
    Ok(url.into())
}

/// `/ws/broker` WebSocket Upgrade 요청을 생성하고 `Authorization: Bearer <Agent 토큰>` 헤더를 설정합니다.
/// - 토큰은 오직 `/ws/broker` 경로의 `Authorization` 헤더로만 전달하며 URL 쿼리나 쿠키로 전달하지 않습니다.
/// - 임의 `Origin`이나 `Cookie` 헤더를 추가하지 않습니다.
/// - `HeaderValue::set_sensitive(true)`를 설정해 디버그 출력 시 헤더 값이 노출되지 않도록 보호합니다.
pub fn build_upgrade_request(server_url: &str, token: &str) -> Result<Request<()>> {
    let trimmed = token.trim();
    ensure!(
        !trimmed.is_empty() && token.len() <= 8192,
        "DEVLINK_TOKEN must be nonempty and at most 8192 bytes"
    );
    let normalized = endpoint_url(server_url)?;
    let mut request = normalized
        .as_str()
        .into_client_request()
        .map_err(|_| anyhow::anyhow!("Cannot build WebSocket upgrade request"))?;

    let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow::anyhow!("DEVLINK_TOKEN contains invalid HTTP header characters"))?;
    bearer.set_sensitive(true);

    request.headers_mut().remove("origin");
    request.headers_mut().remove("cookie");
    request.headers_mut().insert(AUTHORIZATION, bearer);
    Ok(request)
}

/// 중앙 서버 연결 관리의 최상위 루프
/// - 설정된 `auto_reconnect` 옵션 및 지수 백오프(exponential backoff)에 따라 재연결을 관리합니다.
pub async fn run(config: Config, token: String, status: SharedStatus, cancel: CancellationToken) {
    let state = CentralState::new(status);
    run_with_state(config, token, state, cancel).await;
}

/// 상태/식별자/로컬 Agent 접속 상태 저장소를 지정하여 중앙 서버 연결 루프를 실행합니다.
pub async fn run_with_state(
    config: Config,
    token: String,
    state: CentralState,
    cancel: CancellationToken,
) {
    let mut delay = config.reconnect_initial_ms;
    loop {
        let session_handle = state.begin_session().await;
        let outcome = session(&config, &token, &session_handle, &cancel).await;
        // 연결 세션이 종료되면 해당 세션의 connection_id / agent_id를 즉시 무효화
        session_handle.clear_identity().await;

        match outcome {
            Outcome::Stopped => {
                session_handle
                    .set_status(ConnectionStatus::Disconnected)
                    .await;
                return;
            }
            Outcome::AuthFailed => {
                session_handle
                    .set_status(ConnectionStatus::AuthFailed)
                    .await;
                return;
            }
            Outcome::Replaced => {
                session_handle.set_status(ConnectionStatus::Replaced).await;
                return;
            }
            Outcome::ProtocolViolation => {
                session_handle
                    .set_status(ConnectionStatus::ProtocolViolation)
                    .await;
                return;
            }
            Outcome::Retry { was_connected } => {
                if was_connected {
                    delay = config.reconnect_initial_ms;
                }
                if !config.auto_reconnect {
                    session_handle
                        .set_status(ConnectionStatus::Disconnected)
                        .await;
                    return;
                }
            }
        }

        session_handle
            .set_status(ConnectionStatus::Reconnecting)
            .await;

        // 취소 토큰 감지 또는 지수 백오프 지연 대기
        tokio::select! {
            _ = cancel.cancelled() => {
                session_handle.set_status(ConnectionStatus::Disconnected).await;
                return;
            }
            _ = sleep(Duration::from_millis(delay)) => {}
        }

        // 지수 백오프: 재연결 지연 시간을 2배씩 늘려 최대 reconnect_max_ms까지 제한
        delay = delay.saturating_mul(2).min(config.reconnect_max_ms);
    }
}

/// WebSocket Close 코드를 종료 결과(`Outcome`)로 분류합니다.
fn classify_close_code(code: u16, was_connected: bool) -> Outcome {
    match code {
        4001 => Outcome::AuthFailed,
        4002 => Outcome::Replaced,
        1008 => Outcome::ProtocolViolation,
        _ => Outcome::Retry { was_connected },
    }
}

/// 단일 WebSocket 연결 세션을 수립하고 `BROKER_READY` 확인, 확인된 `AGENT_PRESENCE` 보고 및 메시지 루프를 실행합니다.
async fn session(
    config: &Config,
    token: &str,
    handle: &SessionHandle,
    cancel: &CancellationToken,
) -> Outcome {
    if !handle.mark_connecting().await {
        return Outcome::Stopped;
    }

    let Ok(request) = build_upgrade_request(&config.server_url, token) else {
        return Outcome::ProtocolViolation;
    };

    // 웹소켓 프레임 및 메시지 최대 크기 제한 (64KB)
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));

    // 중앙 서버에 비동기 웹소켓 Upgrade 연결 (5초 타임아웃)
    let connection = tokio::select! {
        _ = cancel.cancelled() => return Outcome::Stopped,
        connection = timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false),
        ) => connection,
    };

    let mut socket = match connection {
        Ok(Ok((socket, _response))) => socket,
        Ok(Err(WsError::Http(response)))
            if matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) =>
        {
            return Outcome::AuthFailed;
        }
        Ok(Err(_)) | Err(_) => {
            return Outcome::Retry {
                was_connected: false,
            };
        }
    };

    // Upgrade 직후에는 아직 연결 완료(Connected)로 전환하지 않고 BROKER_READY를 대기
    if !handle.set_status(ConnectionStatus::Authenticating).await {
        let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
        return Outcome::Stopped;
    }

    // BROKER_READY 응답 대기 타임아웃 설정 (config.auth_timeout_ms 재사용)
    let ready_deadline = sleep(Duration::from_millis(config.auth_timeout_ms));
    tokio::pin!(ready_deadline);

    // 5초 간격 하트비트(Ping) 및 응답 타임아웃(15초) 측정
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    let mut last_seen = Instant::now();
    let mut ready = false;

    loop {
        tokio::select! {
            // 브로커 종료 요청 시 정상적으로 웹소켓 Close 프레임 전송 후 종료
            _ = cancel.cancelled() => {
                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                return Outcome::Stopped;
            }

            // BROKER_READY 대기 제한 시간 초과 체크
            _ = &mut ready_deadline, if !ready => {
                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                return Outcome::Retry {
                    was_connected: false,
                };
            }

            // 주기적 하트비트(Ping) 전송 및 15초 이상 서버 응답 부재 시 재연결
            _ = heartbeat.tick(), if ready => {
                if last_seen.elapsed() > Duration::from_secs(15) {
                    return Outcome::Retry {
                        was_connected: true,
                    };
                }
                if !matches!(
                    timeout(
                        Duration::from_secs(1),
                        socket.send(Message::Ping(Vec::new().into())),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    return Outcome::Retry {
                        was_connected: true,
                    };
                }
            }

            // 서버로부터 메시지 수신 처리
            message = socket.next() => {
                match message {
                    // 서버에서 Close 프레임 수신
                    Some(Ok(Message::Close(frame))) => {
                        return match frame {
                            Some(frame) => classify_close_code(u16::from(frame.code), ready),
                            None => Outcome::Retry {
                                was_connected: ready,
                            },
                        };
                    }
                    None | Some(Err(_)) => {
                        return Outcome::Retry {
                            was_connected: ready,
                        };
                    }

                    // 텍스트(JSON) 메시지 수신 처리
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        if !ready {
                            match serde_json::from_str::<ServerMessage>(&text) {
                                Ok(ServerMessage::BrokerReady(payload)) if payload.is_valid() => {
                                    let identity = CentralIdentity {
                                        connection_id: payload.connection_id,
                                        agent_id: payload.agent_id,
                                    };
                                    if !handle.set_identity(identity).await {
                                        let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                                        return Outcome::Stopped;
                                    }

                                    // 신뢰할 수 있는 현재 로컬 Agent 상태가 확인된 경우(Some(Online | Offline))에만
                                    // 초기/재연결 BROKER_READY 직후 AGENT_PRESENCE를 1회 전송한다.
                                    // 로컬 Agent 상태가 미확인(None)인 경우 추측하여 ONLINE/OFFLINE을 전송하지 않는다.
                                    if let Some(presence_status) = handle.current_presence().await {
                                        let presence_json = serde_json::to_string(
                                            &ClientMessage::agent_presence(presence_status),
                                        )
                                        .expect("serializable AGENT_PRESENCE protocol");

                                        let sent = tokio::select! {
                                            _ = cancel.cancelled() => {
                                                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                                                return Outcome::Stopped;
                                            }
                                            sent = timeout(
                                                Duration::from_secs(2),
                                                socket.send(Message::Text(presence_json.into())),
                                            ) => sent,
                                        };
                                        if !matches!(sent, Ok(Ok(()))) {
                                            return Outcome::Retry {
                                                was_connected: false,
                                            };
                                        }
                                    }

                                    ready = true;
                                    last_seen = Instant::now();
                                    heartbeat.reset();
                                    if !handle.set_status(ConnectionStatus::Connected).await {
                                        let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                                        return Outcome::Stopped;
                                    }
                                }
                                _ => {
                                    // 유효한 BROKER_READY가 아닌 초기 메시지는 프로토콜/정책 위반으로 처리하고 재시도 중단
                                    let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                                    return Outcome::ProtocolViolation;
                                }
                            }
                        } else {
                            // 이미 BROKER_READY가 완료된 상태에서 중복 BROKER_READY 또는 알 수 없는 비정상 JSON 수신 시
                            // 보안을 위해 원문은 로그에 남기지 않으며, 중복 BROKER_READY는 정책 위반으로 거부
                            if serde_json::from_str::<ServerMessage>(&text).is_ok() {
                                let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                                return Outcome::ProtocolViolation;
                            }
                        }
                    }

                    // 서버로부터 Binary 프레임이 BROKER_READY 이전에 오면 프로토콜 위반으로 거부
                    Some(Ok(Message::Binary(_))) if !ready => {
                        let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
                        return Outcome::ProtocolViolation;
                    }

                    // 서버로부터 Ping 수신 시 Pong 응답
                    Some(Ok(Message::Ping(payload))) => {
                        last_seen = Instant::now();
                        if !matches!(
                            timeout(Duration::from_secs(1), socket.send(Message::Pong(payload))).await,
                            Ok(Ok(()))
                        ) {
                            return Outcome::Retry {
                                was_connected: ready,
                            };
                        }
                    }

                    // 서버로부터 Pong 수신 시 last_seen 갱신
                    Some(Ok(Message::Pong(_))) => last_seen = Instant::now(),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_url_normalizes_root_and_rejects_non_broker_paths() {
        assert_eq!(
            endpoint_url("ws://localhost:8080").unwrap(),
            "ws://localhost:8080/ws/broker"
        );
        assert_eq!(
            endpoint_url("wss://example.com/").unwrap(),
            "wss://example.com/ws/broker"
        );
        assert_eq!(
            endpoint_url("ws://127.0.0.1:9000/ws/broker").unwrap(),
            "ws://127.0.0.1:9000/ws/broker"
        );
        for invalid in [
            "ws://localhost:8080/custom",
            "ws://localhost:8080/ws/broker/",
            "ws://localhost:8080/ws/broker/extra",
            "ws://localhost:8080/ws",
            "ws://localhost:8080/ws/broker?token=abc",
            "ws://localhost:8080/ws/broker#frag",
        ] {
            assert!(
                endpoint_url(invalid).is_err(),
                "endpoint_url must reject disallowed URL: {invalid}"
            );
        }
    }

    #[test]
    fn upgrade_request_sets_sensitive_bearer_header_without_origin_cookie_or_query() {
        let secret = "test-agent-token-do-not-leak";
        let req = build_upgrade_request("ws://127.0.0.1:8080", secret).unwrap();
        assert_eq!(req.uri().path(), "/ws/broker");
        assert!(req.uri().query().is_none());
        assert!(req.headers().get("origin").is_none());
        assert!(req.headers().get("cookie").is_none());

        let auth = req
            .headers()
            .get(AUTHORIZATION)
            .expect("Authorization header must be present");
        assert!(
            auth.is_sensitive(),
            "Authorization header must be marked sensitive"
        );
        assert_eq!(auth.to_str().unwrap(), format!("Bearer {secret}"));

        let debug_output = format!("{req:?}");
        assert!(
            !debug_output.contains(secret),
            "Debug formatting of upgrade request must not expose token"
        );

        assert!(build_upgrade_request("ws://127.0.0.1:8080/other", secret).is_err());
    }

    #[tokio::test]
    async fn central_state_defaults_to_unknown_local_presence() {
        let status = Arc::new(RwLock::new(ConnectionStatus::Connecting));
        let state = CentralState::new(status);
        assert_eq!(state.presence().await, None);
    }

    #[tokio::test]
    async fn stale_session_handle_cannot_overwrite_newer_session_state_or_identity() {
        let status = Arc::new(RwLock::new(ConnectionStatus::Connecting));
        let state = CentralState::new(status);

        let old_session = state.begin_session().await;
        assert!(
            old_session
                .set_identity(CentralIdentity {
                    connection_id: "conn-old".into(),
                    agent_id: 11,
                })
                .await
        );
        assert!(old_session.set_status(ConnectionStatus::Connected).await);

        // 새 연결 세션 시작 시 이전 세션의 식별 정보는 즉시 무효화됨
        let new_session = state.begin_session().await;
        assert_eq!(state.identity().await, None);
        assert!(
            new_session
                .set_identity(CentralIdentity {
                    connection_id: "conn-new".into(),
                    agent_id: 11,
                })
                .await
        );
        assert!(new_session.set_status(ConnectionStatus::Connected).await);

        // 이전 연결의 지연된 종료/오류 처리가 뒤늦게 실행되어도 새 연결 상태와 식별자를 덮어쓰지 못함
        assert!(!old_session.set_status(ConnectionStatus::Disconnected).await);
        assert!(!old_session.set_status(ConnectionStatus::Replaced).await);
        assert!(!old_session.clear_identity().await);
        assert!(
            !old_session
                .set_identity(CentralIdentity {
                    connection_id: "conn-stale".into(),
                    agent_id: 99,
                })
                .await
        );

        assert_eq!(state.status().await, ConnectionStatus::Connected);
        assert_eq!(
            state.identity().await,
            Some(CentralIdentity {
                connection_id: "conn-new".into(),
                agent_id: 11,
            })
        );
    }
}
