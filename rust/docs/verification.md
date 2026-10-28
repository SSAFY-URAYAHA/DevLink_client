# 로컬 검증 기록

## 2026-10-28: 실시간 45초 연결 유지 자동 회귀 추가

`feature/rust/broekr_sessionapi`의 `6e6fd78` 이후 작업 트리에 `json_heartbeat_keeps_cli_connected_for_45_seconds_without_reconnect`를 추가했습니다. 앞서 별도 실행으로 확인한 45초 유지 검증을 Rust CLI 통합 테스트로 저장했으며 `#[ignore]` 없이 기본 전체 테스트에 포함합니다.

실제 CLI 실행 파일과 테스트 전용 설정·토큰·동적 포트를 사용합니다. 운영의 JSON PING 20초·PONG 대기 20초와 테스트 Mock의 JSON PING 무수신 만료 40초를 단축하지 않습니다. Mock은 유효한 JSON PING만 만료 시각을 갱신하고 같은 ID의 PONG을 응답합니다. presence·WebSocket 제어 Ping/Pong은 만료 시각을 갱신하지 않으며 무수신 시 Close 4001로 종료합니다.

45초 동안 250ms 간격으로 health의 Connected·같은 PID/instanceId를 확인하고 WebSocket 연결 횟수 1회·만료 0회를 검사합니다. 약 20초·40초에 같은 연결에서 서로 다른 ping_id를 받았는지와 정상 stop 후 잠금 해제도 확인합니다. 중간에 끊겼다가 재접속해 Connected로 복구된 경우도 연결 횟수 검사로 실패합니다. 테스트만 다시 실행하려면 `rust/`에서 다음 명령을 사용합니다.

```powershell
cargo test --offline --locked -p devlink-cli --test cli_broker json_heartbeat_keeps_cli_connected_for_45_seconds_without_reconnect -- --exact --test-threads=1
```

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 17개·통합 29개 통과, 실패·무시 0. 새 45초 테스트 포함 |

Windows MSVC에서 전체 테스트를 승인된 샌드박스 외부에서 순차 실행했습니다. 통합 테스트 묶음은 142.88초가 걸렸습니다. 컴파일·Clippy에는 증분 캐시 hard link 대신 파일 복사를 사용하는 환경 경고가 있었고 종료 코드는 0입니다. 실제 중앙 배포 서버·운영 토큰·DB를 사용하는 검증은 아닙니다. 기존 스테이징은 유지했으며 이번 추가분은 별도 커밋·Push하지 않았습니다.

## 2026-10-28: JSON Heartbeat·4001 만료 후 재접속 수정

기준은 `feature/rust/session-api`의 `6e6fd78` 이후 작업 트리입니다. 현재 로컬 중앙 서버의 `DevLinkWebSocketHandler`·`ConnectionRegistry`·`BrokerHeartbeatService`·application.yml을 읽어 JSON PING/PONG, 동일 ping_id 응답, PING 무수신 기본 40초 만료와 Close 4001을 확인했습니다. 서버 파일은 수정하지 않았으며 이번 변경은 모두 `rust/`에 있습니다.

Rust는 READY 후 JSON HEARTBEAT PING을 20초 간격으로 보내고 각 ping_id와 일치하는 PONG을 최대 20초 대기합니다. 응답 대기 중에는 새 요청으로 기존 대기를 덮어쓰지 않으며 다른 메시지·제어 프레임·잘못된/이전 ID의 PONG으로 제한 시간을 연장하지 않습니다. 4001과 PONG 만료는 재접속 설정을 따르고 HTTP 401/403, Close 4002·1008은 자동 재접속을 중단합니다. 개발용 Mock도 JSON PING/PONG·40초 만료·4001로 맞췄습니다.

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 17개·통합 28개 통과, 실패 0 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 통과 |

Windows MSVC에서 실행했으며 ACL·HTTP·WebSocket을 사용하는 전체 테스트는 승인된 샌드박스 외부에서 실행했습니다. 컴파일·Clippy 시 증분 캐시 hard link 실패를 파일 복사로 대체하는 환경 경고가 있었으며 검사 종료 코드는 0입니다.

새 단위 테스트는 실제 로컬 WebSocket 서버와 운영과 같은 연결 루프를 사용하고, private HeartbeatTiming만 PING 40ms·PONG 대기 120ms(서버 만료를 먼저 유도하는 경우 500ms)로 단축합니다. 공개 CLI·환경변수에 테스트 시간 옵션을 추가하지 않았습니다. 다음을 확인했습니다.

- 유효한 JSON PING/PONG을 반복하면 서버의 최초 만료 시간 250ms를 넘겨 같은 연결이 유지되며 ping_id는 매번 새 UUID입니다. 하트비트만으로 미확인 presence를 ONLINE/OFFLINE으로 바꾸지 않습니다.
- 잘못된 ID의 PONG 또는 이전 PONG 재사용, 잘못된 형식, 일반 JSON, 제어 Ping/Pong을 계속 받아도 미응답 PONG 대기가 만료됩니다. 대기 중 새 PING을 보내 이전 요청을 덮어쓰지 않습니다.
- 서버의 만료 감시가 첫 연결을 4001로 종료하면 식별자를 정리하고 백오프 후 새 READY·새 ping_id로 복구하며 확인된 presence를 다시 보고합니다.
- 실제 CLI 통합 테스트는 Close 4001에서 autoReconnect=true이면 재접속·Connected, false이면 Disconnected·추가 연결 없음, HTTP 401/403은 인증 실패·재시도 중단을 각각 확인합니다. 기존 4002·1008 중단 테스트도 통과했습니다.

이 검증은 실제 로컬 소켓과 서버 계약을 재현한 Mock을 사용한 회귀 검증입니다. 실제 중앙 배포·운영 토큰·DB 갱신, 20초/40초 운영 주기의 실환경 장시간 유지, Rust 세션·업무 API 및 두 사용자 업무 왕복은 검증하지 않았습니다. MCP 전체 테스트·일반/릴리스 빌드·준비 상태 재현 도구는 이번 수정에서 재실행하지 않았습니다.

## 2026-10-28: 작업 현황 점검에서 재확인

`feature/rust/broekr_sessionapi`의 HEAD `037810b`와 미커밋 presence·통합 검사 보완을 포함한 로컬 작업 트리에서 다음 결과를 재확인했습니다. 아래 검사는 문서 갱신 직전의 작업 현황 점검에서 실행했으며 문서 수정 후 기능 검사를 새로 실행했다는 의미는 아닙니다.

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과. 증분 캐시 hard link 실패 시 파일 복사로 대체하는 경고 발생 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 13개·통합 27개 통과, 실패 0 |
| `npm.cmd test` (`packages/mcp-server/`) | TypeScript 빌드·공유 계약 검사·100개 테스트 통과, 실패 0 |

Rust와 MCP의 ACL·로컬 HTTP/WebSocket 테스트는 승인된 샌드박스 외부 실행에서 통과했습니다. MCP 테스트의 샌드박스 내부 시도에서는 로컬 통신·권한 검사가 포함된 항목에 실패가 발생해 중단했고, 외부에서 전체를 재실행했습니다. 이 점검에서는 Clippy·일반/릴리스 빌드·Rust 준비 상태 재현 도구를 다시 실행하지 않았습니다. 해당 결과는 아래의 기존 기록과 구분합니다.

이번 테스트는 로컬 테스트 서버·가짜 브로커·임시 자격 파일을 사용했습니다. 실제 중앙 인증·DB 갱신·검증된 사용자/팀 신원 전달·Rust 세션/업무 API·두 사용자 업무 왕복의 통과를 의미하지 않습니다. GitHub 연결에서 저장소 검색 접근 오류가 발생해 원격 PR·이슈·최신 브랜치 상태는 확인하지 못했습니다.

## 2026-10-28: 통합 검사 도구 호환성·presence 변경 보고 보완

기준 브랜치는 `feature/rust/broekr_sessionapi`, 병합 기준은 `037810b`다. 격리 Rust 재현 도구의 중앙 주소를 `/ws/broker`로 변경하고, MCP 준비 상태 검사기에 `Replaced`·`ProtocolViolation`을 추가했다. 두 상태도 유효한 health로 인식한 뒤 `CENTRAL_NOT_CONNECTED`로 보고하고 11개 경로 검사를 계속한다.

Rust는 HTTP 핸들러와 중앙 연결 태스크가 같은 `CentralState`를 공유한다. `set_presence`에 확인된 상태를 게시하면 READY 이후 현재 연결에서 보고하며, 동일 상태는 중복 전송하지 않는다. 연결 전·재접속 중에는 최신 상태를 보존하고 다음 READY 직후 보고한다. 기본 미확인 상태는 유지하며 실제 세션 API의 bind·해제·만료 이벤트 연결은 아직 남아 있다.

Windows MSVC와 Node.js 24 환경에서 다음 검사를 수행했다. ACL·로컬 HTTP/WebSocket 검사는 샌드박스 밖에서 실행했다. 앞선 병렬 검토에서 5초 종료 시간 조건 실패가 관측되어 이번 전체 Rust 검증은 순차 실행했다.

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 13개·통합 27개 통과 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 통과 |
| `npm.cmd test` (`packages/mcp-server/`) | 빌드·공유 계약 검사·100개 테스트 통과 |
| 실제 CLI로 `scripts/verify-rust-readiness.mjs --cli <절대 경로>` 실행 | 자격 파일 valid, health 200, 세션·업무 11개 OPTIONS 404, `blocked` 및 실제 자식 종료 코드 2 확인 |

presence 검증은 실제 로컬 WebSocket 테스트 서버에서 READY 전 전송 보류, 연결 유지 중 ONLINE/OFFLINE 변경, 동일 상태 중복 생략, ACK 없이 연속 보고, 재접속 중 최신 상태 보존·READY 직후 재보고, 종료 후 식별자 정리를 확인했다. 실제 중앙 서버·운영 토큰·DB·실제 Host 세션을 사용한 검증은 아니다. 격리 재현은 중앙 503 서버와 미구현 API를 의도적으로 사용하므로 차단 판정이 예상 결과이며 MVP 통과를 뜻하지 않는다.

## 2026-10-28: PR #11 P2 후속 수정 (S15P31D108-46)

최신 develop `69f155a`에서 확인한 Unix `libc` 의존성 누락과 Windows ACL 검사·기동 시간 예산 충돌을 수정했습니다. Unix 전용 직접 의존성을 workspace manifest와 lockfile에 반영했으며, Windows 시작 대기는 기존 준비 여유 8초에 최대 3회 ACL 검사(각 5초)를 포함한 23초입니다. Unix 시작 대기는 기존 8초를 유지합니다. 권한 검사는 생략하지 않습니다.

Windows MSVC에서 다음 검사를 통과했습니다. 기본 GNU 도구 체인에는 `dlltool`이 없어 설치된 MSVC를 명시했습니다.

```powershell
cargo +stable-x86_64-pc-windows-msvc test --locked --offline --workspace --all-targets
cargo +stable-x86_64-pc-windows-msvc clippy --locked --offline --workspace --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-msvc fmt --all -- --check
```

단위 7개·통합 22개가 통과했습니다. 신규 `startup_waits_for_valid_slow_acl_checks`는 합성 자격 파일과 동적 포트를 사용하고, 테스트가 실행한 start CLI 자식의 PATH에만 네이티브 PowerShell 프록시를 지정해 브로커가 상속하도록 합니다. 실제 ACL 스크립트마다 3초를 추가한 상태에서 검사 3회가 모두 실행되고, 8초를 넘겨 정상 기동하며 기존 토큰·새 주소·status·stop이 유지되는지 확인합니다. 프록시는 테스트에서 설치된 `rustc`로 임시 디렉터리에 컴파일하며 운영 실행 파일에 지연 옵션을 추가하지 않습니다. 실제 MCP reader 검사와 기존 토큰 유지·회전·권한 분리 테스트도 통과했습니다.

`cargo metadata --locked --offline --no-deps`로 broker의 `libc` 직접 의존성과 `cfg(unix)` 조건을 확인했습니다. 이 Windows 환경에 Unix 실행 도구 체인이 없어 Linux/macOS 전체 빌드·실행은 미검증입니다. 실제 `/v1` 업무 API나 중앙 서버 연동 검증을 의미하지 않습니다.

## 이전 확인: 2026-10-27

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
