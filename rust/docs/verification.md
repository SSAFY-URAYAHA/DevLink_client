# 로컬 검증 기록

검증일: 2026-10-26 (Asia/Seoul). Windows x86_64 MSVC, rustc/cargo 1.99.1, Visual Studio 2022 Build Tools 및 Windows SDK.

명령 실행 위치는 저장소의 `rust/`입니다. CLI와 브로커를 두 크레이트로 나눈 Cargo workspace 기준이며 빌드 산출물은 저장소의 `rust/target/`에 생성됩니다.

| 검사 | 결과 |
| --- | --- |
| `cargo build --locked --workspace` | 통과 |
| `cargo test --locked --workspace --all-targets` | 통합 테스트 13개 통과, 실패 0 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 통과 |
| `cargo build --release --locked --workspace` | 통과, `target/release/devlink.exe` 생성 |
| `target/release/devlink.exe --help` | 기본 CLI 명령 도움말 확인 |

통합 테스트는 실제 실행 파일의 init/config/start/status/stop, CLI 종료 후 브로커 유지, 동시·중복 실행, 로컬 API 인증·Origin 거부, 강제 종료 후 stale metadata 복구, 포트 충돌, 살아 있지만 Health 인증이 실패한 인스턴스의 오류 처리, 인증 정보 비노출을 확인합니다. Mock 중앙 서버로 인증 성공/거부/4001, 인증 대기 시간 초과·잘못된 JSON, 연결 종료·서버 재시작 후 재연결, 재시도 대기 중 종료를 확인합니다.

Windows에서 `start`의 출력을 캡처할 때 자식 브로커가 원래 표준 출력 핸들을 상속해 호출자가 EOF를 기다리는 문제를 발견했습니다. 실행 전 기존 표준 핸들의 상속 플래그를 해제하도록 수정했고 실제 CLI 출력을 수집하는 수명 주기 테스트로 검증했습니다.

Ubuntu는 CI 매트릭스에 추가했으며 이 로컬 기록만으로 통과를 주장하지 않습니다. 실제 중앙 서버 운영 환경, WSS 서버와의 end-to-end 연결, 실제 Agent/MCP 서버 Tool 호출은 아직 검증하지 않았습니다. MCP 담당자 합의 상태는 `mcp-interface.md`에 별도로 표시했습니다.
