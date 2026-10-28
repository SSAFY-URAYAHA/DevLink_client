# CLI·브로커 진행 현황

- 갱신일: 2026-10-28 (Asia/Seoul)
- 확인 기준: 로컬 소스 코드·작업 트리와 같은 날 실행한 검사 결과
- 현재 브랜치: `feature/rust/broekr_sessionapi`
- HEAD: `037810b` (develop 병합). 아래 구현 상태는 미커밋 presence·통합 검사 보완을 포함합니다.
- 원격 PR·이슈·최신 브랜치 상태는 GitHub 연결 접근 오류로 확인하지 못했습니다.

## 1. 현재 상태

Rust CLI·독립 로컬 브로커, MCP 자격 파일·토큰 관리·권한 분리와 중앙 WebSocket 인증·READY·presence 보고가 구현돼 있습니다. `packages/mcp-server`에는 TypeScript MCP 서버와 7개 업무 도구가 있습니다. 다음 개발 작업은 **Rust 세션 API 4개·소유권·만료 처리와 presence 연결**입니다.

Rust의 HTTP 라우터는 현재 `/health`와 `/shutdown`만 제공합니다. MCP의 세션 4개·업무 7개 경로, 실제 중앙 신원 전달과 업무 왕복 연동은 아직 구현·검증이 필요합니다.

| 영역 | 현재 상태 | 확인 내용·남은 조건 |
| --- | --- | --- |
| Rust 구조 | 구현 | `devlink-cli` 실행 파일·`devlink-broker` 라이브러리의 Cargo workspace |
| CLI·설정 | 구현 | init/start/stop/status/config, mcp path/rotate, 설정 검증·원자 저장 |
| 브로커 수명 주기 | 구현 | 독립 프로세스·OS 잠금·정상 종료·강제 종료 후 복구 |
| MCP 자격·권한 | 구현 | broker.json 원자 발행·Windows 권한 검사·토큰 유지·중지 중 회전·CLI/MCP 권한 분리 |
| 중앙 연결 | 로컬 검증 | `/ws/broker` Bearer 인증·READY·presence 변경 보고·재연결·종료 분류. 실제 중앙 인증·DB 연동은 미검증 |
| MCP 서버·업무 도구 | 구현·가짜 브로커 검증 | stdio MCP 서버·업무 도구 7개·세션 복구 클라이언트. 실제 Rust 세션·업무 연동 필요 |
| Rust 세션·업무 API | 미구현 | 세션 4개·업무 7개 경로, 검증된 userId/teamId·Agent 소유권 연결 필요 |
| 실제 MVP 통합 | 미검증 | 서로 다른 두 사용자·프로젝트에서 요청·수락·편집·결과 조회까지 검증 필요 |
| CI·Unix | 미검증 | `.github/workflows/` 없음. Ubuntu 빌드·테스트와 Unix 권한 분기 실행 필요 |

## 2. 구현된 자격 파일·토큰 관리

설정 디렉터리의 `.devlink-runtime/`에 내부 파일을 저장합니다. 별도 설정 디렉터리는 별도 브로커·자격 파일을 사용합니다.

| 파일·자격 | 용도·수명 주기 |
| --- | --- |
| `runtime.json`의 controlToken | CLI 제어용. 기동마다 재생성하며 health·shutdown 허용 |
| `broker.json`의 localToken | MCP용. 정상·강제 종료 후 재시작에 유지하며 현재 health만 허용, shutdown 거부 |
| `broker.lock`·`control.lock` | 중복 실행 및 start/stop/토큰 회전 조정 |
| 환경변수 `DEVLINK_TOKEN` | 중앙 Agent 인증용. MCP 자격 파일에 포함하지 않음 |

`devlink [--config <FILE>] mcp path`는 자격 파일의 절대 경로만 출력합니다. 브로커 기동 시 `schemaVersion`, `brokerUrl`, `localToken`을 원자적으로 발행합니다. 현재 Rust 발행 스키마에는 `agentId`가 없으며 Agent ID 영속 저장·재사용은 후속 작업입니다.

`devlink mcp rotate`는 브로커 중지 중에만 토큰을 교체합니다. 자격 파일과 직계 부모의 링크·Windows junction을 거부하고 Windows 소유권·DACL을 설정·검증합니다. Unix 소유권·권한 검사 코드도 있지만 이 환경에서 실행하지 않았습니다. 손상되거나 권한이 안전하지 않은 기존 자격은 기동 시 오류로 처리합니다.

MCP는 주소·토큰을 실행 중 고정하므로 회전·주소 변경 후 재시작해야 합니다. `localPort: 0`은 기동마다 주소가 달라질 수 있습니다. Host별 프로필·신원을 하나의 브로커에서 관리하는 제품 정책은 별도 후속 항목입니다. 사용법은 [Rust README](README.md#mcp-자격-파일과-토큰)를 참고하세요.

## 3. 중앙 연결과 presence

현재 Rust는 `/ws/broker` WebSocket Upgrade 요청의 Bearer 토큰으로 인증하고 유효한 `BROKER_READY`를 기다립니다. 연결 후 `auth`·`hello` JSON을 보내지 않습니다. READY의 `connection_id`·양의 정수 `agent_id`를 검증하며 사용자·팀 필드를 임의로 추가하지 않습니다.

HTTP 핸들러와 중앙 태스크가 같은 `CentralState`를 공유합니다. `set_presence`로 확인된 상태를 게시하면 READY·연결 중 변경·재접속 후 READY에서 보고하고 같은 상태는 중복 전송하지 않습니다. 기본 상태는 미확인이고 실제 세션의 생성·bind·해제·만료 이벤트는 아직 연결되지 않았습니다.

`401`·`403`·`4001`은 AuthFailed, `4002`는 Replaced, `1008`·잘못된 READY는 ProtocolViolation으로 분류합니다. 재시도 가능한 단절에는 백오프를 적용합니다. 현재 Rust Ping 5초·15초 초과 무응답 판정과 중앙 초기안 20초·20초·40초의 최종 정합성 확인이 남아 있습니다. 상세 규칙은 [중앙 WebSocket 연동 기준](docs/central-websocket-contract.md)을 따릅니다.

## 4. 최근 검증 결과

2026-10-28 작업 현황 점검에서 다음 검사를 재확인했습니다. ACL·로컬 HTTP/WebSocket 테스트는 승인된 샌드박스 외부에서 실행했습니다.

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 13개·통합 27개 통과 |
| `npm.cmd test` (`packages/mcp-server/`) | 빌드·공유 계약 검사·100개 테스트 통과 |

Clippy·일반/릴리스 빌드·준비 상태 재현 도구는 이번 점검에서 다시 실행하지 않았습니다. 기존 실행 결과와 샌드박스 실패·외부 실행 통과의 구분은 [로컬 검증 기록](docs/verification.md)에 보관합니다. 가짜 브로커·로컬 WebSocket 테스트 통과는 실제 중앙 인증·DB·Host 업무 왕복 통과를 의미하지 않습니다.

## 5. 다음 작업 순서

1. **현재 변경 정리·공유**: 문서와 presence·통합 검사 보완을 검토한 뒤 현재 브랜치에 커밋·Push합니다. 문서 갱신 자체는 커밋·Push 완료를 의미하지 않습니다.
2. **Rust 세션 API 4개 구현**: 세션 생성·bind·heartbeat·해제를 구현하고 instanceId 멱등성, fresh/recover 소유권, 교체·충돌, 브로커 시계 기준 90초 만료와 오래된 갱신·해제 차단을 적용합니다. 확인된 세션 상태를 presence에 연결합니다.
3. **검증된 중앙 신원·인증 연결**: 세션 구현과 함께 userId/teamId 전달·Agent 소유 검증 경로를 확인합니다. 실제 Agent 토큰으로 Upgrade → READY → PRESENCE → DB 갱신, 해제·만료·재접속·연결 교체를 검증하고 Heartbeat 최종값을 맞춥니다. 신원은 클라이언트 입력이나 READY만으로 추측하지 않습니다.
4. **업무 API·중앙 라우팅 구현**: 업무 7개 경로, 요청·응답·오류 매핑, timeout 후 결과 조회, 수신함 디스크 기록·ACK·중복 방지·전달 결과를 연결합니다. 업무 요청을 자동 재전송하거나 로컬 만료로 중앙 작업을 임의 실패 처리하지 않습니다.
5. **실제 통합 검증**: 두 사용자·프로젝트의 양방향 왕복과 실패·권한·중복·timeout 시나리오를 [MVP 검증 절차](../packages/mcp-server/docs/mvp-integration.md)에 따라 수행합니다.
6. **Windows/Ubuntu CI 구성·원격 확인**: 로컬 검사를 자동화하고 Unix 실행 결과를 확인합니다.

세션 API의 상세 구현 기준은 [MCP 인터페이스](docs/mcp-interface.md#다음-구현-rust-세션-api)와 [MCP → 로컬 브로커 계약](../packages/mcp-server/docs/broker-contract-v0.1.md)을 따릅니다. 기존 MCP 규칙은 2026-10-27 채택했으며 재시작 전 소유권은 보존하지 않고 먼저 유효하게 복구한 인스턴스가 빈 슬롯을 확보합니다.

## 6. 별도 후속 범위

중앙 Agent ID 영속 재사용·등록 응답 유실의 멱등성, Host별 프로필·설치/등록·Node 배포 정책, Agent 프로그램 자체 실행·종료, 프론트엔드와 재부팅 후 자동 실행·Windows 서비스 등록은 후속 범위입니다. 세션 목록·실행 취소·로그 구독을 현재 세션·업무 계약에 임의로 추가하지 않습니다.
