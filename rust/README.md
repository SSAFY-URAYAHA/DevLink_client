# DevLink

Rust CLI와 독립 로컬 브로커입니다. CLI는 설정과 프로세스 수명 주기를 관리하고, 브로커는 로컬 제어 API와 중앙 서버 WebSocket 연결을 관리합니다.

이 디렉터리는 `devlink-cli`(실행 파일)와 `devlink-broker`(라이브러리)로 구성한 Cargo workspace입니다. CLI가 브로커 라이브러리를 참조하며 브로커는 CLI에 의존하지 않습니다. 아래 명령은 저장소 루트에서 `cd rust`로 이동한 뒤 실행합니다.

## Windows 개발 환경

1. Visual Studio 2022 Build Tools의 **Desktop development with C++** 워크로드와 Windows SDK를 설치합니다.
2. [공식 Rust 설치 프로그램](https://www.rust-lang.org/tools/install)으로 MSVC stable 도구 체인을 설치합니다.
3. 새 PowerShell에서 아래 명령을 실행합니다. PATH가 갱신되지 않았다면 `$env:USERPROFILE\.cargo\bin\cargo.exe`를 사용합니다.

```powershell
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --release --locked --workspace
.\target\release\devlink.exe --help
```

`rust-toolchain.toml`은 stable, rustfmt, clippy를 선택합니다. 의존성 버전은 이 디렉터리의 `Cargo.lock`으로 고정하며 공통 의존성은 workspace의 `Cargo.toml`에서 관리합니다. 빌드 산출물은 `rust/target/`에 생성됩니다. Windows MSVC에서 검증하며 CI는 Windows와 Ubuntu에서 같은 검사를 수행합니다. Linux에서도 stable Rust로 빌드할 수 있습니다.

## 빠른 실행

첫 번째 PowerShell에서 Mock 중앙 서버를 실행합니다. 예제 값은 개발용이며 실제 인증 토큰은 설정 파일이나 명령행 인수에 넣지 않습니다.

```powershell
$env:DEVLINK_TOKEN = 'local-development-token'
cargo run -p devlink-broker --example mock_server
```

두 번째 PowerShell에서 **동일한** 토큰을 설정하고 CLI를 실행합니다.

```powershell
$env:DEVLINK_TOKEN = 'local-development-token'
cargo run -p devlink-cli -- init
cargo run -p devlink-cli -- config show
cargo run -p devlink-cli -- start
cargo run -p devlink-cli -- status
cargo run -p devlink-cli -- stop
```

빌드한 실행 파일을 PATH에 추가하면 `devlink init`, `devlink start`, `devlink status`, `devlink stop`으로 사용할 수 있습니다. CLI가 종료되어도 브로커는 계속 실행되며 Windows에서 별도 콘솔 창을 만들지 않습니다. 운영체제 재부팅 후 자동 실행이나 Windows 서비스 등록은 포함하지 않습니다.

Mock 서버를 Ctrl+C로 종료하면 `status`가 `Reconnecting`으로 바뀝니다. 같은 포트·토큰으로 서버를 다시 실행하면 `Connected`로 돌아옵니다. `Connected`는 WebSocket 연결뿐 아니라 `auth_ok` 인증 응답까지 받은 상태입니다. `auth_error` 또는 종료 코드 `4001`은 `AuthFailed`가 되고 재시도하지 않습니다. 토큰을 고친 뒤 `stop`/`start`로 다시 시작합니다.

## 설정

기본 경로는 `$HOME/.devlink/config.json`입니다. 모든 명령에 `--config <FILE>`을 지정할 수 있습니다. 공백·한글을 포함하는 경로도 지원합니다.

```powershell
cargo run -p devlink-cli -- --config .\.devlink\config.json init
cargo run -p devlink-cli -- --config .\.devlink\config.json config set serverUrl ws://127.0.0.1:8080
cargo run -p devlink-cli -- --config .\.devlink\config.json config set autoReconnect false
cargo run -p devlink-cli -- --config .\.devlink\config.json start
```

`--config`를 사용한 인스턴스는 `status`/`stop`에도 같은 경로를 지정해야 합니다. **설정 디렉터리 하나당 브로커 하나**를 실행합니다. 별도 인스턴스는 별도 디렉터리를 사용하고 `localPort`를 다르게 지정하거나 `0`으로 설정합니다.

| 키 | 기본값 | 제약 |
| --- | --- | --- |
| `serverUrl` | `ws://localhost:8080` | ws/wss URL. 사용자 정보·쿼리·프래그먼트 금지 |
| `agent` | `codex` | 영문·숫자·`-`·`_`, 1~64자 |
| `autoReconnect` | `true` | JSON boolean |
| `localPort` | `43127` | 0~65535, 0은 OS 자동 할당 |
| `reconnectInitialMs` | `2000` | 50 이상, 최대 지연 이하 |
| `reconnectMaxMs` | `30000` | 초기 지연 이상, 300000 이하 |
| `authTimeoutMs` | `5000` | 100~60000 |

`config`와 `config show`는 동일합니다. `config set <키> <값>`은 검증한 뒤 임시 파일을 원자적으로 교체합니다. 잘못된 값·알 수 없는 키는 실패하며 기존 파일을 유지합니다. 설정 변경은 브로커 재시작 후 반영됩니다. `init`은 기존 파일을 덮어쓰지 않습니다. TypeScript의 `serverUrl`/`agent`/`autoReconnect` 3필드 설정도 읽을 수 있지만 기존 `.devlink-practice` 파일을 자동 이동하지는 않습니다.

중앙 서버 인증에는 `DEVLINK_TOKEN` 환경 변수만 사용합니다. 토큰은 필수이며 설정·상태·서버 응답·오류에 출력하지 않습니다. 로컬 API의 제어 토큰은 별개이며 같은 디렉터리의 `runtime.json`에 저장합니다. 이 디렉터리는 사용자 전용으로 관리하고 공유하거나 커밋하지 마세요. Unix JSON 파일은 0600 권한으로 생성하며 Windows는 디렉터리의 ACL을 상속합니다. 다른 로컬 관리자나 같은 사용자 계정의 프로세스에 대한 격리는 제공하지 않습니다.

## 상태와 오류 처리

```text
Local Broker  : Running (PID: 12345)
Central Server: Connected
```

중앙 연결 상태는 `Connecting`, `Authenticating`, `Connected`, `Reconnecting`, `AuthFailed`, `Disconnected`입니다. 재연결은 초기 지연부터 최대 지연까지 지수 백오프를 사용하며, 인증된 연결이 끊기면 초기 지연으로 돌아갑니다. 인증 후 5초마다 Ping을 보내고 15초 이상 응답·메시지가 없으면 재연결합니다. 연결 수립은 5초 제한, 인증 대기는 `authTimeoutMs` 제한입니다. `wss`는 OS 신뢰 저장소로 서버 인증서를 검증합니다.

브로커 실행 여부는 PID 파일 대신 OS 파일 잠금으로 판단합니다. 살아 있지만 Health Check가 실패한 프로세스는 오류로 보고하며 `Stopped`로 표시하지 않습니다. `stop`은 인증된 HTTP 종료 요청을 보내고 잠금 해제를 기다립니다. 잠금 파일은 삭제하지 않으며 정상 종료 시 `runtime.json`과 소켓·재연결 작업을 정리합니다. 강제 종료로 남은 런타임 정보는 다음 `status`/`start`/`stop`에서 잠금을 획득한 상태로 제거합니다. PID를 이용해 임의 프로세스를 강제 종료하지 않습니다.

명령/설정/시작 오류는 종료 코드 1, clap 사용법 오류는 2입니다. 이미 실행 중인 브로커의 `start`, 이미 중지된 브로커의 `stop`, 정상 `status`는 0입니다. 중앙 연결 실패는 브로커 자체의 실패가 아니므로 상태로 확인합니다.

시작 시 포트 충돌 등으로 실패하면 `devlink --config <FILE> broker`를 포그라운드에서 실행해 초기화 오류를 확인할 수 있습니다. `broker`는 내부 실행용이며 도움말에는 노출하지 않습니다.

## 코드와 테스트

| 위치 | 책임 |
| --- | --- |
| `crates/devlink-cli/src/main.rs` | CLI 진입점 |
| `crates/devlink-cli/src/cli.rs` | clap 명령·설정 관리, 자식 프로세스 실행·준비 확인·종료 대기 |
| `crates/devlink-broker/src/lib.rs` | 브로커 라이브러리 진입점 |
| `crates/devlink-broker/src/config.rs` | CLI와 브로커가 공유하는 설정 타입·검증·원자 저장·잠금 |
| `crates/devlink-broker/src/broker.rs` | HTTP 서버·종료 신호·태스크 정리 |
| `crates/devlink-broker/src/local.rs` | 런타임 발견·인증된 제어 클라이언트·Health 계약 |
| `crates/devlink-broker/src/central.rs` | 중앙 WebSocket 상태·인증·Heartbeat·재연결 |
| `crates/devlink-broker/src/protocol.rs` | 중앙 서버 메시지 DTO |
| `crates/devlink-broker/examples/mock_server.rs` | 개발용 중앙 서버 |
| `crates/devlink-cli/tests/cli_broker.rs` | 실제 CLI·브로커·Mock 서버 통합 테스트 |

테스트는 임시 디렉터리·동적 포트를 사용하며 사용자 설정을 변경하지 않습니다. 독립 실행, 동시 시작, 중복 방지, 정상/강제 종료, 포트 충돌, 토큰 비노출, 설정 오류, 로컬 API 인증, WebSocket 인증·서버 재시작·재연결·재시도 중 종료를 검증합니다.

[TypeScript 분석](docs/typescript-analysis.md), [MCP 실행 구조 및 내부 API 초안](docs/mcp-interface.md), [작업별 현황](docs/task-status.md)을 참고하세요. MCP 담당자와의 합의, 실제 MCP Tool과 중앙 서버 운영 프로토콜은 후속 협의 대상입니다.
