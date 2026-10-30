# 작업 수신함·ACK — S15P31D108-95 통합

2026-10-30. 최신 develop의 TASK_CREATED/ACK 타입, Inbox::open/record/list_for, 중앙·세션 코드를 유지하고 닫힌 PR #16의 필요한 CLI·영속성 보완을 통합했다. 이전 PR 전체를 덮어쓰거나 별도 수신함을 병행하지 않는다. ACK는 저장 확인이며 열람·사용자 수락·편집·IN_PROGRESS 전이가 아니다.

## 수신과 기록

1. 유효한 BROKER_READY 이후 양의 signed64-bit ID, event_id/prompt/created_at, 인증된 receiver_agent_id를 확인한다. READY team_id가 있으면 팀도 대조한다. 수신 payload를 사용자 신원으로 승격하지 않는다.
2. 최대5ms/16건을 묶어 하나의 snapshot으로 저장한다. blocking writer는 Inbox당1개이고 현재 연결은 기록 중 batch1개 + 대기16건까지만 보관한다. heartbeat/control Ping/close 처리는 계속한다. 초과하면 ACK 없이 연결을 닫는 기존 재연결 정책을 따른다.
3. 안전한 경로·기존 private 파일을 확인하고 inbox.lock을 잡는다. 임시 파일의 사용자 전용 권한 설정·검사 → 쓰기·sync_all → 원자 교체 → 최종 ACL 검사·열려 있는 write 핸들 sync_all, Unix 부모 디렉터리 sync를 거쳐야 ACK를 반환한다.
4. ACK 직전 현재 연결 generation과 Agent를 다시 확인하고 같은 소켓으로 해당 event_id/task_id를 보낸다. 이전 generation의 완료는 새 신원/연결을 덮어쓰거나 ACK하지 않는다. 전송에 실패하면 기록을 되돌리지 않는다.
5. batch 안에 충돌·용량·저장 실패가 있으면 batch 전체에 성공 ACK를 보내지 않고 InboxFailed로 닫는다. 원자 교체 전 검증 실패에는 부분 기록도 없다. 교체 후 최종 flush/권한 실패는 결과가 불확실하므로 성공으로 간주하지 않는다.

Windows 파일 점유로 교체가 거부되면 private 임시 파일을 유지해 최대10회(재시도 간50ms, 최대450ms)만 다시 시도한다. 계속 점유되면 ACK 없이 실패하며 원본은 유지한다. 교체 전 실패로 인메모리 store를 영구 오염시키지 않고, 교체 후 검증/flush 실패만 불확실한 게시 상태로 잠근다. 중앙 연결의 실패 후 재시작 정책은 동일하다.

정규화된 중앙 URL + receiver_agent_id + task_id로 사용자 업무 중복을 방지한다. origin + receiver + event_id는 연결 재전달 이력이며 같은 event가 다른 task로 재사용되면 거부한다. 같은 task·payload에 새 event_id가 도착하면 업무는1건을 유지하고 새 영수증만 저장·ACK한다. **따라서 다른 event_id의 재전달은 journal 바이트를 바꿀 수 있다.** 같은 event/task의 완전 중복은 재기록 없이 확인 ACK한다. 실행의 exactly-once를 보장하는 계약이 아니다.

## 파일과 호환

현재 경로는 프로필별 .devlink-runtime/inbox.json이다. origin·Agent가 다른 기록을 함께 보관하되 조회에서 분리한다. 중앙 host alias는 자동 통합하지 않는다.

- 기존 upstream schema_version=1/tasks 파일을 원본 payload·origin·Agent 그대로 읽는다. 기존 첫 event는 해당 task의 dedup 근거로 사용한다.
- tests/fixtures/inbox-v1-d49cdba.json은 d49cdba 원본 Inbox를 별도 빌드·실행해 생성한 파일이다. 기존 writer도 같은 endpoint_url 정규화와 StoredTask 필드를 사용함을 대조했고, 이 원본의 읽기·완전 중복 무변경·다음 기록 업그레이드를 회귀로 확인한다.
- 새 기록/새 영수증을 저장할 때 schema_version=2로 전환하며 tasks와 receipts를 함께 원자 저장한다. 기존 파일의 완전 중복 조회만으로는 업그레이드하지 않는다.
- 구버전 바이너리는 schema2를 읽지 못한다. 다운그레이드 전 private 원본을 별도로 보관·내보내야 하며 새 파일을 구버전 형식으로 임의 변경하지 않는다.
- 최대64MiB·task1024건·추가 receipt16384건을 명시적 clear까지 보존한다. 자동 축출·기간 삭제는 없다. 한도 초과는 ACK 없이 거부하며 이미 아는 event의 확인은 가능하다.
- 닫힌 PR16의 별도 inbox/tasks.json 형식은 최신 develop의 저장 계약이 아니다. 출처/Agent를 추측해 자동 병합·이동하거나 삭제하지 않는다. 필요하면 해당 브랜치와 원본을 보관한 뒤 별도 마이그레이션으로 다룬다.
- 손상/schema/충돌/안전하지 않은 경로나 권한은 자동 초기화하지 않는다. 원본을 private 보관하고 원인을 수정한 후 재시작한다.

stop/start·재연결·토큰 회전으로 수신함을 삭제하지 않는다. 종료 시 새 쓰기를 금지하고, 늦은 blocking writer도 파일 lock 획득 후 stop 상태를 확인한다. 실제 기록 중에는 inbox.lock이 clear와 겹치지 않게 하므로 정지 후 정리된 파일을 이전 worker가 되살리지 않는다. 단절 전에 시작된 저장은 완료될 수 있지만 ACK future는 그 연결에만 속한다.

## CLI 조회·정리

다음 명령은 중앙 업무 상태를 변경하지 않는다. Host는 기존 get_tasks(received)를 사용하며 새 MCP push/tool을 추가하지 않는다.

~~~powershell
devlink --config C:\work\profile\config.json inbox list --limit 20
devlink --config C:\work\profile\config.json inbox list --agent-id 34 --limit 100
devlink --config C:\work\profile\config.json status
devlink --config C:\work\profile\config.json stop
devlink --config C:\work\profile\config.json inbox clear
~~~

list는 디스크의 현재 원자 snapshot을 읽고 중앙 호출·디렉터리 생성·ACK·수락을 수행하지 않는다. 기본은 현재 serverUrl과 로컬 바인딩된 Agent다. 바인딩이 없는 legacy 프로필은 같은 서버의 기록을 표시한다. --agent-id는 로컬 표시 필터이며 인증 주체를 만들지 않는다. 최신 저장 순서부터 limit1~1024건의 server_url/message JSON을 표시해 requester_agent_id/task_id/prompt를 확인할 수 있다. C0는 JSON escape, DEL/C1/bidi/서식 문자는 표시 escape를 적용해 재파싱한 원문은 보존한다. 로컬 보관 데이터는 서버 최신 업무 상태가 아니다.

clear는 control.lock/broker.lock/inbox.lock 하에서 **해당 프로필의 모든 origin·Agent 기록과 dedup 이력**, private .inbox- 예약 임시 파일을 삭제한다. 실행 중인 브로커에서는 거부한다. 필요한 자료를 먼저 별도 private 보관해야 한다. 사용자/Agent/MCP 자격 파일과 중앙 업무·ACK·수락·완료는 변경하지 않는다. 손상된 원본이나 위험 경로를 자동 삭제하는 복구 명령으로 사용하지 않는다.

## 실패 진단과 검증

인증된 health의 선택적 inbox.rejectedEvents/lastError 및 CLI status에 내용 없는 STORE_FAILED/QUEUE_FULL 진단을 제공한다. 진단은 프로세스 수명 동안 유지되며 재시작 시 초기화된다. Connected는 디스크 수신 성공을 뜻하지 않는다. 저장 실패는 InboxFailed에서 자동 재접속하지 않는 upstream 정책을 유지한다.

- 단위/실소켓: 기존 schema1 보존·schema2 이력·event/task 충돌·원자 batch·정리/기록 상호 배제·손상 거부, 느린 기록 중 heartbeat와 무선행 ACK, 오래된 generation 무ACK, 16건 burst의 기본5초 예산을 회귀로 검증한다.
- 실제 CLI: start→수신→ACK→stop→새 instanceId start→새 event로 같은 task 재수신·ACK, 업무1건 유지와 event 이력 추가, 실행 중 clear 거부와 정지 후 정리, 제어 문자·로컬 Agent 필터를 확인한다.
- 실제 중앙 재현: loopback Spring/PostgreSQL을 준비하고 DEVLINK_TEST_CENTRAL_URL과 절대 DEVLINK_TEST_CLI를 지정해 저장소 루트에서 node rust/scripts/verify-task-inbox.mjs를 실행한다. 합성 사용자2명·실제 Rust 수신자·확인된 세션 presence로16건 ACK_RECEIVED, 재시작 후 새 업무 ACK, 실제 로컬 기록 실패의 무ACK/DELIVERY_UNKNOWN을 점검한다.
- 실제 중앙의 requestKey 재조회는 기존 업무를 재전송하지 않는다. 이를 wire 재전달 검증으로 세지 않으며, 같은/새 event 재전달과 재시작 dedup은 실소켓 fixture에서 별도로 검증한다.

~~~powershell
# rust 디렉터리
cargo +stable-x86_64-pc-windows-msvc test --locked --offline --workspace --all-targets -- --test-threads=1
cargo +stable-x86_64-pc-windows-msvc clippy --locked --offline --workspace --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-msvc fmt --all -- --check
~~~

실제 중앙 ACK 확인과 최종 TaskEvent 연결(-71), 전체 Rust 통합(-52), 사용자 수락 후 코드 수정·결과 회신(-66), 업무 중계/조회(-93/-94/-97)는 각 범위로 구분한다. 최신 중앙이 READY 사용자·팀을 공급하더라도 -92의 모든 인증 무효화·소유권·lease 수용을 이 스토리 테스트만으로 완료 처리하지 않는다.
