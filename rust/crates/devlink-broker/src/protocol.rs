//! # DevLink Central Protocol
//!
//! 중앙 서버(DevLink Server)와 WebSocket 통신 시 교환하는 메시지 데이터 규격을 정의하는 모듈입니다.
//!
//! - 모든 메시지는 JSON 형식으로 직렬화/역직렬화됩니다.
//! - serde의 내부 태그 방식(`#[serde(tag = "type", rename_all = "snake_case")]`)을 사용하여
//!   JSON 객체의 `"type"` 필드로 각 메시지 유형을 식별합니다.
//! - 정식 프로토콜 확정 전까지 사용되는 실습/임시 프로토콜 규격입니다.

use serde::{Deserialize, Serialize};

/// 클라이언트(Broker)에서 중앙 서버로 전송하는 메시지 규격
///
/// 불필요한 메모리 할당 및 복사를 방지하기 위해 라이프타임(`'a`) 기반의 문자열 슬라이스(`&'a str`)를 사용합니다.
///
/// # 직렬화(JSON) 예시
/// - `Auth`: `{"type": "auth", "token": "..."}`
/// - `Hello`: `{"type": "hello", "message": "..."}`
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage<'a> {
    /// 서버 연결 직후 인증을 위해 클라이언트 토큰을 전송하는 메시지
    Auth {
        /// 사용자 또는 장치 인증 토큰
        token: &'a str,
    },
    /// 인증 완료 후 연결 확인 및 인사 메시지를 전송하는 메시지
    Hello {
        /// 전송할 메시지 내용
        message: &'a str,
    },
}

/// 중앙 서버에서 클라이언트(Broker)로 수신되는 메시지 규격
///
/// 수신된 JSON 텍스트로부터 역직렬화되며, `"type"` 필드 값에 따라 해당 variant로 파싱됩니다.
///
/// # 역직렬화(JSON) 예시
/// - `AuthOk`: `{"type": "auth_ok"}`
/// - `AuthError`: `{"type": "auth_error"}`
/// - `Echo`: `{"type": "echo", "message": "..."}`
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 인증 성공 응답
    AuthOk,
    /// 인증 실패 응답 (만료되거나 올바르지 않은 토큰)
    AuthError,
    /// 서버로부터 전달받은 에코 테스트 응답 메시지
    Echo {
        /// 에코 응답 메시지 내용
        message: String,
    },
}

