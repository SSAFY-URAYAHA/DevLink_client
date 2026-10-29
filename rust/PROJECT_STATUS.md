# CLI·브로커 진행 현황

2026-10-29 READY 신원 연결: 중앙 담당자와 합의한 user_id/team_id를 받아 실제 ReadyIdentityProvider로 로컬 세션을 생성·bind하도록 연결했다. 같은 신원 재접속은 lease를 유지하고 신원 변경은 이전 세션·ONLINE을 무효화한다. [상세 계약](docs/ready-identity.md)이 아래 과거 미구성 공급부 설명보다 우선한다. 토큰 발급·업무 REST·복구 API·실제 운영 중앙/Host 검증은 남아 있다.

2026-10-29 추가: 작업 수신함·저장 후 ACK와 재시작 후 중복 수신 방지를 구현했습니다. [상세 문서](docs/task-inbox.md), [검증 기록](docs/verification.md)을 참고하세요. 아래 2026-10-28 현황에서 수신함·ACK 미구현으로 기재된 부분은 이번 추가로 대체하며, 중앙 신원·업무 API·실제 Host 왕복은 계속 미완료입니다.

- 갱신일: 2026-10-28 (Asia/Seoul)
- 확인 기준: 로컬 소스 코드·작업 트리와 같은 날 실행한 검사 결과
- 현재 브랜치: `feature/rust/session-api`
- 기준 커밋: `b8bd142` (PR #14 병합). 아래 현황은 후속 로컬 세션 구현을 포함합니다. 이번 작업은 커밋·Push 없이 작업 트리에 남깁니다.

## 1. 현재 상태

Rust CLI·독립 브로커, MCP 자격·권한 분리와 중앙 WebSocket·JSON Heartbeat에 이어 로컬 세션 API 4개·소유권·90초 만료·presence를 구현했습니다. `packages/mcp-server`에는 TypeScript MCP 서버와 7개 업무 도구가 있습니다. 남은 선행 연결은 **중앙이 검증한 사용자·팀·Agent를 제공하는 IdentityProvider**입니다.

HTTP 라우터에는 health/shutdown과 세션 생성·bind·heartbeat·해제가 있습니다. 기본 CLI의 신원 공급부는 미구성이어서 생성 요청을 503 CENTRAL_UNAUTHENTICATED로 거부합니다. 성공 흐름은 테스트 내부 신원 공급부로 검증했으며, 업무 API 7개·실제 중앙 신원 전달·Host 업무 왕복은 아직 남아 있습니다. 상세 계약은 [로컬 세션 API](docs/local-sessions.md)를 참고하세요.

| 영역 | 현재 상태 | 확인 내용·남은 조건 |
| --- | --- | --- |
| Rust 구조 | 구현 | `devlink-cli` 실행 파일·`devlink-broker` 라이브러리의 Cargo workspace |
| CLI·설정 | 구현 | init/start/stop/status/config, mcp path/rotate, 설정 검증·원자 저장 |
| 브로커 수명 주기 | 구현 | 독립 프로세스·OS 잠금·정상 종료·강제 종료 후 복구 |
| MCP 자격·권한 | 구현 | broker.json 원자 발행·Windows 권한 검사·토큰 유지·중지 중 회전·CLI/MCP 권한 분리 |
| 중앙 연결 | 로컬 검증 | `/ws/broker` Bearer 인증·READY·presence 변경 보고·재연결·종료 분류. 실제 중앙 인증·DB 연동은 미검증 |
| MCP 서버·업무 도구 | 구현·가짜 브로커 검증 | stdio MCP 서버·업무 도구 7개·세션 복구 클라이언트. 실제 Rust 세션·업무 연동 필요 |
| Rust 로컬 세션 | 구현·로컬 검증 | HTTP 4개 경로·fresh/recover·90초 lease·presence. READY 기반 실제 신원 공급부 연결, 신원 변경 시 소유권 무효화. 운영 중앙 검증 필요 |
| 작업 수신함·ACK | 구현·로컬 검증 | 기존 TASK_CREATED/ACK 형식, 디스크 원자 저장·재시작 중복 방지·실패 시 ACK 차단. 저장 후 ACK·기본 5초 방향 확인, 운영 검증 필요 |
| Rust 업무 API | 미구현 | 업무 7개 경로·중앙 인증/DTO·라우팅·수신함 연결 필요 |
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

Upgrade HTTP `401`·`403`은 AuthFailed, Close `4002`는 Replaced, `1008`·잘못된 READY는 ProtocolViolation으로 분류해 자동 재시도를 중단합니다. Close `4001`은 서버의 하트비트 만료이며 재시도 가능한 단절로 처리합니다. Rust는 JSON HEARTBEAT PING 20초 간격·같은 ping_id PONG 최대 20초 대기를 적용합니다. 현재 로컬 서버의 JSON PING 무수신 기본 만료는 40초입니다. 다른 메시지나 제어 프레임으로 PONG 대기를 연장하지 않습니다. 로컬 소켓 검증과 실제 중앙 배포 환경 검증은 구분하며 상세 규칙은 [중앙 WebSocket 연동 기준](docs/central-websocket-contract.md)을 따릅니다.

## 4. 최근 검증 결과

2026-10-28 로컬 세션 구현 후 다음 검사를 실행했습니다. ACL·로컬 HTTP/WebSocket 테스트는 승인된 샌드박스 외부에서 실행했습니다.

| 검사 | 결과 |
| --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 통과 |
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 통과 |
| `cargo test --offline --locked --workspace --all-targets -- --test-threads=1` | 단위 27개·세션 HTTP/WebSocket 6개·CLI 통합 30개 통과 |

MCP 전체 테스트·일반/릴리스 빌드·준비 상태 재현 도구는 이번 추가에서 다시 실행하지 않았습니다. MCP 100개 테스트 등 기존 실행 결과는 [로컬 검증 기록](docs/verification.md)에 실행 시점별로 보관합니다. JSON Heartbeat에는 시간 단축 방식의 만료·재접속 테스트와 실제 CLI를 45초 동안 같은 연결로 유지하는 자동 회귀 테스트가 있습니다. 후자는 기본 전체 테스트에 포함하며 PING 20초·PONG 대기 20초·서버 만료 40초를 단축하지 않습니다. 로컬 검증 통과는 실제 중앙 인증·DB·Host 업무 왕복 통과를 의미하지 않습니다.

## 5. 다음 작업 순서

1. **검증된 중앙 신원·인증 연결**: userId/teamId 전달 API/메시지·Agent 소유 검증・최초 등록/토큰 발급 순서를 합의하고 IdentityProvider를 구현합니다. 기본 미구성 오류를 임의의 신원으로 우회하지 않습니다.
2. **실제 MCP 세션 검증**: 실제 공급부와 Host로 생성·bind·갱신·해제·선복구·충돌·만료·재시작을 검증합니다. 이번 로컬 테스트용 신원 주입과 구분합니다.
3. **실제 중앙 상태 확인**: 운영 Agent 토큰으로 Upgrade → READY → PRESENCE → DB 갱신과 JSON Heartbeat 유지·만료·교체를 확인합니다.
4. **업무 API·중앙 라우팅 구현**: 업무 7개 경로, 요청·응답·오류 매핑, timeout 후 결과 조회, 수신함 디스크 기록·ACK·중복 방지·전달 결과를 연결합니다. 업무 요청을 자동 재전송하거나 로컬 만료로 중앙 작업을 임의 실패 처리하지 않습니다.
5. **실제 통합 검증**: 두 사용자·프로젝트의 양방향 왕복과 실패·권한·중복·timeout 시나리오를 [MVP 검증 절차](../packages/mcp-server/docs/mvp-integration.md)에 따라 수행합니다.
6. **Windows/Ubuntu CI 구성·원격 확인**: 로컬 검사를 자동화하고 Unix 실행 결과를 확인합니다.

세션 API의 구현·검증 범위는 [로컬 세션 문서](docs/local-sessions.md)와 [MCP → 로컬 브로커 계약](../packages/mcp-server/docs/broker-contract-v0.1.md)을 따릅니다. 기존 MCP 규칙은 2026-10-27 채택했으며 재시작 전 소유권은 보존하지 않고 먼저 유효하게 복구한 인스턴스가 빈 슬롯을 확보합니다.

## 6. 별도 후속 범위

중앙 Agent ID 영속 재사용·등록 응답 유실의 멱등성, Host별 프로필·설치/등록·Node 배포 정책, Agent 프로그램 자체 실행·종료, 프론트엔드와 재부팅 후 자동 실행·Windows 서비스 등록은 후속 범위입니다. 세션 목록·실행 취소·로그 구독을 현재 세션·업무 계약에 임의로 추가하지 않습니다.
