# Epic 작업별 현황

2026-10-28 중앙 규칙 확인: [중앙 WebSocket 연동 규칙](central-websocket-contract.md)을 Rust 구현 기준으로 기록했습니다. Agent 토큰 인증·READY·PRESENCE·연결 교체 규칙과 현재 로컬 서버의 JSON Heartbeat·4001 만료 처리를 반영했습니다. 실제 인증 모듈 연동, 로컬 세션 상태 보고, MCP 신원 전달·업무 라우팅과 배포 환경에서의 Heartbeat 검증은 후속 항목입니다.

| Task | 이번 변경 | 남은 완료 조건 |
| --- | --- | --- |
| 1 | Rust/Cargo 설치, Windows MSVC 빌드, CLI/브로커 모듈 분리, 기존 TS 소스 분석 | 없음 (검증 결과는 PR 참고) |
| 2 | clap init/start/stop/status/config/mcp path/mcp rotate, 설정 검증·인증정보 비노출. `.devlink-runtime/broker.json` 원자 발행, OS 권한 검사, 정상·강제 종료 후 MCP 토큰 유지, 중지 중 회전, CLI/MCP 토큰·권한 분리 | 자격 파일·제어 권한 구현은 완료. Unix 실행 검증과 Host별 프로필·설치 정책은 후속 항목 |
| 3 | 사용자 결정으로 기존 MCP v0.1 규칙을 Rust 구현 기준으로 채택. 지연 활성화·fresh/recover·선복구 소유권·갱신/만료·API·timeout 기준 문서화. 자격 파일 발행·권한 분리는 Task 2에 반영 | **Rust 세션 API 4개·소유권·만료·presence 연결 구현과 실제 MCP 상호 검증 필요. 검증된 userId/teamId 전달·Agent ID 영속 재사용·중앙 등록 멱등성·업무 매핑·Host별 프로필·설치 방식은 후속 항목** |
| 4 | 독립 프로세스·OS 잠금·Health·정상 종료·강제 종료 후 복구 | 없음 (검증 결과는 PR 참고) |
| 5 | `/ws/broker` Upgrade Bearer 인증, READY·식별자 관리, presence의 READY·변경·재접속 보고와 중복 생략·최신 상태 보존. JSON HEARTBEAT PING 20초·동일 ping_id PONG 최대 20초 대기. HTTP 401/403 인증 실패와 Close 4001 만료를 분리하고 4001·PONG 만료는 백오프 재접속. Mock도 JSON PING/PONG·40초 만료로 갱신 | 실제 중앙 토큰 검증·배포 환경 연동, 세션 초기 상태·bind·해제·만료의 set_presence 호출 연결, 실제 DB 상태·하트비트 유지·만료·교체 확인 |
| 6 | 2026-10-28 실시간 45초 Heartbeat 자동 회귀 추가 후 Windows MSVC 컴파일·포맷·Clippy 검사, Rust 단위 17개·통합 29개 통과. 기본 테스트에서 20초 PING·40초 만료와 연결 1회 유지 확인. MCP 100개·일반/릴리스 빌드는 이전 기록 | CI 워크플로 추가 및 Ubuntu·원격 검증 필요. 현재 변경 검토·스테이징·커밋·Push는 별도 진행 |

현재 Rust에는 MCP용 세션·업무 API, Agent 등록·실행, 작업 라우팅이 포함되지 않습니다. 저장소의 `packages/mcp-server`에는 MCP 서버·Tool·브로커 클라이언트와 가짜 브로커 테스트가 있으나 실제 Rust 연동은 미검증입니다. 별도 TypeScript 참고 소스의 원본은 수정하지 않았습니다.

결정 상태 갱신일: 2026-10-27 (Asia/Seoul). 사용자의 “MCP 규칙을 따라가자” 결정으로 기존 MCP 규칙을 구현 기준으로 채택했습니다. 브로커 재시작 후에는 먼저 유효하게 복구한 인스턴스가 해당 Agent의 빈 슬롯을 확보하며 이전 소유자는 보존하지 않습니다. 이는 상대 담당자의 추가 승인·Rust 구현·수용 검증 완료를 의미하지 않습니다. MCP 규칙 밖의 잔여 결정과 실제 구현의 차이는 [MCP 인터페이스 문서](mcp-interface.md)를 참고하세요.

[로컬 검증 기록](verification.md): 2026-10-28 실시간 45초 Heartbeat 자동 회귀 추가 후 Windows MSVC에서 `cargo check`, `cargo fmt`, `cargo clippy -D warnings`, Rust 단위 17개·통합 29개가 통과했습니다. ACL·로컬 HTTP/WebSocket 테스트는 샌드박스 밖의 기본 Windows 임시 경로에서 실행했습니다. MCP 100개 테스트와 일반·릴리스 빌드는 이번 추가에서 재실행하지 않은 이전 기록입니다. 현재 저장소에는 `.github/workflows/`가 없으며 Ubuntu·원격 CI 실행은 검증하지 않았습니다.

다음 순서는 문서와 현재 변경 검토·커밋·Push → Rust 세션 API 4개·소유권·만료·presence 연결 → 중앙의 검증된 신원·실제 인증 연동 확인 → 업무 API 7개·라우팅·수신함·ACK → 두 사용자 왕복 검증입니다. 중앙 신원 전달 경로는 세션 구현과 함께 확인해야 하며, Windows/Ubuntu CI 구성도 남아 있습니다. 세부 구현 순서는 [MCP 인터페이스](mcp-interface.md#다음-구현-rust-세션-api)를 참고하세요.

프로젝트 구조는 `rust/` Cargo workspace로 변경했습니다. CLI와 통합 테스트는 `crates/devlink-cli`, 브로커 라이브러리와 Mock 예제는 `crates/devlink-broker`, Rust 문서는 `rust/docs`에서 관리합니다.
