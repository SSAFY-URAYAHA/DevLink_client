# 로컬 MCP 세션 API

2026-10-29 갱신: 기본 CLI에 [READY 기반 신원 공급부](ready-identity.md)를 연결했다. 아래 2026-10-28 미구성 공급부 설명은 당시 기록이다. 현재 중앙의 user_id/team_id/agent_id가 있는 READY와 Connected 상태에서는 로컬 세션 생성·bind가 가능하며, legacy READY는 계속 CENTRAL_UNAUTHENTICATED다. 신원이 바뀌면 기존 세션을 만료시키고, 같은 신원 재접속은 lease 안의 소유권을 유지한다. 이 공급부와 실제 로컬 HTTP/WebSocket·CLI는 Mock으로 검증하며 운영 중앙 연동 완료를 뜻하지 않는다.

2026-10-28 (Asia/Seoul). 기존 [MCP 계약](../../packages/mcp-server/docs/broker-contract-v0.1.md)의 로컬 세션 관리 부분을 구현했습니다. 중앙 신원 조회·Agent 등록·토큰 발급과 업무 API는 별도 후속 항목입니다.

## 구현 범위와 현재 실행 상태

`sessions.rs`의 SessionManager가 생성·bind·갱신·해제·만료·소유권을 관리하고 `session_api.rs`가 HTTP 요청을 처리합니다. 브로커의 HTTP 핸들러·세션 관리자·중앙 WebSocket은 같은 CentralState를 사용합니다.

중앙 신원은 IdentityProvider 인터페이스로 분리했습니다. 기본 CLI 실행에는 UnconfiguredIdentityProvider를 연결하므로 유효한 MCP 토큰으로 세션 생성을 요청해도 **503 CENTRAL_UNAUTHENTICATED**를 반환합니다. READY의 agent_id, config.agent, 요청 본문으로 사용자·팀 신원을 만들지 않습니다. 실제 신원 공급부가 없는 상태에서 세션 성공을 반환하는 환경변수·설정 우회는 추가하지 않았습니다.

테스트에서만 검증된 사용자·팀·Agent를 반환하는 공급부를 주입합니다. 미래의 실제 공급부는 중앙의 인증·소유권 검증 결과로 Principal을 구성해야 하며 요청의 agentId를 복사하는 것으로 검증을 대체하면 안 됩니다. 공급부는 현재 중앙 연결의 Agent·프로필과 일치하는 범위만 허용하고, 신원 유효기간·폐기·팀 변경 정책도 중앙 계약에 맞춰 연결해야 합니다.

## HTTP 계약

주소는 브로커의 기존 loopback URL입니다. Authorization에는 MCP 자격 파일의 localToken만 허용합니다. CLI controlToken은 세션 권한이 없으며 Origin 헤더가 있는 요청은 거부합니다. GET /health와 POST /shutdown의 기존 권한은 유지합니다.

| 메서드·경로 | 요청 JSON | 성공 data |
| --- | --- | --- |
| POST /v1/sessions | requestId, instanceId, mode: fresh/recover, 선택 agentId | sessionId, principal |
| POST /v1/sessions/{id}/bind | requestId, instanceId, agentId | 같은 sessionId와 바인딩된 principal |
| POST /v1/sessions/{id}/heartbeat | requestId, instanceId | 빈 객체 |
| DELETE /v1/sessions/{id} | requestId, instanceId | 빈 객체 |

```json
{
  "requestId": "claim-1",
  "data": {
    "sessionId": "broker-generated-uuid",
    "principal": { "userId": "verified-user", "teamId": "verified-team" }
  }
}
```

검증된 Agent로 bind하면 principal에 agentId와 선택적 toolType(codex/claude_code)을 포함합니다. 미바인딩 세션의 agentId/toolType은 null 대신 생략합니다. 공유 principal/session schema와 identity fixture를 Rust 테스트에서 소비합니다.

식별자는 공백·제어 문자 없는 1~128 Unicode code point이며 임의로 trim하지 않습니다. 입력의 정의되지 않은 필드·잘못된 타입·명시적 null·중복 필드를 거부합니다. userId/teamId/principal/context/clientTime 등을 요청에 넣어 신원·시간을 덮어쓸 수 없습니다. 요청은 application/json, 최대 128KiB이고 본문 수신과 신원 공급부 호출은 각각 최대 5초로 제한합니다. 오류 원문·토큰·요청 본문은 응답에 복사하지 않습니다.

유효한 요청에는 같은 requestId와 data 또는 error를 반환합니다. 인증·본문 크기/수신 제한·JSON 오류 등으로 requestId를 안전하게 읽을 수 없으면 오류용 UUID를 생성합니다. 정상 MCP 클라이언트의 401 및 control-plane 403 처리는 이 경우에도 인증 거부를 보존합니다.

| HTTP | error.code / scope | 의미 |
| --- | --- | --- |
| 401 | BROKER_UNAUTHORIZED / local_auth | 잘못된 로컬 자격. 중앙 인증 실패와 구분 |
| 403 | LOCAL_AUTH_FORBIDDEN / local_auth | Origin이 있는 요청 |
| 400·413·415·408 | VALIDATION_FAILED / domain | JSON·식별자·크기·Content-Type·본문 수신 시간 거부 |
| 409 | SESSION_EXPIRED / session | 해제·만료·존재하지 않는 세션 |
| 409 | SESSION_REPLACED / session | 다른 fresh 인스턴스로 교체된 세션 |
| 409 | SESSION_CONFLICT / session | 다른 소유자·instanceId 불일치·Agent 전환 시도 |
| 409 | AGENT_UNREGISTERED / session | 살아 있는 unbound 세션의 recover claim으로 bind를 우회하려는 경우 |
| 503 | CENTRAL_UNAUTHENTICATED / domain | 검증된 신원 공급부 미연결 또는 중앙 인증 필요 |
| 503 | CENTRAL_UNAVAILABLE / transport | 신원 공급부 오류·5초 제한 초과. outcome은 unknown |
| 403 | FORBIDDEN / domain | 검증된 사용자·팀·Agent와 claim/bind 불일치 |

성공 DTO는 작게 제한된 식별자들로만 구성돼 256KiB 응답 상한 안에 있습니다. 지원하지 않는 HTTP 메서드는 Axum의 405이며 성공으로 해석하지 않습니다. 세션 경로 등록이나 OPTIONS의 405는 실제 중앙 인증·업무 준비 완료를 뜻하지 않습니다.

## 소유권과 수명

- 같은 활성 instanceId의 claim은 같은 sessionId를 반환하고 lease를 갱신합니다. claim 재시도로 기존 사용자·팀·Agent·도구 메타데이터를 몰래 바꾸지 않습니다. Agent 연결은 명시적 bind에서 수행합니다.
- 새 인스턴스의 fresh는 같은 검증된 Agent의 기존 소유자를 교체할 수 있습니다. recover는 자기 세션 또는 빈 슬롯만 확보하며 다른 소유자가 있으면 충돌합니다. 서로 다른 사용자·팀의 기존 소유권은 fresh로 교체하지 않습니다.
- 교체된 인스턴스의 재claim·heartbeat·해제는 기존 lease가 남은 동안 SESSION_REPLACED입니다. 이후에는 SESSION_EXPIRED이며, 오래된 세션 ID로 새 소유자를 지우거나 갱신하지 않습니다.
- bind는 같은 검증된 사용자·팀과 요청한 Agent의 일치를 확인합니다. 다른 활성 소유자를 빼앗거나 이미 bound인 세션의 Agent를 전환할 수 없습니다. 같은 Agent의 명시적 bind에서는 검증된 toolType 갱신을 허용하지만 이미 확인한 메타데이터의 소실은 거부합니다.
- lease는 브로커의 monotonic clock 기준 90초입니다. 90초 경계부터 만료하며, 만료 후 heartbeat로 되살리지 않습니다. 요청 처리 전 만료 정리와 1초 주기의 백그라운드 정리를 모두 사용합니다. MCP의 30초 갱신과 중앙 JSON Heartbeat의 20초 주기는 별개입니다.
- 재시작 후 세션·소유권은 메모리에서 새로 시작하며 과거 소유자는 보존하지 않습니다. 먼저 유효하게 recover한 인스턴스가 빈 슬롯을 확보합니다.
- 신원 공급부의 await 중에는 저장소 잠금을 잡지 않습니다. bind 응답을 적용하기 직전에 소유권·만료를 다시 확인하고, 종료한 관리자는 늦은 claim 응답도 거부합니다.

## Presence와 종료

시작·health·잘못된 입력·거부된 인증만으로 presence를 확정하지 않습니다. 첫 성공 세션 이후에는 활성 bound 소유자가 하나라도 있으면 ONLINE, 없으면 OFFLINE입니다. unbound 세션 생성은 ONLINE으로 만들지 않습니다.

생성·bind·해제·만료와 presence 게시를 같은 저장소 잠금 안에서 처리합니다. 새 fresh 소유자로 교체하는 사이의 임시 OFFLINE이나 오래된 해제가 새 상태를 덮어쓰는 것을 막습니다. 중앙 단절 중에도 최신 상태는 기존 CentralState에 남고 새 READY 후 재보고합니다. 세션 해제는 브로커를 종료하지 않으며 중앙 작업을 실패·취소로 바꾸지 않습니다.

브로커 취소 시 만료 태스크는 세션·소유권을 정리하고 종료합니다. 이미 종료된 관리자에는 지연된 신원 응답으로 새 세션을 만들 수 없습니다. 네트워크 최종 OFFLINE 정리는 중앙의 연결 종료 정책도 따릅니다.

## 검증과 남은 연결

공유 신원 fixture, 동시 claim, fresh/recover 충돌, 오래된 요청, 90초 경계·갱신, 요청 없는 만료, 지연 bind/claim, HTTP 인증·입력·응답과 실제 로컬 WebSocket presence를 검증합니다. 테스트용 신원은 테스트 코드 내부에서만 제공합니다. 실제 CLI에서는 미구성 신원이 503으로 닫히며 health·중앙 연결·shutdown은 유지되는지 검사합니다.

중앙 신원 전달 API·메시지, Agent 소유권 검증·최초 등록/토큰 발급 순서, 프로필 정책·Agent ID 영속 재사용, 업무 API 7개와 수신함·ACK, 실제 MCP Host·중앙·DB 왕복은 후속 항목입니다. 실행 결과는 [검증 기록](verification.md)을 참고합니다.
