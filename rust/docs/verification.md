# 로컬 검증 기록

## 최종 확인: 2026-10-27

검증 환경: Windows x86_64 MSVC, rustc/cargo 1.99.1, Visual Studio 2022 Build Tools 및 Windows SDK. 기능 코드 기준은 `7bfca74`(P2 디렉터리 분리)입니다. P1의 `Cargo.lock` 추적과 `protocol.rs` 포맷 수정도 이 기준에 포함되어 있습니다.

명령 실행 위치는 저장소의 `rust/`입니다. CLI와 브로커를 두 크레이트로 나눈 Cargo workspace 기준이며 빌드 산출물은 저장소의 `rust/target/`에 생성됩니다.

| 검사 | 결과 |
| --- | --- |
| `cargo build --offline --locked --workspace` | 통과 |
| `cargo test --offline --locked --workspace --all-targets` | 단위 테스트 1개, 통합 테스트 15개 통과, 실패 0 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 통과 |
| `cargo build --release --offline --locked --workspace` | 통과, `target/release/devlink.exe` 생성 |
| `target/release/devlink.exe --help` | 기본 CLI 명령 도움말 확인 |

의존성 캐시가 있는 환경에서 `--offline`을 추가해 실행했습니다. 로컬 통신이 필요한 테스트는 승인된 샌드박스 외부 실행으로 검증했으며, `TEMP`와 `TMP`는 기본 Windows 임시 경로를 사용했습니다.

통합 테스트는 실제 실행 파일의 init/config/start/status/stop, CLI 종료 후 브로커 유지, 동시·중복 실행, 로컬 API 인증·Origin 거부, 강제 종료 후 stale metadata 복구, 포트 충돌, 살아 있지만 Health 인증이 실패한 인스턴스의 오류 처리, 인증 정보 비노출을 확인합니다. Mock 중앙 서버로 인증 성공/거부/4001, 인증 대기 시간 초과·잘못된 JSON, 연결 종료·서버 재시작 후 재연결, 재시도 대기 중 종료를 확인합니다.

P2 회귀 검증은 `runtime.json`, `broker.lock`, `control.lock` 및 대소문자 변형을 설정 파일명으로 사용할 때 설정을 보존하는지 확인합니다. `init`·설정 수정, stale metadata 정리, 토큰 누락으로 실패한 `start`, 실제 브로커 시작·상태 조회·정상 종료를 검사합니다. 단위 테스트는 런타임 게시 후 `RuntimeGuard`가 내부 메타데이터만 삭제하고 설정과 잠금 파일은 유지하는지 확인합니다. 내부 파일은 설정 디렉터리 아래의 `.devlink-runtime/`에 저장됩니다.

## 샌드박스에서 발생했던 실패와 원인

샌드박스 내부에서 작업 폴더의 임시 경로를 사용한 실행은 단위 테스트 1개 통과, 통합 테스트 5개 통과·10개 실패였습니다. 직접 진단한 결과 브로커 프로세스와 런타임 파일은 정상 생성됐지만 `127.0.0.1` TCP 연결이 Windows 오류 `10013`(소켓 접근 거부)으로 차단됐습니다. 이로 인해 8개 테스트는 시작 확인에서, 강제 종료 복구 테스트는 준비 대기에서, 동시 시작 테스트는 성공 여부 검사에서 실패했습니다.

같은 코드를 샌드박스 밖에서 재실행하자 16개 테스트가 모두 통과했습니다. 작업 폴더의 임시 경로와 기본 Windows 임시 경로에서 각각 통과를 확인했습니다. 앞서 샌드박스의 기본 임시 경로에서 발생했던 `Cannot replace JSON file` 오류도 최종 샌드박스 외부 실행에서는 재현되지 않았습니다. 정확한 파일 저장 실패의 하위 OS 오류는 별도로 확정하지 않았습니다.

## 이전 기록과 미검증 범위

2026-10-26 문서에는 통합 테스트 13개와 빌드·릴리스·포맷·Clippy 통과가 기록되어 있었습니다. 위 표는 그 기록을 재사용한 것이 아니라 P1·P2 수정 이후의 최종 실행 결과입니다.

현재 저장소에는 `.github/workflows/`가 없으므로 Windows/Ubuntu CI 구성 및 실행 완료로 표기하지 않습니다. Ubuntu, 실제 중앙 서버 운영 환경, WSS 서버와의 end-to-end 연결, 실제 Agent/MCP 서버 Tool 호출은 이번 검증에 포함하지 않았습니다. MCP 담당자 합의 상태는 [MCP 인터페이스 문서](mcp-interface.md)에 별도로 표시했습니다.
