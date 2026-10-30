# 중앙 공식 생성 경로·REJECTED 호환 검증 (S15P31D108-93)

작성일: 2026-10-30 (Asia/Seoul). client 기준 `8199ba77d8fffde2351857608e95ba028ae48003`, 실제 중앙 기준 `a5c7987e1253a8a17912d4ea272ff105f4f4a658`. 초기 -93 구현의 `02de434` 중앙 검증과 이번 실행을 구분한다.

## 수정 범위

- 중앙 생성 URL을 `POST /api/v1/tasks/{teamId}`로 맞췄다. 기존 로컬 세션과 중앙 context를 대조한 teamId를 사용하고 요청자 신원은 Agent Bearer로 인증한다. 본문은 `requestKey/receiverAgentId/prompt`를 유지한다.
- 명시적으로 같은 요청을 재접수했을 때 반환되는 기존 작업의 `REJECTED` 상태를 보존한다. 알 수 없는 상태는 계속 프로토콜 오류로 처리한다.
- 실제 Rust HTTP fixture·CLI 재시작 회귀와 업무 중계/수신함 재현 스크립트의 생성 경로를 갱신했다. 실제 중앙에서 수신자가 거절한 뒤 같은 로컬 요청으로 기존 작업을 확인하는 검증을 추가했다.
- MCP 입력·7개 업무 도구, 로컬 `/v1/tasks/create`, requestKey 저장 정책, DB schema, WS 메시지/ACK, 자동 재시도 정책은 변경하지 않았다. 기존 -94/-96/-97 및 최종 Host 왕복은 이 수정 범위가 아니다.

## 수정 전 재현

새 회귀는 공식 경로만 제공하는 중앙 fixture를 사용한다. 이전 코드에서는 첫 생성이 로컬 502 `CENTRAL_PROTOCOL_ERROR`로 실패했다. URL만 수정한 상태에서는 첫 생성은 성공하고, 거절된 기존 작업의 재접수가 동일 오류로 실패했다. REJECTED를 허용한 뒤 두 경로를 함께 검증한다. 로그는 ignored `.planning/task-create93-red-route.log`, `task-create93-red-rejected.log`에 보관한다.

## 현재 실행 결과

- MCP: 빌드·계약 export 확인 및 기존 100건 모두 통과했다.
- Rust 전체 workspace/all-targets: 159건 통과, 실패 0, 기존 MCP opt-in 2건 ignored. 실제 CLI·로컬 HTTP·WS·수신함·재시작 회귀를 포함한다. workspace 빌드도 최종 통과했다. 전체 회귀 실행 중 별도 빌드는 실행 중인 CLI의 Windows 파일 잠금으로 한 번 실패했으며, 회귀 종료 후 순차 빌드로 정상 완료했다.
- 포맷·Clippy: 전체 targets의 `-D warnings` 검사를 통과했다. 샌드박스 파일시스템의 incremental cache hard-link 경고는 복사로 대체됐으며 lint 오류는 없었다.
- 실제 중앙: Java 21, Spring Boot, PostgreSQL 17.11 새 전용 loopback 클러스터를 사용했다. 고정 server source를 격리 빌드하고 Flyway V1~V4 및 Hibernate schema validate를 유지했다. 최초 기동은 WebSocket allowed-origins 설정 누락으로 종료돼 테스트 프로세스에 해당 설정을 명시한 뒤 정상 기동했다. 운영 소스·마이그레이션은 수정하지 않았다.
- `verify-business-relay.mjs`: 실제 사용자/팀/Agent, 기존 Agent 확인·팀 목록·정확한 prompt 저장·TASK_CREATED 영속 수신·중앙 ACK·동일 키 재접수·내용 충돌·권한 거부·실제 REJECTED 재접수를 통과했다. 같은 키 동시 생성은 이번 실행에서 201/200, DB 작업 1건이었다.
- 해당 재현의 `passed=true`는 절차 일치이며 `storyComplete=false`다. 오프라인 수신자의 작업이 저장된 뒤 DELIVERY_NOT_SENT/RECEIVER_OFFLINE으로 돌아오는 중앙 -67/-71 수용 차이는 그대로 기록했다. 이번 동시 요청 한 번의 성공으로 -68 전체 수용을 대신하지 않는다.
- `verify-task-inbox.mjs`: 최신 공식 생성 경로로 16건 ACK_RECEIVED, 재시작 후 새 작업 1건 ACK, 기록 실패의 ACK 0건 및 DELIVERY_UNKNOWN을 확인했다. 상태는 PENDING을 유지하고 자동 수락은 없었다.
- 정리: 두 재현 스크립트의 임시 프로필·자식 브로커를 정리했다. 검증 DB의 ONLINE Agent 0건과 실제 적용된 migration 4건을 확인한 뒤 이번 작업에서 띄운 중앙·PostgreSQL 프로세스만 종료했다. private DB/로그는 ignored 작업 폴더에 보존하며 사용자 자료는 삭제하지 않았다.

기존 MCP opt-in/실제 Host 시험 및 -66 최종 코드 수정 왕복을 이번 결과에 포함하지 않는다.

## Claude Code CLI 교차검증

- CLI `2.1.310`에 `--model claude-opus-5-5 --effort high`를 명시하고 Read/Grep/Glob만 허용해 변경분·최종 검증 기록·고정 중앙 코드를 검토했다. `modelUsage`와 assistant model의 실제 ID는 모두 `claude-opus-5-5`였다. effort는 CLI 인자로 기록하며 모델 자신의 추측으로 확인하지 않는다.
- 1차 완결 검토: 정확성·보안·회귀의 차단 결함 없음. 낮은 우선순위 2건(중앙 context의 팀 불일치 시 생성 차단 회귀 누락, teamId가 경로에도 전송되지 않는 것처럼 읽히는 문구)을 지적했다.
- 반영: 중앙이 다른 teamId를 반환하면 403/FORBIDDEN이고 실제 생성 POST·저장 0건임을 검증하는 회귀를 추가했다. 문서는 actor ID를 본문에 넣지 않되 검증된 teamId를 경로에 사용한다고 명확히 했다. 반영 후 task_creation HTTP 회귀 20건 통과·실패 0·기존 opt-in 2 ignored다. 이 결과를 앞선 전체 159건과 합산하지 않는다.
- 검토 원문·실제 모델 metadata·prompt는 private ignored `.planning/task-create93`에 보관한다. 1차 prompt SHA-256은 `ACB4416532470D527899D1D567CED8FED32C6D02E8DE151AC25225235306B436`다. 초기 파이프 호출은 응답 지연으로 중단했고, 도구 없는 호출은 추가 읽기 요청만 남겨 완료 판정에서 제외했다. 같은 지정 모델에 입력 EOF와 읽기 전용 도구를 명시한 완결 검토만 결과로 인정한다. 다른 모델로 대체하지 않았다.
- 최종 재검토: 2026-10-30 13:53 KST, 실제 모델 `claude-opus-5-5`, CLI effort `high`, exit 0. 두 지적의 반영을 확인했고 최종 판정은 **no actionable findings**다. 원문은 `opus-review-complete-result.json`/`opus-review-complete.jsonl`, 최종 prompt SHA-256은 `906FD5B7205209C9FA6696FCE0E63A66798AED8662D40A58BC9859E454CF5EEA`다. 새 회귀를 추가한 뒤 task_creation 대상 Clippy와 전체 포맷 검사도 재통과했다. 이 판정과 실행 기록을 반영한 상태에서 -93 후속 수정만 하나의 커밋으로 정리한다.

## 재현 명령

~~~powershell
# rust 디렉터리
cargo +stable-x86_64-pc-windows-msvc test --locked --offline --workspace --all-targets -- --test-threads=1
cargo +stable-x86_64-pc-windows-msvc clippy --locked --offline --workspace --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-msvc fmt --all -- --check

# 저장소 루트: 전용 중앙/DB 및 현재 CLI를 환경변수로 지정
node --check rust/scripts/verify-business-relay.mjs
node --check rust/scripts/verify-task-inbox.mjs
node rust/scripts/verify-business-relay.mjs
node rust/scripts/verify-task-inbox.mjs

# packages/mcp-server 디렉터리
npm.cmd test
~~~

업무 중계 스크립트는 `DEVLINK_TEST_CENTRAL_URL`, 절대 `DEVLINK_TEST_CLI`/`DEVLINK_TEST_PSQL`, 전용 DB port인 `DEVLINK_TEST_DB_PORT`를 사용한다. DB/role은 `devlink_relay_test`이며 비밀번호는 검증 프로세스 환경으로만 전달한다. 합성 자료와 private 임시 프로필만 사용하고 출력의 비밀 포함 여부를 검사한다. 원격·운영 DB를 대상으로 실행하지 않는다.
