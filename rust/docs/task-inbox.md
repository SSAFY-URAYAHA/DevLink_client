# 작업 수신함·ACK

2026-10-29. 중앙의 기존 `TASK_CREATED`와 `ACK` 형식으로 Rust 로컬 수신 경로를 구현했다. 실제 운영 중앙·MCP Host의 업무 왕복 완료를 의미하지 않는다. 저장 후 ACK 정책은 로컬 구현안이며 중앙 담당자와 최종 합의해야 한다.

## 수신 순서

1. 유효한 `BROKER_READY` 이후 `TASK_CREATED`를 받는다. READY에 사용자·팀 필드를 임의로 추가하지 않았다.
2. 양의 숫자 ID·비어 있지 않은 event_id/prompt/created_at, 현재 인증된 Agent와 receiver_agent_id의 일치를 검증한다. created_at은 시간대를 추정하지 않고 서버 문자열 그대로 보존한다.
3. `.devlink-runtime/inbox.json`에 기록한다. 임시 파일 비공개 권한 → 쓰기 → sync_all → 권한 검사 → 원자 교체 순서이며 Unix는 부모 디렉터리도 sync한다.
4. 저장 성공 뒤 현재 WebSocket으로 `{"type":"ACK","payload":{"event_id":"현재 전송 ID","task_id":101}}`를 보낸다. 작업 수락·실행·완료는 하지 않는다.

수신함은 설정 디렉터리 단위로 보존되며 stop/start·토큰 회전에 삭제하지 않는다. snapshot schema_version은 1이다. 정규화된 중앙 URL + 수신 Agent ID + task_id로 중복을 판별한다. 다른 중앙 URL·Agent의 기록은 내부 조회에서도 분리한다. URL의 host 별칭은 자동 통합하지 않으며 중앙 DB 자체 교체 시 과거 ID 재사용 처리는 후속 운영 계약이다.

같은 키와 같은 payload의 재수신은 새 항목을 만들지 않고 현재 전송의 event_id로 다시 ACK한다. event_id만 다른 재전달은 허용하지만 prompt·team·생성 시각 등 payload가 바뀌면 원본을 보존하고 ACK하지 않는다. 이것은 중복 수신함 항목 방지이며 Agent 실행의 exactly-once 보장이 아니다. 실제 실행 소유권·업무 상태 연결은 미구현이다.

## 실패·자원 제한

- 신규 저장 실패·충돌·수신함 용량 초과·미구성 수신함은 ACK하지 않고 중앙 연결을 닫아 `InboxFailed`로 표시한다. 자동 재접속은 중단한다. 로컬 HTTP health·stop과 세션 lease 관리는 유지되므로 원인을 해결한 뒤 stop/start한다.
- 잘못된 수신자나 유효하지 않은 작업 값은 `ProtocolViolation`이다. 초기 READY 이전 작업도 거부한다. 알 수 없거나 역직렬화할 수 없는 READY 이후 JSON은 기존대로 무시하며 ACK하지 않는다.
- 손상·지원하지 않는 schema·중복 키·안전하지 않은 경로/권한의 기존 수신함은 기동 시 거부한다. 조용히 초기화하지 않는다. 원본을 보존한 뒤 복구해야 한다.
- 파일은 최대 64 MiB, 기록은 전체 1,024건이다. 자동 삭제·오래된 항목 퇴출은 없다. 확정된 보관/정리 정책이 생기면 확장한다. 이는 로컬 자원 제한이며 중앙 API의 합의된 상한이 아니다.
- 연결당 저장 대기 최대 16건, 실제 blocking writer는 Inbox당 1개다. 파일·ACL 처리는 spawn_blocking에서 수행해 Heartbeat·종료 처리를 막지 않는다. 연결당 대기가 가득 차면 해당 연결을 닫고 기존 재접속 정책을 따른다. 저장하지 못한 메시지에 ACK하지 않는다.
- 단절·취소 직전에 이미 시작된 디스크 쓰기는 완료될 수 있다. 그 결과의 ACK를 새 연결에 옮겨 보내지 않는다. 이후 같은 작업을 재수신하면 저장된 기록으로 중복을 확인한다.
- ACK의 중앙 기본 제한은 5초다. 느린 디스크나 큐 대기로 이 제한을 넘을 수 있으며 중앙 ACK 성공을 보장하지 않는다. 늦은 ACK·누락 조회·재전달은 공동 계약으로 남아 있다.

## Mock 데모

개발용 Mock에서만 다음 값을 사용한다. 실제 작업 실행·운영 인증을 흉내 내는 설정은 아니다.

```powershell
# 첫 번째 터미널: 동일 task_id를 서로 다른 event_id로 두 번 전달
$env:DEVLINK_TOKEN = 'local-development-token'
$env:DEVLINK_MOCK_TASK_DEMO = '1'
cargo run -p devlink-broker --example mock_server

# 두 번째 터미널: 별도 테스트 설정 사용
$env:DEVLINK_TOKEN = 'local-development-token'
cargo run -p devlink-cli -- --config .\.demo\config.json init
cargo run -p devlink-cli -- --config .\.demo\config.json start
cargo run -p devlink-cli -- --config .\.demo\config.json status
cargo run -p devlink-cli -- --config .\.demo\config.json stop
```

Mock에 ACK 2건이 표시되지만 `.demo/.devlink-runtime/inbox.json`에는 1건만 저장된다. 재시작 후에도 같은 작업은 추가되지 않는다. 데모 재전달은 Mock의 명시적인 테스트 동작이며 운영 중앙의 자동 재전송을 뜻하지 않는다.

## 계약이 필요한 후속 작업

- 중앙 REST 인증, Agent 등록/토큰 발급, 검증된 userId/teamId 전달과 실제 IdentityProvider.
- MCP 업무 경로와 get_tasks 수신함 노출·상태 병합. 지금은 내부 `Inbox::list_for`만 제공하며 공개 HTTP API를 추가하지 않았다.
- 중앙 누락 조회·늦은 ACK·전달 결과 조회와 재전달 식별자 정책.
- 작업 수락·실패·완료, version 충돌·결과 제출, 실제 Agent 실행·완료 알림.

중앙 구현 기준: [5891ef2의 생성·ACK 문서](https://github.com/SSAFY-URAYAHA/DevLink_server/blob/5891ef2a6adb4a3d3a7c7e89a5c064386a9c1bb9/docs/task-creation-implementation.md). 이 버전은 ACK_PENDING/ACK_RECEIVED/DELIVERY_NOT_SENT/DELIVERY_UNKNOWN을 사용하며 생성 HTTP는 ACK를 기다리지 않는다. 로컬 수신은 이 상태를 중앙에 임의 보고하지 않고 기존 ACK만 전송한다.
