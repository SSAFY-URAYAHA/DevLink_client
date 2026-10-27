# Epic 작업별 현황

| Task | 이번 변경 | 남은 완료 조건 |
| --- | --- | --- |
| 1 | Rust/Cargo 설치, Windows MSVC 빌드, CLI/브로커 모듈 분리, 기존 TS 소스 분석 | 없음 (검증 결과는 PR 참고) |
| 2 | clap init/start/stop/status/config, serde JSON, 생성·조회·수정, 검증과 인증정보 비노출 | 없음 (검증 결과는 PR 참고) |
| 3 | 실행 책임·HTTP·Tool/API 초안 작성, A1~A10 가확정안 검토 및 협의 범위를 핵심 3개 항목으로 정리 | **세션 활성화와 재시작 후 소유권, CLI 설정 파일과 자격 수명주기, HTTP 계약과 중앙 위임 경계의 최종 상호 합의 필요** |
| 4 | 독립 프로세스·OS 잠금·Health·정상 종료·강제 종료 후 복구 | 없음 (검증 결과는 PR 참고) |
| 5 | TS 호환 인증 프로토콜·Mock·상태·백오프 재연결·Heartbeat, 프로토콜 모듈 분리 | 운영 중앙 서버 계약은 별도 협의 |
| 6 | 통합 테스트, README, Windows/Ubuntu CI, 로컬 작업 브랜치 및 스테이징 | 커밋 권한 요청 거절로 **커밋·Push·PR 생성 미완료**, 이후 원격 CI 및 리뷰 확인 필요 |

현재 실제 MCP 서버와 Tool 실행, Agent 등록·실행, 작업 라우팅은 포함하지 않습니다. TypeScript 참고 소스는 사용자 로컬 폴더에 있으며 원본을 수정하지 않았습니다.

협의 상태 갱신일: 2026-10-27 (Asia/Seoul). A1~A10 가확정안의 큰 방향을 유지하며 위 세 항목에 협의를 집중합니다. 사용자 검토에 따른 범위 정리이며 상대 담당자와의 최종 합의·구현·수용 시나리오 검증 완료를 의미하지 않습니다. 세부 결정 사항과 현재 구현의 차이는 [MCP 인터페이스 문서](mcp-interface.md)를 참고하세요.

[로컬 검증 기록](verification.md): Windows 빌드·릴리스·Clippy·포맷 검사 통과, 통합 테스트 13개 통과.

프로젝트 구조는 `rust/` Cargo workspace로 변경했습니다. CLI와 통합 테스트는 `crates/devlink-cli`, 브로커 라이브러리와 Mock 예제는 `crates/devlink-broker`, Rust 문서는 `rust/docs`에서 관리합니다.
