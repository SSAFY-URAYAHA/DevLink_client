# DevLink MCP 실행 기반

S15P31D108-59의 TypeScript·Node.js MCP 실행 모듈입니다. MCP Host가 stdio 자식 프로세스로 실행하며, 현재 부작용 없는 `devlink_ping` 도구 하나를 제공합니다. 브로커 연결이나 인증·업무 도구는 후속 작업입니다.

## 설치와 실행

Node.js 24 이상이 필요합니다. 현재 Windows, Node.js 24.18.0, npm 12.0.2에서 검증했습니다. 아래 명령은 저장소 루트에서 실행합니다.

```powershell
npm --prefix packages/mcp-server ci
npm --prefix packages/mcp-server run build
node packages/mcp-server/dist/index.js --transport stdio
```

서버는 stdin에서 MCP 요청을 기다립니다. 명령이 시작됐다는 사실만으로 Host 연결이 완료된 것은 아닙니다. 진단 로그는 stderr, JSON-RPC 메시지는 stdout에 출력합니다. Host에는 빌드된 JavaScript를 `node`로 직접 실행하는 명령을 설정합니다.

환경변수로도 설정할 수 있습니다.

```powershell
$env:DEVLINK_MCP_TRANSPORT = 'stdio'
$env:DEVLINK_MCP_LOG_LEVEL = 'debug'
node packages/mcp-server/dist/index.js
```

| 설정 | 인자 | 환경변수 | 규칙 |
| --- | --- | --- | --- |
| 전송 방식 | `--transport` | `DEVLINK_MCP_TRANSPORT` | 필수, 현재 `stdio`만 지원 |
| 로그 수준 | `--log-level` | `DEVLINK_MCP_LOG_LEVEL` | `debug`, `info`, `error`, 기본값 `info` |

인자는 환경변수보다 우선합니다. 모르는 인자, 누락된 필수 설정, 지원하지 않는 값은 `CONFIG_INVALID`로 종료합니다. 로그에는 설정 필드명만 기록하고 입력값·토큰·임의 환경변수는 기록하지 않습니다. 브로커 주소나 인증 토큰은 이 단계의 필수 설정이 아닙니다.

## MCP Host 설정 예시

다음은 `mcpServers` 형식을 사용하는 Host용 예시입니다. `command`와 첫 번째 인자는 각 컴퓨터의 실제 절대 경로로 변경합니다. 자동으로 사용자 설정 파일을 수정하지 않습니다.

```json
{
  "mcpServers": {
    "devlink": {
      "command": "C:\\Program Files\\nodejs\\node.exe",
      "args": [
        "C:\\Users\\SSAFY\\Documents\\DevLink\\packages\\mcp-server\\dist\\index.js",
        "--transport",
        "stdio"
      ]
    }
  }
}
```

Codex는 같은 실행 명령을 다음 TOML 형태로 설정합니다. [공식 MCP 설정 문서](https://learn.chatgpt.com/docs/extend/mcp)의 `command`·`args` 형식을 따릅니다.

```toml
[mcp_servers.devlink]
command = 'C:\Program Files\nodejs\node.exe'
args = ['C:\Users\SSAFY\Documents\DevLink\packages\mcp-server\dist\index.js', '--transport', 'stdio']
enabled_tools = ['devlink_ping']
```

MCP SDK 2.4.0의 `serveStdio`가 기존 `initialize` 경로와 현대 프로토콜 경로를 처리합니다. `devlink_ping`은 입력 `{}`에 대해 다음 구조를 텍스트와 `structuredContent`로 반환합니다. 버전은 MCP 패키지 버전이며 브로커·중앙 서버의 준비 상태를 뜻하지 않습니다.

```json
{ "status": "ok", "version": "0.1.0" }
```

## 검증

```powershell
npm --prefix packages/mcp-server run check
npm --prefix packages/mcp-server test
```

테스트는 실제 Node 자식 프로세스와 공식 SDK Client를 사용합니다.

- legacy와 `2026-07-28` 프로토콜에서 도구 목록·호출 후 각각 두 번 실행합니다.
- 필수 설정 누락·잘못된 값은 종료 코드 1, stdout 0바이트와 구조화된 stderr 사유로 확인합니다.
- 30회 도구 호출 중 stdout의 모든 줄이 JSON-RPC인지, 로그와 테스트용 비밀값이 섞이지 않는지 검사합니다.
- stdin EOF 후 강제 종료 없이 코드 0으로 종료하는지, 초기화 전 EOF 후에도 재실행할 수 있는지 확인합니다.
- 명시적 close의 중복 호출과 stdin 리스너 정리를 확인합니다.

테스트의 5초 제한은 테스트 실패를 감지하는 harness 값입니다. 브로커 요청·ACK·업무 처리의 운영 제한 시간이 아닙니다.

2026-10-26에 Claude Code 2.1.310과 Codex CLI 0.164.0-alpha.11.1에서도 일회성 설정으로 `devlink_ping({})`의 실제 도구 호출과 결과를 확인했습니다. Codex 공유 데몬에서는 일회성 설정의 도구가 노출되지 않아 `--no-daemon` 실행으로 확인했습니다. 이는 최소 연결 점검이며 S15P31D108-64의 전체 오류·인증·브로커 호환성 검증은 후속 작업입니다.

## 종료와 담당 경계

Host가 stdin을 닫으면 SDK가 서버·전송을 정리합니다. 직접 실행할 때 Ctrl+C로도 닫을 수 있습니다. `SIGINT`·`SIGTERM` 핸들러는 보조 경로이며 Windows 검증은 EOF와 명시적 close를 기준으로 합니다. 종료 시 `process.exit()`로 출력을 강제로 끊지 않습니다.

`src/runtime.ts`는 전송과 인스턴스를, `src/server.ts`는 서버 팩터리와 예제 도구 등록을 맡습니다. 로거는 stderr만 사용하며 SDK 내부의 stdout 함수를 재정의하지 않습니다. 공유 브로커 실행·종료·중앙 연결·토큰 보관은 CLI 담당 범위로 유지합니다.

S15P31D108-50의 실행 주체·설정 전달 계약과 S15P31D108-28 산출물 확인은 남아 있습니다. 이 모듈의 검증 결과를 해당 합의나 두 Agent의 전체 업무 왕복 검증으로 간주하지 않습니다.
