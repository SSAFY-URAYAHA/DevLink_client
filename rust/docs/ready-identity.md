# READY 신원과 로컬 세션

2026-10-29 후속 구현: 기본 CLI는 기존 `GET /api/v1/agent/context`를 같은 중앙 Agent Bearer로 조회해 검증된 신원을 공급한다. 아래의 READY 필드 누락에 따른 차단 설명은 이전 기록이다. 중앙 `30b7d70`의 READY 자체는 변경하지 않는다. 실제 중앙/DB 검증 결과는 [세션 검증 기록](session-story-verification.md)의 최신 항목을 따른다.

## 현재 운영 경로: READY + 실시간 context 조회

- CLI는 검증된 중앙 origin에 고정된 ContextClient를 CentralState에 주입한다. MCP에는 중앙 토큰을 전달하지 않는다. 수동 테스트용 공급부와 기존 확장 READY 테스트는 이 클라이언트를 주입하지 않는 별도 경계다.
- claim/bind/heartbeat마다 기존 `/api/v1/agent/context`를 조회한다. 캐시만으로 lease를 갱신하지 않는다. JSON `success=true`, 양의 Java Long 범위의 숫자 `ownerUserId/teamId/agentId`가 필요하다. 요청의 ID·config·bootstrap 파일로 신원을 추측하지 않는다. toolType은 기존과 같이 생략한다.
- 현재 WS가 Connected이고 REST의 agentId와 READY의 Agent가 같아야 한다. READY에 user/team이 있으면 그 값도 일치해야 한다. 응답 적용 때 WS generation·connection identity를 다시 비교한다. 조회 중 재접속하면 늦은 응답은 Unavailable로 거부한다.
- REST는 redirect·proxy·자동 retry·cookie를 사용하지 않는다. 응답 본문까지 총 3초, 256KiB 상한이다. 외부 오류 본문이나 자격을 로컬 응답에 복사하지 않는다. 401/403은 중앙 인증 거부이고 나머지 HTTP 오류·형식/전송 오류는 중앙 불가로 처리한다.
- 중앙 인증 거부·신원 모순은 subject epoch를 바꾸고 로컬 상태를 AuthFailed/OFFLINE으로 전환한다. 같은 WS를 legacy READY로 재해석해 다시 권한을 얻을 수 없다. 새 연결 후 재검증이 필요하다. 기존 세션은 epoch 변화로 만료된다.
- 같은 신원의 새 WS는 lease 안의 소유권을 유지한다. legacy READY에서도 agent_id가 바뀌면 즉시 subject epoch를 증가시켜 이전 소유권을 무효화한다. 중간 Agent의 context 조회 없이 A → B → A로 돌아와도 이전 세션은 복원하지 않는다. legacy READY 재접속 직후에는 context 확인까지 OFFLINE을 유지하고, 백그라운드 만료 정리도 이전 ONLINE을 재게시하지 않는다. 성공한 claim/bind/heartbeat가 현재 신원을 확인한 뒤 실제 슬롯에 따라 presence를 게시한다.
- DELETE는 현재 sessionId/instanceId 검증으로 해제하며 중앙 재인증을 요구하지 않는다. 중앙이 불가해도 로컬 해제와 OFFLINE 정리가 가능하다.

Agent 등록·기존 Agent 선택·토큰 안전 저장을 자동화하는 -91 CLI bootstrap 명령은 이 변경에 포함하지 않는다. 실제 중앙 검증의 자격은 기존 사용자 세션·CSRF·Agent API로 합성 계정에서 준비하며 Agent 생성 API를 복제하지 않는다.

2026-10-29 중앙 소스 재대조: 서버 develop `30b7d7073150771b2e83877d7edee71baee7c56f`의 `DevLinkWebSocketHandler`는 아직 `connection_id/agent_id`만 전송한다. 아래 확장 READY는 Rust가 지원하는 계약이며 해당 중앙 커밋의 제공 형식이 아니다. 현 조합의 세션 발급은 `CENTRAL_UNAUTHENTICATED`로 차단된다. 기존 `GET /api/v1/agent/context`는 `ownerUserId/teamId/agentId`를 제공하지만 현재 ReadyIdentityProvider는 이를 조회하지 않는다. READY 확장 반영 또는 REST 공급부의 신선도·현재 WS 신원 일치·재접속/폐기 정책 연결 전에는 실제 연동 완료로 처리하지 않는다.

2026-10-29. 중앙 담당자 답변에 따라 READY 신원 확장과 저장 후 ACK를 적용한다. 작업 REST의 Agent Bearer 인증도 방향이 확인됐다. 토큰 발급·누락 복구·재전달 정책은 사용자가 제안안을 구현 기준으로 채택했으며 중앙 API의 실제 경로·DTO·배포 완료를 의미하지 않는다.

## READY 계약

```json
{
  "type": "BROKER_READY",
  "payload": {
    "connection_id": "central-session-id",
    "agent_id": 12,
    "user_id": 3,
    "team_id": 1
  }
}
```

ID는 기존 중앙 숫자 ID와 같은 양의 Java Long 범위로 받는다. Rust는 로컬 Principal에 `{"userId":"3","teamId":"1","agentId":"12"}`로 변환한다. 문자열 ID·null·0·음수·범위 초과·사용자/팀 중 한 필드만 있는 READY는 거부한다. 중앙의 최종 예시가 다른 타입이면 배포 전 DTO를 맞춰야 한다.

점진적 배포를 위해 user_id/team_id가 모두 없는 기존 READY는 WS 연결·Heartbeat에 한해 허용한다. 이 경우에는 세션 신원을 만들지 않고 503 CENTRAL_UNAUTHENTICATED를 반환한다. 정상 READY 전체 신원이 있어야 실제 ReadyIdentityProvider가 세션을 발급한다. Mock 예제는 합성 user_id=1/team_id=1/agent_id=1을 제공하며 운영 인증을 대체하지 않는다.

## 세션·소유권 처리

- 기본 CLI는 UnconfiguredIdentityProvider 대신 현재 중앙 연결의 ReadyIdentityProvider를 사용한다.
- READY가 확인된 Connected 상태에서만 사용자·팀을 공급한다. 연결 중·단절·재접속 상태는 503 CENTRAL_UNAVAILABLE, 중앙 인증 실패나 구버전 READY의 신원 누락은 503 CENTRAL_UNAUTHENTICATED다.
- agentId 없는 claim은 기존 MCP 계약처럼 unbound Principal을 반환한다. 같은 READY의 agent_id로 bind하거나, 검증된 동일 agentId를 명시한 claim만 bound 세션을 얻는다. 다른 Agent를 요청하면 422 FORBIDDEN(domain)이다.
- toolType은 READY에 없으므로 config.agent나 요청 입력에서 추측하지 않고 생략한다. Agent 등록·메타데이터 연결은 후속 계약이다.
- 동일 사용자·팀·Agent로 재접속하면 lease 안의 로컬 세션을 유지한다. 연결 ID 변경만으로 소유권을 교체하지 않는다.
- 사용자·팀·Agent 조합이 달라지거나 전체 신원이 없는 READY로 바뀌면 subject epoch를 증가시키고 이전 세션·소유권을 무효화한다. A → B → A의 빠른 변경도 이전 세션을 복원하지 않는다. 오래된 세션 heartbeat/delete는 SESSION_EXPIRED다.
- 신원 변경 즉시 이전 소유권의 ONLINE을 새 신원에 보고하지 않도록 OFFLINE으로 바꾼다. 세션 관리자는 변경 알림과 요청 시 epoch를 확인해 정리한다.
- claim/bind/heartbeat의 최종 신원 검증·저장·presence 게시를 중앙 신원 read lock 안에서 처리한다. 이전 신원으로 시작한 지연 처리가 새 신원의 세션으로 적용되지 않게 한다.
- 중앙 연결이 끊긴 동안 heartbeat는 lease를 연장하지 않는다. 명시적 해제는 허용하며 같은 신원 재접속 후 lease가 유효하면 recover할 수 있다. 로컬 세션 만료는 중앙 작업 실패·취소가 아니다.

## 다음 연결 지점

| 항목 | 적용 기준 | 현재 남은 내용 |
| --- | --- | --- |
| 업무 REST | 작업 API에 중앙 Agent Bearer 사용, localToken과 분리 | 중앙 REST 필터 적용·실제 호출 경로와 DTO·권한 검증 |
| 토큰 발급·관리 | 사용자 인증 → Agent 등록/기존 Agent 확인 → 토큰 확보 → WS 연결 | 로그인·등록·발급·갱신·폐기 API와 안전한 저장 수명 주기 |
| 등록 주체 | CLI/MCP 역할 중복 방지 | MCP register_agent와 최초 CLI 등록의 최종 역할 연결 |
| ACK | 디스크 저장 후 기존 event_id/task_id로 전송, 서버 기본 5초 유지 | 실제 중앙에서 저장·ACK 지연 측정 후 필요할 때 시간 조정 |
| 복구·재전달 | 누락 조회로 수신함 복구, 같은 작업 중복 저장·실행 방지 방향 | 조회·페이지·수신 확인 경로, event_id 정책, 실행 소유권 |

경로가 없는 API를 임의로 구현하거나 성공 응답을 만드는 공급부는 추가하지 않았다. 이미 분리한 IdentityProvider와 Inbox 경계를 기준으로 후속 중앙/MCP 계약을 연결한다. 검증 범위는 [로컬 검증 기록](verification.md)을 따른다.
