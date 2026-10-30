# -93 Gemini 지적 후속 수정 (2026-10-30)

사용자가 전달한 독립 검토는 `d5b6911aed0e3f3c970a781170e2c44bad0548f8` 기준 미커밋 -93 변경분에서 P2 두 건을 지적했다. 현재 작업 브랜치의 HEAD는 이후 병합된 `58da7a8`이며, 이 문서는 검토 원본과 수정본을 구분한다. 기존 소스/문서 변경을 되돌리거나 중앙 정책을 변경하지 않았다.

## P2-1: 동시 내용 충돌에서 requestKey 누락

기존 `known_key` 선조회와 `prepare` 저장 사이에는 다른 요청이 키를 게시할 수 있었다. 같은 requestId/actor의 두 요청이 선조회 때 모두 None을 본 뒤 하나가 저장되고 다른 하나가 내용 충돌하면 409 응답에 키가 빠졌다.

`prepare_sync`가 저장소 잠금 안에서 기존 actor의 키를 확인하고 `PrepareError { error, request_key }`로 오류와 함께 반환하도록 변경했다. origin/user/team/requesterAgent/requestId가 모두 같은 경우에만 키를 포함한다. `task_api`는 이 결과를 그대로 envelope에 전달하며 선조회/두 번째 spawn_blocking은 제거했다. 저장 실패에서도 이미 게시된 동일 actor의 키는 보존한다. 다른 팀/Agent의 충돌 응답에는 기존 키를 넣지 않는다. 파일 schema 1과 중앙 요청 형식은 바꾸지 않았다.

실제 Rust HTTP에서 같은 새 requestId로 서로 다른 내용을 동시에 제출하는 회귀를 추가했다. 수정 전 키가 Null인 409가 재현됐고, 수정 후 성공 응답/충돌 응답/중앙 기록이 같은 키를 사용하며 중앙 전송은 한 번임을 확인한다. 단위 회귀는 다른 팀·Agent의 키 비노출, 저장 경로 오류에서도 기존 키 보존을 검사한다.

## P2-2: 메타데이터 오류가 신원·세션 오류를 가림

`register_agent`는 먼저 toolType의 허용 문자열을 로컬 검증한다. 잘못된 형식이면 중앙 요청을 보내지 않는다. 정상 형식이면 로컬 세션 인가 → 중앙 Agent 확인 → READY 신원 및 세션 재검증 → name/role/toolType 메타데이터 대조 순서를 따른다. 다른 Agent나 해제/교체된 세션을 VALIDATION_FAILED로 가리지 않는다. Agent 생성/수정/토큰 발급이나 자동 bind는 추가하지 않았다.

미바인딩 세션에서 중앙이 다른 Agent ID와 다른 이름을 반환하는 경우를 추가했다. 중앙 응답 대기 중 DELETE 또는 fresh 교체가 발생하면서 이름도 불일치하는 경우도 검사한다. invalid toolType의 중앙 호출 수는 0이어야 한다.

## 증거와 남은 조건

수정 전 실행은 기존/신규 회귀 중 13 통과·4 실패·2 ignored였다. 네 실패는 동시 충돌 키 누락, 해제 오류가 400으로 가려짐, 잘못된 toolType에서 중앙 호출 4회, 다른 READY Agent가 400으로 가려짐이다. 별도 교체 회귀는 이후 추가했다. 로그는 ignored `rust/target/story93/gemini-followup-before.log`다.

수정 후 관련 66개와 실제 CLI 생성·재시작 회귀 1개가 통과했다. 포맷·전체 targets Clippy·diff 검사도 통과했다. 결과와 실행 범위는 [업무 중계 문서](business-relay.md)의 후속 검사 절을 따른다. 이전 전체 Rust 145개·실제 중앙/DB 기록과 이번 합성 provider/Rust HTTP 회귀를 구분한다. 이번 후속 수정에서 실제 중앙·PostgreSQL이나 Host를 다시 기동하지 않았다. -67/-71 오프라인 저장 전 거부, -68 동시 키 정책의 기존 provider blocker는 그대로다.

Gemini가 이 수정본을 다시 검토한 기록은 아직 없다. 새 검토 묶음과 이전 원본을 ignored `rust/target/story93/`에 구분해 보관하며 현재 소스의 SHA-256 manifest를 함께 제공한다. 이 문서가 Gemini의 수정본 승인이나 스토리 전체 완료를 뜻하지 않는다.
