# 중앙 WebSocket 연동 규칙과 Rust 적용 기준

확인일: 2026-10-28 (Asia/Seoul). 중앙 서버 문서와 로컬 서버 소스에 명시된 규칙을 Rust 브로커의 구현 기준으로 기록한다. JSON Heartbeat·4001의 의미는 아래 서버 구현을 확인해 갱신했다. 이 기록은 실제 중앙 배포 환경과 Rust의 연동 완료를 의미하지 않는다.

## 1. 근거 문서와 적용 범위

| 근거 | 확인한 내용 |
| --- | --- |
| [중앙 서버 보안 전략](../../../DevLink_server/docs/security-strategy.md) | 사용자 세션과 Agent 토큰의 분리, WebSocket 인증, Principal 전달, 연결·접속 상태 처리, 인증 모듈의 남은 구현 |
| [2026-10-27 중앙 실시간 연결·라우팅 작업 기록](../../../DevLink_server/docs/bogyeong/2026-10-27.md) | 사용자당 Agent 1개·활성 브로커 1개, Registry 교체·종료 보호, READY·PRESENCE 형식과 시점, DB 검증, Heartbeat 초기안, 업무 라우팅 후속 범위 |
| [현재 로컬 서버 핸들러](../../../DevLink_server/src/main/java/com/devlink/server/handler/DevLinkWebSocketHandler.java)·[Registry](../../../DevLink_server/src/main/java/com/devlink/server/realtime/ConnectionRegistry.java) | JSON HEARTBEAT PING의 검증·수신 시각 갱신과 같은 ping_id의 PONG 응답. 제어 Ping이나 presence는 만료 감시 갱신을 대체하지 않음 |
| [Heartbeat 서비스](../../../DevLink_server/src/main/java/com/devlink/server/service/BrokerHeartbeatService.java)·[설정](../../../DevLink_server/src/main/resources/application.yml) | 유효한 JSON PING 무수신 기본 40초·검사 기본 1초, 만료 시 4001 Heartbeat timeout |

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

로컬 세션 관리자는 생성·bind·해제·만료와 같은 잠금 안에서 위 게시 메서드를 호출하도록 연결했다. 첫 성공 세션 전에는 미확인 상태를 유지하고, 이후 활성 bound 소유자가 있으면 ONLINE, 없으면 OFFLINE이다. 거부된 요청·health는 상태를 활성화하지 않는다. 중앙 신원 공급부는 미연결이며 성공 흐름은 테스트용 검증 신원으로 확인했으므로 실제 Host 세션·운영 중앙까지 포함한 완료로 판정하지 않는다. 로컬 세션 만료를 중앙 작업의 실패·취소로 변환하지 않는다. 상세 내용은 [로컬 세션 API](local-sessions.md)를 따른다.

## 5. 연결 교체·종료와 오래된 이벤트 보호

- 중앙 Registry는 `Agent ID → 현재 열린 WebSocket 연결`을 관리한다.
- 같은 Agent가 새로 연결하면 새 연결을 등록하고 기존 연결을 4002로 종료한다.
- 이전 연결 종료에 실패해도 이전 연결을 활성 연결로 복원하지 않는다.
- 해제는 현재 등록된 실제 세션과 일치할 때만 수행한다. 이전 연결의 지연 종료가 새 연결을 제거하면 안 된다.
- 중앙의 상태 보고·종료 처리는 Agent별 DB 행 잠금으로 직렬화하고, 잠금 획득 후 현재 연결을 다시 확인한다.
- 현재 연결이 실제로 해제되고 새 활성 연결이 없을 때 중앙은 Agent를 OFFLINE 처리한다.
- 실제 전달 가능 여부는 DB 상태와 Registry의 현재 열린 연결을 함께 확인한다.
- 중앙 서버 재시작으로 메모리 연결 정보가 사라지면 브로커 재접속으로 다시 구성한다. 자동 재접속은 Rust 담당 범위다.

현재 Rust의 종료 분류는 다음과 같다. 서버 `BrokerHeartbeatService`가 4001을 `Heartbeat timeout`에 사용하는 것을 확인했으므로 기존의 4001 인증 실패 분류를 수정했다. HTTP 401/403 인증 실패와 WebSocket Close 4001은 구분한다.

| 응답·종료 | 현재 Rust 처리 |
| --- | --- |
| Upgrade HTTP 401·403 | `AuthFailed`, 자동 재접속 중단 |
| Close 4001 (Heartbeat timeout) 또는 JSON PONG 응답 제한 초과 | `autoReconnect=true`이면 백오프 재접속, false이면 `Disconnected` |
| Close 4002 | `Replaced`, 자동 재접속 중단 |
| Close 1008 또는 잘못된 초기 READY | `ProtocolViolation`, 자동 재접속 중단 |
| 일시적 단절·READY 시간 초과 | `autoReconnect` 설정에 따라 지수 백오프로 재접속 |
| 브로커 종료 요청 | WebSocket·재접속 대기 취소 후 종료 |

## 6. JSON Heartbeat와 만료 처리

2026-10-27 문서는 하트비트를 초기안으로 남겼지만 현재 로컬 서버에는 JSON Heartbeat와 만료 정리가 구현돼 있다. Rust도 이 JSON 규칙을 따른다. 아래 서버 값은 로컬 소스의 기본값이며 배포 환경의 실제 설정을 확인한 것은 아니다.

```json
{"type":"HEARTBEAT","payload":{"kind":"PING","ping_id":"unique-ping-id"}}
```

서버는 같은 `ping_id`를 사용해 다음과 같이 응답한다.

```json
{"type":"HEARTBEAT","payload":{"kind":"PONG","ping_id":"unique-ping-id"}}
```

| 구분 | 기준 |
| --- | --- |
| 현재 Rust | READY 후 첫 PING까지 20초, 이후 20초 간격 JSON PING. 각 PING의 동일 ping_id PONG을 전송 시점부터 최대 20초 대기 |
| 현재 로컬 서버 | 등록 시점부터 감시. 유효한 JSON PING만 수신 시각 갱신, 기본 40초 이상 무수신이면 다음 검사에서 4001로 종료 |
| 개발용 Rust Mock | 같은 JSON PING/PONG과 40초 무수신 만료·4001 적용. presence·제어 프레임으로 감시 시간 갱신하지 않음 |
| MCP 로컬 세션 | MCP 갱신 기본 30초, 브로커 lease 90초. 중앙 Heartbeat와 별도 |

각 PING은 UUID ping_id를 사용하고 응답 대기 중에는 새 PING으로 기존 요청·제한 시간을 덮어쓰지 않는다. 잘못된 형식·다른 ID·이전 PONG·일반 JSON·WebSocket 제어 Ping/Pong은 PONG 대기를 완료하거나 연장하지 않는다. 제어 Ping에는 전송 계층의 Pong을 응답하지만 이를 JSON Heartbeat로 인정하지 않는다. PONG이 만료되면 연결을 정리하고 재접속 설정을 따른다.

재접속할 때는 이전 연결의 식별자·PONG 대기를 폐기하고 새 READY 후 최신 확인된 presence를 다시 보고한다. 4002 교체·1008 정책 위반·Upgrade 401/403은 계속 자동 재접속을 중단한다. 연결 유지·PONG 불일치/재사용·만료·4001 재접속과 autoReconnect=false의 로컬 검증은 [검증 기록](verification.md)을 참고한다.

실제 주기의 연결 유지는 CLI 통합 테스트 `json_heartbeat_keeps_cli_connected_for_45_seconds_without_reconnect`로 자동 검증한다. JSON PING 무수신 시 40초 만료·4001로 종료하는 Mock을 사용하고, 시간 단축 없이 45초간 Connected·연결 1회·만료 0회와 서로 다른 ping_id의 반복 수신을 확인한다. 재접속 후 Connected로 돌아온 경우는 연결 횟수 검사에서 실패한다. 기본 테스트 실행에 포함하며 실제 중앙 배포 환경 검증과는 구분한다.

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

1. 구현한 로컬 세션·presence에 실제 중앙 IdentityProvider를 연결한다. 현재 기본 공급부는 신원을 만들지 않고 503 CENTRAL_UNAUTHENTICATED로 닫힌다.
2. 실제 중앙의 AgentTokenVerifier·Agent 토큰으로 Upgrade → READY → PRESENCE → DB 상태 갱신을 검증한다.
3. 세션 해제·만료, 중앙 재시작·재접속, 동일 Agent 연결 교체와 오래된 이벤트가 새 상태에 영향을 주지 않는지 확인한다.
4. 구현한 JSON Heartbeat·4001 재접속을 실제 중앙 배포 설정·인증 환경에서도 확인한다.
5. 중앙 업무 라우팅 계약에 맞춰 4단계 업무 API와 5단계 수신함·ACK를 구현한다.

현재 구현 근거: [프로토콜 DTO](../crates/devlink-broker/src/protocol.rs), [중앙 연결 루프](../crates/devlink-broker/src/central.rs), [로컬 HTTP 라우터](../crates/devlink-broker/src/broker.rs), [MCP principal 스키마](../../packages/mcp-server/src/broker/schemas.ts).
