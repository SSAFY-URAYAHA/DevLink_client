# 중앙 WebSocket 연동 규칙과 Rust 적용 기준

확인일: 2026-10-28 (Asia/Seoul). 중앙 서버 문서에 명시된 규칙을 Rust 브로커의 구현 기준으로 기록한다. 문서상의 규칙, 현재 Rust 구현, 초기안과 후속 연동 항목을 구분한다. 이 기록은 실제 중앙 서버와 Rust의 연동 완료를 의미하지 않는다.

## 1. 근거 문서와 적용 범위

| 근거 | 확인한 내용 |
| --- | --- |
| [중앙 서버 보안 전략](../../../DevLink_server/docs/security-strategy.md) | 사용자 세션과 Agent 토큰의 분리, WebSocket 인증, Principal 전달, 연결·접속 상태 처리, 인증 모듈의 남은 구현 |
| [2026-10-27 중앙 실시간 연결·라우팅 작업 기록](../../../DevLink_server/docs/bogyeong/2026-10-27.md) | 사용자당 Agent 1개·활성 브로커 1개, Registry 교체·종료 보호, READY·PRESENCE 형식과 시점, DB 검증, Heartbeat 초기안, 업무 라우팅 후속 범위 |

두 문서에 명시된 인증·연결·접속 상태 규칙을 우선 적용한다. 같은 Agent의 중앙 WebSocket 연결 교체와 MCP 로컬 세션의 `fresh/recover` 소유권은 각각 별도로 구현한다. 중앙 문서가 사용하는 활성 브로커 1개는 중앙 WebSocket 연결 기준이며, MCP 프로세스의 세션 수를 정하는 계약으로 확대 해석하지 않는다.

## 2. 연결과 인증

- 중앙 연결 경로는 raw WebSocket `/ws/broker`다.
- WebSocket Upgrade 요청의 `Authorization: Bearer <Agent 토큰>`으로 인증한다.
- 사용자 로그인은 세션 방식이고, Agent 인증은 별도다. 사용자 세션으로 WebSocket Agent 인증을 대체하지 않으며 Agent 인증을 사용자 REST API 권한으로 사용하지 않는다.
- 중앙의 전용 인증 필터가 `AgentTokenVerifier` 결과를 인증 Principal로 전달한다. 검증 구현 Bean이 없거나 인증이 실패하면 중앙은 401로 연결을 거부한다.
- 중앙은 설정된 Origin 정책을 적용한다. Origin 검증만으로 인증을 대신하지 않는다. 현재 Rust는 Upgrade 요청에 Origin·Cookie를 추가하지 않는다.
- Agent 토큰의 DB 저장 계약은 원문이 아닌 해시다. 발급·만료·폐기·연결 종료 정책과 실제 검증 구현은 중앙 인증 담당 범위의 후속 항목이다.
- Rust는 중앙 Agent 토큰과 MCP의 로컬 토큰을 분리한다. 중앙 연결에는 현재 `DEVLINK_TOKEN`을 사용한다.

현재 Rust는 Upgrade 헤더로 인증하며, 연결 후 별도의 `auth`·`hello` JSON을 보내지 않는다. 운영용 토큰 검증 Bean과 실제 토큰을 사용한 중앙 연동은 별도 확인이 필요하다.

## 3. 연결 등록과 BROKER_READY

중앙은 인증된 `AgentPrincipal`의 Agent ID로 `ConnectionRegistry`에 연결을 등록하고, 현재 등록된 연결에 `BROKER_READY`를 보낸다. 클라이언트가 보낸 Agent ID를 인증 근거로 사용하지 않는다.

```json
{
  "type": "BROKER_READY",
  "payload": {
    "connection_id": "current-websocket-session-id",
    "agent_id": 11
  }
}
```

- `BROKER_READY`의 의미는 중앙 서버와 브로커 사이의 인증·연결 등록 완료다.
- WebSocket 연결 등록은 사용자나 Agent를 DB에 새로 등록하는 작업과 별개다.
- 토큰 검증 성공이나 READY 전송만으로 로컬 Agent를 ONLINE 처리하지 않는다.
- Rust는 유효한 READY 수신까지 `Authenticating`을 유지한다. 현재 DTO는 비어 있지 않은 `connection_id`와 양의 정수 `agent_id`를 검사한다.
- 현재 READY 계약에는 `userId/teamId`가 없다. 이 필드가 없다는 이유로 중앙 WebSocket 계약 자체를 미정으로 취급하거나 임의 필드를 추가하지 않는다.
- 연결이 종료·교체되면 Rust는 해당 연결의 식별자를 무효화한다. 이전 연결의 지연 처리가 새 연결의 상태·식별자를 덮어쓰지 못하게 한다.

## 4. AGENT_PRESENCE와 온라인 판정

로컬 Agent와 브로커 사이의 접속 상태는 다음 형식으로 보고한다.

```json
{
  "type": "AGENT_PRESENCE",
  "payload": {
    "status": "ONLINE"
  }
}
```

| 항목 | 중앙 문서의 규칙 |
| --- | --- |
| 허용 상태 | `ONLINE`, `OFFLINE` |
| Agent 식별 | 메시지 본문 대신 현재 인증된 연결의 Agent를 사용 |
| 전송 시점 | 최초 READY 수신 직후, 로컬 Agent 상태 변경 시, 재접속 후 READY 수신 직후 |
| 중앙 검증 | 인증 정보·현재 연결 여부, DB Agent의 소유자·팀과 인증 정보의 일치 여부 확인 |
| ONLINE 저장 | `agents.status`와 서버 현재 시각의 `last_seen_at` 갱신 |
| OFFLINE 저장 | 상태만 갱신하고 마지막 온라인 확인 시각 유지 |
| 동일 상태 재보고 | 오류 없이 처리 |
| 잘못된 메시지 | 잘못된 JSON·필드·타입·상태와 JSON 뒤 추가 값은 1008로 종료 |
| ACK | `AGENT_PRESENCE`에는 별도 ACK 없음 |

Rust는 상태 보고에 `agent_id`, 사용자·팀 식별자, 클라이언트 시각을 덧붙이지 않는다. 중앙이 인증된 Agent와 서버 시각으로 검증·저장한다.

현재 Rust는 `watch`로 최신 로컬 상태를 보존하며, 확인된 상태가 있으면 최초·재접속 READY 직후 보고하고 연결 유지 중 변경도 같은 WebSocket으로 보고한다. 로컬 상태가 미확인인 경우에는 ONLINE/OFFLINE을 추측해서 보내지 않는다. 같은 상태의 중복 갱신은 생략하며 연결 전·재접속 중 지나간 상태를 큐에 쌓아 재생하지 않는다. PRESENCE ACK를 기다리지 않고 전송 실패 시 연결 재시도 정책을 따른다.

로컬 HTTP 핸들러와 중앙 연결 태스크는 같은 `CentralState`를 공유한다. 세션 관리가 확인한 상태는 `CentralState::set_presence(PresenceStatus::Online/Offline)`으로 게시한다. `SharedPresence`는 `watch::Sender<Option<PresenceStatus>>`이며 기존 `Arc<RwLock<...>>` 직접 쓰기 방식 대신 이 게시 메서드를 사용한다. 기본 상태는 미확인(`None`)이다.

3단계 로컬 세션 구현에서는 활성 세션의 생성·bind·해제·만료에 따라 위 게시 메서드를 호출해야 한다. 초기 로컬 상태 확인과 실제 세션 이벤트 연결은 아직 구현되지 않았으므로 실제 Host 세션까지 포함한 완료로 판정하지 않는다. 로컬 세션 만료를 중앙 작업의 실패·취소로 변환하지 않는다.

## 5. 연결 교체·종료와 오래된 이벤트 보호

- 중앙 Registry는 `Agent ID → 현재 열린 WebSocket 연결`을 관리한다.
- 같은 Agent가 새로 연결하면 새 연결을 등록하고 기존 연결을 4002로 종료한다.
- 이전 연결 종료에 실패해도 이전 연결을 활성 연결로 복원하지 않는다.
- 해제는 현재 등록된 실제 세션과 일치할 때만 수행한다. 이전 연결의 지연 종료가 새 연결을 제거하면 안 된다.
- 중앙의 상태 보고·종료 처리는 Agent별 DB 행 잠금으로 직렬화하고, 잠금 획득 후 현재 연결을 다시 확인한다.
- 현재 연결이 실제로 해제되고 새 활성 연결이 없을 때 중앙은 Agent를 OFFLINE 처리한다.
- 실제 전달 가능 여부는 DB 상태와 Registry의 현재 열린 연결을 함께 확인한다.
- 중앙 서버 재시작으로 메모리 연결 정보가 사라지면 브로커 재접속으로 다시 구성한다. 자동 재접속은 Rust 담당 범위다.

현재 Rust의 종료 분류는 다음과 같다. 4002·1008은 위 중앙 문서와 대응하며, 403·4001의 처리도 Rust에 구현돼 있다. 제공된 두 중앙 문서만으로 403·4001의 모든 서버 발생 조건이 확인된 것은 아니다.

| 응답·종료 | 현재 Rust 처리 |
| --- | --- |
| Upgrade 401·403 또는 Close 4001 | `AuthFailed`, 자동 재접속 중단 |
| Close 4002 | `Replaced`, 자동 재접속 중단 |
| Close 1008 또는 잘못된 초기 READY | `ProtocolViolation`, 자동 재접속 중단 |
| 일시적 단절·READY 시간 초과 | `autoReconnect` 설정에 따라 지수 백오프로 재접속 |
| 브로커 종료 요청 | WebSocket·재접속 대기 취소 후 종료 |

## 6. Heartbeat: 초기안과 현재 구현

중앙 작업 기록은 Heartbeat와 무응답 연결 정리를 남은 작업으로 표시한다. 아래 중앙 수치는 **논의한 초기안**이며, 최종 합의·구현 완료로 기록하지 않는다.

| 구분 | 기준 |
| --- | --- |
| 중앙 문서 초기안 | PING 20초 간격, PONG 대기 20초, 서버의 PING 무수신 판정 40초 |
| 현재 Rust | READY 후 WebSocket Ping 5초 간격, 마지막 수신 후 15초 초과를 Heartbeat tick에서 감지하면 재접속 |
| MCP 로컬 세션 | MCP 갱신 기본 30초, 브로커 lease 90초. 중앙 Heartbeat와 별도 |

현재 Rust의 무응답 판정은 Pong만이 아니라 수신 Text·Ping·Pong으로 마지막 수신 시각을 갱신한다. 중앙의 최종 Heartbeat 주기·응답 판정·종료 동작과 실제 연동할 때 정합성을 확인한다. 이번 문서 정리로 Rust 상수를 변경하지 않는다.

## 7. MCP 신원·등록 및 업무 라우팅과의 경계

중앙은 Agent 토큰으로 인증된 Principal과 DB의 Agent 소유자·팀을 대조한다. 따라서 WebSocket 인증·presence의 사용자·팀 검증 책임은 이미 중앙 규칙에 정의돼 있다.

현재 MCP 로컬 세션은 `principal.userId/teamId`를 필수로 요구한다. 제공된 중앙 문서에는 이 값을 브로커에 전달하는 응답이나 조회 API가 명시돼 있지 않다. 이 부분은 **MCP 로컬 세션에 검증된 신원을 전달하는 후속 연결 항목**으로 남긴다. Agent ID나 도구 입력으로 사용자·팀을 추측하지 않는다. 중앙 READY를 임의로 확장하기 전에 검증된 신원 전달 경로를 확인한다.

추가 연결 항목은 다음과 같다.

- 중앙 Agent 토큰 발급·기존 DB Agent와 MCP 최초 `register_agent`의 실행 순서 및 역할 연결.
- 중앙의 숫자형 `agent_id`와 MCP의 문자열 식별자 사이 표현 규칙, 검증된 Agent ID의 영속 저장·재사용.
- 현재 중앙의 사용자당 Agent 1개·활성 브로커 1개 기준에 맞는 로컬 프로필·세션 소유권 처리.
- 중앙 업무 요청·응답의 DTO, 요청 식별·권한 확인·timeout 후 결과 조회.
- `TASK_CREATED` 라우팅, 수신함 디스크 기록 후 ACK, 중복 방지·전달 결과 저장, 상태·결과 알림.

중앙 보안 전략은 업무 메시지 라우팅을 현재 미제공으로 명시한다. 작업 기록도 TASK_CREATED·수신함·ACK를 남은 작업으로 분류한다. PRESENCE의 ACK 미전송 규칙을 업무 전달 ACK에 적용하지 않는다.

## 8. 다음 구현·검증 순서

1. 기존 READY·PRESENCE 규칙에 맞춰 3단계 로컬 세션 상태와 중앙 presence를 연결한다. MCP 세션 principal에 필요한 검증된 신원 전달 경로는 별도로 연결한다.
2. 실제 중앙의 AgentTokenVerifier·Agent 토큰으로 Upgrade → READY → PRESENCE → DB 상태 갱신을 검증한다.
3. 세션 해제·만료, 중앙 재시작·재접속, 동일 Agent 연결 교체와 오래된 이벤트가 새 상태에 영향을 주지 않는지 확인한다.
4. 중앙 Heartbeat 최종값과 무응답 정리 계약을 확인하고 Rust 동작을 맞춘다.
5. 중앙 업무 라우팅 계약에 맞춰 4단계 업무 API와 5단계 수신함·ACK를 구현한다.

현재 구현 근거: [프로토콜 DTO](../crates/devlink-broker/src/protocol.rs), [중앙 연결 루프](../crates/devlink-broker/src/central.rs), [로컬 HTTP 라우터](../crates/devlink-broker/src/broker.rs), [MCP principal 스키마](../../packages/mcp-server/src/broker/schemas.ts).
