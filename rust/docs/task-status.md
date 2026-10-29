# Epic 작업별 현황

2026-10-28 로컬 세션 구현: 세션 API 4개·fresh/recover·90초 lease·presence 연결을 구현했습니다. 중앙 신원 공급부는 인터페이스로 분리했으며 기본 CLI의 생성 요청은 503 CENTRAL_UNAUTHENTICATED입니다. [로컬 세션 API](local-sessions.md)와 [중앙 WebSocket 규칙](central-websocket-contract.md)에 로컬 검증과 실제 중앙 신원·업무 연동의 남은 범위를 구분합니다.

| Task | 이번 변경 | 남은 완료 조건 |
| --- | --- | --- |
| 1 | Rust/Cargo 설치, Windows MSVC 빌드, CLI/브로커 모듈 분리, 기존 TS 소스 분석 | 없음 (검증 결과는 PR 참고) |
| 2 | clap init/start/stop/status/config/mcp path/mcp rotate, 설정 검증·인증정보 비노출. `.devlink-runtime/broker.json` 원자 발행, OS 권한 검사, 정상·강제 종료 후 MCP 토큰 유지, 중지 중 회전, CLI/MCP 토큰·권한 분리 | 자격 파일·제어 권한 구현은 완료. Unix 실행 검증과 Host별 프로필·설치 정책은 후속 항목 |
| 3 | 로컬 생성·bind·heartbeat·해제 API, requestId envelope·입력/인증 검증, instanceId 멱등성·fresh/recover 소유권·90초 lease·백그라운드 만료·presence 연결 구현. IdentityProvider 분리와 미구성 거부 | **실제 중앙 신원 전달·Agent 소유 검증·등록/토큰 발급 연결과 실제 MCP Host 상호 검증 필요. Agent ID 영속 재사용·등록 멱등성·업무 매핑·프로필·설치는 후속 항목** |
| 4 | 독립 프로세스·OS 잠금·Health·정상 종료·강제 종료 후 복구 | 없음 (검증 결과는 PR 참고) |
| 5 | `/ws/broker` Bearer 인증·READY·식별자·JSON Heartbeat·만료 재접속. 세션 관리자의 생성·bind·해제·만료와 set_presence 연결 및 실제 로컬 WebSocket 보고 검증 | 실제 중앙 신원·토큰·DB 상태 갱신·운영 하트비트 유지·만료·교체 확인 |
| 6 | 2026-10-28 Windows MSVC 단위 27개·세션 HTTP/WebSocket 6개·CLI 통합 30개와 컴파일·포맷·Clippy 통과. 기존 45초 Heartbeat 회귀 포함 | CI·Ubuntu·원격·실제 Host 검증 필요. 이번 작업은 rust/ 내부만 수정하고 커밋·Push하지 않음 |

Rust 로컬 세션 API는 구현됐고 중앙 신원 공급부는 보류했습니다. Agent 등록·실행, 업무 API·작업 라우팅은 미구현입니다. packages/mcp-server의 실제 Host·중앙 왕복은 미검증이며 이 작업에서 TypeScript·중앙 서버 파일은 수정하지 않았습니다.

결정 상태 갱신일: 2026-10-27 (Asia/Seoul). 사용자의 “MCP 규칙을 따라가자” 결정으로 기존 MCP 규칙을 구현 기준으로 채택했습니다. 브로커 재시작 후에는 먼저 유효하게 복구한 인스턴스가 해당 Agent의 빈 슬롯을 확보하며 이전 소유자는 보존하지 않습니다. 이는 상대 담당자의 추가 승인·Rust 구현·수용 검증 완료를 의미하지 않습니다. MCP 규칙 밖의 잔여 결정과 실제 구현의 차이는 [MCP 인터페이스 문서](mcp-interface.md)를 참고하세요.

[로컬 검증 기록](verification.md): Windows MSVC에서 단위 27개·통합 36개, 컴파일·포맷·Clippy가 통과했습니다. ACL·HTTP/WebSocket은 샌드박스 외부의 임시 설정·합성 신원으로 검증했습니다. MCP 100개·일반/릴리스 빌드는 이전 기록이며 Ubuntu·원격 CI·실제 중앙 인증과 DB 연동은 미검증입니다.

다음은 중앙 신원 조회·Agent 소유권·등록/토큰 발급 계약 → 실제 IdentityProvider·MCP Host 세션 검증 → 업무 API·라우팅·수신함·ACK → 두 사용자 왕복입니다. Windows/Ubuntu CI도 남아 있습니다. [로컬 세션 API](local-sessions.md)에 구현된 범위와 남은 연결을 정리했습니다.

프로젝트 구조는 `rust/` Cargo workspace로 변경했습니다. CLI와 통합 테스트는 `crates/devlink-cli`, 브로커 라이브러리와 Mock 예제는 `crates/devlink-broker`, Rust 문서는 `rust/docs`에서 관리합니다.
