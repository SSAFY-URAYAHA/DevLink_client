# TASK_CREATED 수신함 (S15P31D108-95)

2026-10-29. 중앙 develop `5891ef2a6adb4a3d3a7c7e89a5c064386a9c1bb9`의 TaskCreatedMessage와 WebSocket ACK handler를 소비하는 client 구현이다. 실제 중앙 로그인/Agent 토큰 verifier와 -92 로컬 세션은 아직 제공되지 않았으므로 **실제 중앙 DB·두 사용자 MVP 완료는 미검증**이다. 인증·bootstrap 결정과 남은 조건은 [계약 PR #15](https://github.com/SSAFY-URAYAHA/DevLink_client/pull/15)를 참조한다.

## 수신과 ACK

production broker는 프로필별 private inbox를 중앙 연결 루프에 전달한다. 유효한 BROKER_READY 이후의 TASK_CREATED만 처리한다. READY의 현재 Agent와 receiver_agent_id가 다르거나, ID가 양수 signed64-bit 범위를 벗어나거나, 필수 필드/타입/크기가 맞지 않으면 기록·ACK하지 않는다. TASK_CREATED를 READY 전에 받으면 기존 초기 프로토콜 위반 정책으로 중단한다. 메시지 원문·prompt를 진단 로그에 출력하지 않는다.

```json
{
  "type": "TASK_CREATED",
  "event_id": "central-event-uuid",
  "payload": {
    "task_id": 100,
    "team_id": 3,
    "requester_agent_id": 11,
    "receiver_agent_id": 22,
    "prompt": "사용자가 수락한 다음 수정할 작업",
    "created_at": "2026-10-29T12:00:00"
  }
}
```

저장 순서:

1. 현재 연결 Agent·wire 형식을 검증한다. payload를 로컬 user/team principal로 승격하지 않는다.
2. 최대5ms 동안 모인 이벤트를 같은 snapshot에 묶는다. 각 항목의 충돌/용량을 따로 판정하고, 받아들인 항목만 같은 디렉터리의 private 임시 파일에 쓰고 flush한다.
3. 원자 교체 후 최종 권한·flush를 확인한다. Unix에서는 디렉터리도 fsync한다.
4. 성공한 기록에서 얻은 원래 event_id/task_id만 **같은 연결 세대의 소켓**으로 ACK한다. 연결이 종료/교체되면 대기 중 완료가 새 소켓에 ACK를 보내지 못한다.

```json
{"type":"ACK","payload":{"event_id":"central-event-uuid","task_id":100}}
```

ACK는 로컬 수신함에 저장됐다는 의미다. 사용자 읽기·명시적 수락·IN_PROGRESS·코드 수정·완료를 의미하지 않는다. 이 기능은 업무 상태 변경/중앙 재전송/Agent 자동 실행을 하지 않는다. 중앙은 현재 유효한 ACK를 저장할 때만 ACK_RECEIVED/deliveredAt을 기록하며 업무 상태는 바꾸지 않는다.

같은 event_id/같은 payload의 중복은 다시 flush한 기존 기록을 확인하고 ACK하되 journal을 늘리지 않는다. 같은 event_id의 다른 payload 또는 같은 task_id의 바뀐 생성 payload는 충돌로 거부한다. 서로 다른 event_id로 같은 작업이 재수신되면 각각의 event 기록을 유지하되 CLI 목록에는 task_id당 한 번만 표시한다. 프로세스 재시작 후에도 저장된 기록으로 같은 정책을 적용한다. 기존 수신함과 다른 receiver Agent는 별도 프로필을 사용해야 한다.

디스크·ACL·파싱·용량·잠금 실패나 대기열 초과에는 성공 ACK를 보내지 않는다. 이미 중앙에 생성된 작업을 실패 처리하거나 삭제하지 않는다. 중앙의 ACK_TIMEOUT/DELIVERY_UNKNOWN 가능성과 taskId는 그대로 두고 중앙 조회로 확인해야 한다. 현재 그 조회 provider는 후속 -94/-76 범위다.

파일/ACL 작업은 blocking worker로 분리했다. 연결별로 최대16건의 다음 batch를 대기시키고, 한 번에 최대16건을 기록한다. 기록 중 작업은 재접속 후 남은 worker까지 포함해 전역16건으로 제한한다. 대기 중에도 기존 JSON heartbeat와 제어 Ping/Pong·종료를 처리한다. 종료 시 대기 worker의 새 저장을 막고, clear는 broker/inbox 잠금으로 실행·기록 작업과 경합을 차단한다. inbox 없는 transport-only run_with_state 호출은 TASK_CREATED를 ACK하지 않는다.

저장·용량·잠금 실패, queue 초과, 유효하지 않은 TASK_CREATED는 내용 없는 고정 오류 코드/거부 횟수를 프로세스 수명 동안 유지한다. 인증된 `/health`의 선택적 `inbox: {rejectedEvents,lastError}`와 `devlink status`의 Inbox rejects로 확인한다. broker 재시작 시 이 진단 카운터는 초기화된다. foreground에는 고정 stderr 진단도 있지만 `devlink start`의 백그라운드 stderr는 숨겨지므로 상태 조회를 사용한다. Connected는 중앙 연결 상태이며 수신함 저장 성공을 보장하지 않는다. 늦게 성공한 ACK의 수용 여부는 중앙 -54 정책을 따른다.

## 보관·용량·정리

파일은 설정 프로필에 대응하는 `.devlink-runtime/inbox/tasks.json`이다. schemaVersion1 snapshot에 최대1000 event, 최대8MiB를 보관한다. snapshot은 일반 파일이어야 하며 state/inbox 경로의 symlink/junction, 비공개가 아닌 파일 권한을 거부한다. private 임시 파일은 내용을 쓰기 전에 권한을 설정하고, journal은 기존 자격 발행의 OS 권한 검증 함수를 재사용한다. 중앙 자격·requestKey·작업 결과는 이 메시지 규격에 없고 새로 저장하지 않는다.

보관 기간은 **명시적으로 정리할 때까지**다. 자동 만료/덮어쓰기/오래된 기록 축출은 하지 않는다. 용량에 도달하면 새 event의 기록·ACK를 거부해 기존 데이터와 중복 방지 이력을 지킨다. 이미 보관한 동일 event의 재확인 ACK는 가능하다. 임시 파일은 실패 시 정리되며 원본 journal을 빈 파일로 덮지 않는다.

정리하려면 broker를 멈추고 필요한 기록을 안전한 private 보관 위치에 보관한 뒤 `inbox clear`를 사용한다. clear는 **요청 데이터와 event 중복 방지 이력을 함께 삭제**해 보관 구간을 끝낸다. 강제 종료로 남은 예약 prefix `.inbox-*` 일반 임시 파일도 private 권한을 확인한 뒤 정리한다. 이후 동일 event가 새로 전달되면 새 보관 구간에서 다시 기록될 수 있다. clear는 중앙 작업/ACK/수락/업무 상태를 변경하지 않는다. 손상/과대/위험 경로 파일은 자동 삭제·복구하지 않고 오류를 반환하므로, broker를 멈추고 private 원본을 확인해 별도로 복구한다.

검증은 저장 후 새 Inbox/연결 runner의 재로드·원자 발행·저장 실패 시 ACK 금지와 실제 CLI broker 프로세스의 정상 정지·재시작을 포함한다. OS/전원 장애 복구는 실행하지 않았다.

## 사용자 확인

```powershell
# 최신 journal 순서, 기본20개, task별1개. 중앙 요청/수락 없음.
devlink --config C:\profiles\a\config.json inbox
devlink --config C:\profiles\a\config.json inbox list --limit 50

# 보관 구간 종료. 실행 중이면 거부한다.
devlink --config C:\profiles\a\config.json stop
devlink --config C:\profiles\a\config.json inbox clear
```

목록은 원래 TASK_CREATED 객체의 JSON 배열이다. C0는 JSON 직렬화로, DEL/C1 및 bidi·선택된 서식 문자는 출력 후처리로 Unicode escape한다. JSON을 다시 파싱하면 저장된 원문이 보존된다. 원격 문자열을 자동 실행하지 않는다. 중앙의 최신 상태/작업 목록이 아니라 로컬 수신 기록이다. Host는 기존 `get_tasks(direction: received)`를 사용하고, 사용자가 특정 작업을 수락한 뒤 기존 update_task_status를 호출한다. 새로운 MCP push/tool은 추가하지 않았다.

## Windows ACK 지연 실측

같은 Windows MSVC debug 환경에서 실제 파일/ACL/flush와 loopback 소켓을 사용했다. 단건5회와16건 burst의 송신→ACK 지연이며 운영 중앙/DB 측정이나 SLA가 아니다. foreground CPU/보안 소프트웨어 상태를 통제한 대규모 측정은 아니다.

| 구현 | 단건 p50/p95 (ms) | 16건 burst p50/p95 (ms) | burst 5초 초과 |
| --- | --- | --- | --- |
| 항목별 snapshot 직렬 기록 | 801.77 / 876.30 | 5987.69 / 12282.58 | 10/16 |
| 최대5ms 수집 후 원자 batch 기록 | 797.44 / 826.82 | 881.71 / 881.84 | 0/16 |

기존 기록 순서·권한 검사를 제거하지 않고 batch당 한 번 발행하는 방식으로 개선했다. 작은 표본이므로 느린 디스크·더 큰 journal·반복 재접속에도 중앙 기본5초 ACK 예산을 만족한다고 일반화하지 않는다. 대상 환경 재현:

```powershell
cd rust
cargo +stable-x86_64-pc-windows-msvc run --offline -p devlink-broker --example measure_inbox_ack
```

## 검증과 남은 조건

```powershell
cd rust
cargo +stable-x86_64-pc-windows-msvc test --workspace --all-targets -- --test-threads=1
```

단위 회귀는 실제 private 파일·권한 확인, 재시작 중복/충돌, 용량 도달 시 원본 보존, 손상/과대/비파일 snapshot, receiver/ID/wire 형식을 확인한다. 소켓 회귀는 실제 Rust 연결 루프와 loopback WebSocket fixture로 저장 선행 ACK·중복/새 runner·실패 시 ACK 없음·제어 Ping 처리·READY 이전 거부를 확인한다. CLI subprocess 회귀는 목록/limit/문자 escape·실행 중 clear 거부·정상 정리와 실제 `start → 수신/ACK → stop → start → 동일 이벤트 수신/ACK`를 검증한다. 새 broker instanceId와 원래 journal 바이트/단일 작업 기록을 대조하며 각 ACK 수신 직후 독립 파일 리더로 저장 내용을 확인한다.

fixture의 READY는 테스트 신원이며 실제 중앙 인증을 증명하지 않는다. -49/-84 production 자격, -91/-92 검증된 프로필·세션·presence와 결합한 실제 중앙 socket/DB ACK 저장 검증, 기존 -52 전체 Rust 통합 및 -66 두 사용자 왕복은 남아 있다. 코드/테스트 커밋을 Jira Done 또는 최종 MVP 통과로 판정하지 않는다.
