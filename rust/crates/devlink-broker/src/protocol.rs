//! # DevLink Central Protocol
//!
//! 중앙 서버(DevLink Server)의 `/ws/broker` 엔드포인트와 브로커 간 교환하는
//! 확정된 2단계 WebSocket JSON 메시지 규격을 정의하는 모듈입니다.
//!
//! - 연결 인증은 WebSocket Upgrade 요청의 `Authorization: Bearer <Agent 토큰>` 헤더로 수행하며,
//!   연결 수립 후 별도의 `auth` / `hello` 메시지를 송신하지 않습니다.
//! - 서버는 인증·등록 완료 시 `BROKER_READY`를 전송합니다.
//! - 브로커는 유효한 `BROKER_READY` 수신 직후 `AGENT_PRESENCE`로 로컬 Agent 접속 상태(`ONLINE` / `OFFLINE`)를 보고합니다.
//! - 연결 유지는 JSON `HEARTBEAT` PING/PONG과 동일한 `ping_id`로 확인합니다.

use serde::{Deserialize, Serialize};

/// 로컬 Agent와 브로커 사이의 접속 상태 (`ONLINE` 또는 `OFFLINE`)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PresenceStatus {
    Online,
    Offline,
}

/// `AGENT_PRESENCE` 메시지 페이로드 규격
///
/// `status` 외의 `agent_id`나 클라이언트 시각(`last_seen_at` 등)을 추가하지 않습니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresencePayload {
    pub status: PresenceStatus,
}

/// 서버가 만료 감시에 사용하는 JSON PING. WebSocket 제어 Ping과 별개입니다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum HeartbeatPing {
    Ping { ping_id: String },
}

/// 서버는 요청의 ping_id를 그대로 돌려줍니다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum HeartbeatPong {
    Pong { ping_id: String },
}

/// 브로커에서 중앙 서버로 전송하는 메시지 규격
///
/// # 직렬화(JSON) 예시
/// ```json
/// {
///   "type": "AGENT_PRESENCE",
///   "payload": { "status": "OFFLINE" }
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClientMessage {
    /// 로컬 Agent 접속 상태 보고 메시지
    AgentPresence(AgentPresencePayload),
    Heartbeat(HeartbeatPing),
}

impl ClientMessage {
    /// 주어진 접속 상태로 `AGENT_PRESENCE` 메시지를 생성합니다.
    pub fn agent_presence(status: PresenceStatus) -> Self {
        Self::AgentPresence(AgentPresencePayload { status })
    }

    pub fn heartbeat_ping(ping_id: String) -> Self {
        Self::Heartbeat(HeartbeatPing::Ping { ping_id })
    }
}

/// `BROKER_READY` 메시지 페이로드 규격
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerReadyPayload {
    /// 현재 중앙 WebSocket 세션 식별자
    pub connection_id: String,
    /// 인증된 Agent의 숫자 식별자 (양의 정수)
    pub agent_id: u64,
}

impl BrokerReadyPayload {
    /// `connection_id`가 비어 있지 않고 `agent_id`가 양수인지 검증합니다.
    pub fn is_valid(&self) -> bool {
        !self.connection_id.trim().is_empty() && self.agent_id > 0
    }
}

/// 중앙 서버에서 브로커로 수신되는 메시지 규격
///
/// # 역직렬화(JSON) 예시
/// ```json
/// {
///   "type": "BROKER_READY",
///   "payload": {
///     "connection_id": "current-websocket-session-id",
///     "agent_id": 11
///   }
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServerMessage {
    /// 서버 측 인증 및 ConnectionRegistry 등록 완료 응답
    BrokerReady(BrokerReadyPayload),
    Heartbeat(HeartbeatPong),
}

impl ServerMessage {
    pub fn heartbeat_pong(ping_id: String) -> Self {
        Self::Heartbeat(HeartbeatPong::Pong { ping_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_ready_parses_valid_payload_and_rejects_invalid_shapes() {
        let raw = r#"{
            "type": "BROKER_READY",
            "payload": {
                "connection_id": "current-websocket-session-id",
                "agent_id": 11
            }
        }"#;
        let msg: ServerMessage = serde_json::from_str(raw).expect("valid BROKER_READY must parse");
        let ServerMessage::BrokerReady(payload) = msg else {
            panic!("expected BROKER_READY");
        };
        assert!(payload.is_valid());
        assert_eq!(payload.connection_id, "current-websocket-session-id");
        assert_eq!(payload.agent_id, 11);

        // Empty connection_id or zero agent_id must be rejected by validation
        let empty_conn: ServerMessage = serde_json::from_str(
            r#"{"type":"BROKER_READY","payload":{"connection_id":"   ","agent_id":11}}"#,
        )
        .unwrap();
        let ServerMessage::BrokerReady(empty_payload) = empty_conn else {
            panic!("expected BROKER_READY");
        };
        assert!(!empty_payload.is_valid());

        let zero_agent: ServerMessage = serde_json::from_str(
            r#"{"type":"BROKER_READY","payload":{"connection_id":"conn-1","agent_id":0}}"#,
        )
        .unwrap();
        let ServerMessage::BrokerReady(zero_payload) = zero_agent else {
            panic!("expected BROKER_READY");
        };
        assert!(!zero_payload.is_valid());

        // Malformed types, string agent_id, unknown fields, and legacy messages must fail deserialization
        for invalid in [
            r#"{"type":"auth_ok"}"#,
            r#"{"type":"BROKER_READY","payload":{"connection_id":"c1","agent_id":"11"}}"#,
            r#"{"type":"BROKER_READY","payload":{"connection_id":"c1","agent_id":-1}}"#,
            r#"{"type":"BROKER_READY","payload":{"connection_id":"c1","agent_id":11,"extra":true}}"#,
            r#"{"type":"BROKER_READY","payload":{"connection_id":"c1"}}"#,
            r#"{"type":"BROKER_READY"}"#,
        ] {
            assert!(
                serde_json::from_str::<ServerMessage>(invalid).is_err(),
                "expected invalid ServerMessage to be rejected"
            );
        }
    }

    #[test]
    fn agent_presence_serializes_exact_contract_without_extra_fields() {
        let online = serde_json::to_value(ClientMessage::agent_presence(PresenceStatus::Online))
            .expect("online presence must serialize");
        assert_eq!(
            online,
            serde_json::json!({
                "type": "AGENT_PRESENCE",
                "payload": { "status": "ONLINE" }
            })
        );

        let offline = serde_json::to_value(ClientMessage::agent_presence(PresenceStatus::Offline))
            .expect("offline presence must serialize");
        assert_eq!(
            offline,
            serde_json::json!({
                "type": "AGENT_PRESENCE",
                "payload": { "status": "OFFLINE" }
            })
        );
    }

    #[test]
    fn heartbeat_uses_exact_json_ping_pong_contract() {
        assert_eq!(
            serde_json::to_value(ClientMessage::heartbeat_ping("ping-1".into())).unwrap(),
            serde_json::json!({"type":"HEARTBEAT","payload":{"kind":"PING","ping_id":"ping-1"}})
        );
        let pong = r#"{"type":"HEARTBEAT","payload":{"kind":"PONG","ping_id":"ping-1"}}"#;
        assert_eq!(
            serde_json::from_str::<ServerMessage>(pong).unwrap(),
            ServerMessage::heartbeat_pong("ping-1".into())
        );
        for payload in [
            r#"{"kind":"PING","ping_id":"ping-1"}"#,
            r#"{"kind":"PONG"}"#,
            r#"{"kind":"PONG","ping_id":1}"#,
            r#"{"kind":"PONG","ping_id":"ping-1","agent_id":11}"#,
            r#"{"kind":"UNKNOWN","ping_id":"ping-1"}"#,
        ] {
            let raw = format!(r#"{{"type":"HEARTBEAT","payload":{payload}}}"#);
            assert!(serde_json::from_str::<ServerMessage>(&raw).is_err());
        }
        assert!(serde_json::from_str::<ServerMessage>(&format!("{pong} {{}}")).is_err());
    }
}
