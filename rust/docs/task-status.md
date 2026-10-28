# Epic 작업별 현황

2026-10-28 중앙 규칙 확인: [중앙 WebSocket 연동 규칙](central-websocket-contract.md)을 Rust 구현 기준으로 기록했습니다. Agent 토큰 인증·READY·PRESENCE·연결 교체 규칙은 중앙 문서에 정의돼 있습니다. 실제 인증 모듈 연동, 로컬 세션 상태 보고, Heartbeat 초기안의 최종값, MCP 신원 전달과 업무 라우팅은 각각 후속 항목으로 구분합니다.

| Task | 이번 변경 | 남은 완료 조건 |
| --- | --- | --- |
| 1 | Rust/Cargo 설치, Windows MSVC 빌드, CLI/브로커 모듈 분리, 기존 TS 소스 분석 | 없음 (검증 결과는 PR 참고) |
| 2 | clap init/start/stop/status/config, serde JSON, 생성·조회·수정, 검증과 인증정보 비노출 | 없음 (검증 결과는 PR 참고) |
| 3 | 사용자 결정으로 기존 MCP v0.1 규칙을 Rust 구현 기준으로 채택. 지연 활성화·fresh/recover·선복구 소유권·갱신/만료·자격 스키마·API·timeout 기준 문서화 | **Rust 대응 구현·실제 MCP 상호 검증 필요. 파일 배치·프로필·Agent ID 영속 재사용·제어 권한·중앙 매핑·설치 방식은 추가 결정 필요** |
| 4 | 독립 프로세스·OS 잠금·Health·정상 종료·강제 종료 후 복구 | 없음 (검증 결과는 PR 참고) |
| 5 | `/ws/broker` 전용 Upgrade Bearer 인증, `BROKER_READY` 검증·식별자 관리, 확인된 로컬 상태(`ONLINE`/`OFFLINE`)와 미확인 상태 구분 `AGENT_PRESENCE` 보고, `401`/`403`/`4001`/`4002`/`1008` 종료 분류, 백오프 재연결·Heartbeat·Mock. 중앙 규칙은 [적용 기준](central-websocket-contract.md)에 기록 | 서버 `AgentTokenVerifier` 실제 구현 Bean·실제 Rust 연동 검증, 3단계 세션의 초기 상태·변경·해제·만료와 presence 연결, Heartbeat 초기안(20초·20초·40초)과 현재 Rust(5초·15초 초과)의 최종 정합성 확인 |
| 6 | 단위 테스트 1개·통합 테스트 15개, README, 검증 문서, 일반·릴리스 빌드·포맷·Clippy 검사 통과 | CI 워크플로 추가 및 원격 검증 필요. 최종 문서 변경의 스테이징·커밋·Push는 사용자가 진행 |

현재 Rust에는 MCP용 세션·업무 API, Agent 등록·실행, 작업 라우팅이 포함되지 않습니다. 저장소의 `packages/mcp-server`에는 MCP 서버·Tool·브로커 클라이언트와 가짜 브로커 테스트가 있으나 실제 Rust 연동은 미검증입니다. 별도 TypeScript 참고 소스의 원본은 수정하지 않았습니다.

결정 상태 갱신일: 2026-10-27 (Asia/Seoul). 사용자의 “MCP 규칙을 따라가자” 결정으로 기존 MCP 규칙을 구현 기준으로 채택했습니다. 브로커 재시작 후에는 먼저 유효하게 복구한 인스턴스가 해당 Agent의 빈 슬롯을 확보하며 이전 소유자는 보존하지 않습니다. 이는 상대 담당자의 추가 승인·Rust 구현·수용 검증 완료를 의미하지 않습니다. MCP 규칙 밖의 잔여 결정과 실제 구현의 차이는 [MCP 인터페이스 문서](mcp-interface.md)를 참고하세요.

[로컬 검증 기록](verification.md): 2026-10-27 Windows MSVC에서 일반·릴리스 빌드·Clippy·포맷 검사, 단위 테스트 1개와 통합 테스트 15개 통과. 테스트는 샌드박스 밖의 기본 Windows 임시 경로에서 실행했습니다. 현재 저장소에는 `.github/workflows/`가 없으며 Ubuntu·원격 CI 실행은 검증하지 않았습니다.

프로젝트 구조는 `rust/` Cargo workspace로 변경했습니다. CLI와 통합 테스트는 `crates/devlink-cli`, 브로커 라이브러리와 Mock 예제는 `crates/devlink-broker`, Rust 문서는 `rust/docs`에서 관리합니다.
