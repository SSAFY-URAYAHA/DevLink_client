# 도구 계약·확장 예제 v0.1 (S15P31D108-61)

2026-10-27 MCP 측 선구현 초안. 현재 Notion API명세의 7개 도구 페이지에는 요청·응답 JSON이 채워져 있지 않다. 아래 필드명·열거값·필수 여부는 담당자 확인이 필요한 초안이다. 중앙 업무 규칙과 저장 처리는 구현하지 않는다.

## 공개 도구와 입력

`devlink_ping` 진단과 아래 7개 업무 도구를 등록한다. 별도 activate 도구는 없다. 모든 입력 객체는 strict다. 호출자의 userId/teamId/agentId/sessionId/context 필드를 허용하지 않는다. `recipientAgentId`는 작업 대상이며 발신 actor가 아니다. 브로커가 인증한 세션에서 context를 주입한다.

아래 객체의 각 값은 해당 도구의 `arguments` 예제다.

```json
{
  "register_agent": {"name":"MCP worker","toolType":"codex","role":"MCP 구현"},
  "list_agents": {},
  "create_task": {"recipientAgentId":"agent-b","title":"인터페이스 검토","description":"계약 초안을 확인해주세요","taskContext":"관련 Jira: S15P31D108-60"},
  "update_task_status": {"taskId":"task-1","status":"FAILED","reason":"의존 API 미제공","partiallyModified":false},
  "complete_task": {"taskId":"task-1","result":{"summary":"구현 완료","changedFiles":["src/example.ts"],"verification":"npm test 통과"}},
  "get_tasks": {"direction":"received","status":"PENDING","limit":20},
  "get_task_result": {"taskId":"task-1"}
}
```

| 도구 | 입력 규칙 |
| --- | --- |
| register_agent | name 1–100자, toolType `codex / claude_code`, 선택 role 1–500자. 성공 data.agentId로 세션 bind 후 반환 |
| list_agents | 빈 객체 |
| create_task | recipientAgentId 1–128자, title 1–200자, description 1–16000자, 선택 taskContext 1–16000자 |
| update_task_status | taskId 1–128자, status `IN_PROGRESS / FAILED`, 선택 reason·partiallyModified. FAILED에는 reason 필수. 완료는 complete_task 사용 |
| complete_task | taskId, result.summary 필수. 선택 changedFiles 문자열 배열(최대 500개), verification 문자열. 결과 객체도 strict |
| get_tasks | direction `sent / received`(기본 received), 선택 status `PENDING / IN_PROGRESS / COMPLETED / FAILED`, limit 1–100(기본 20), 선택 cursor 1–512자 |
| get_task_result | taskId 1–128자 |

문자열 입력은 명시된 필드에서 trim하고 빈 값을 거부한다. JSON Schema는 `tools/list`로 제공하고, 조건부 FAILED 사유도 실행 전 검증한다. create/complete 등의 전체 요청은 [HTTP 계약](broker-contract-v0.1.md)의 128KiB 제한도 적용한다. 실패 작업을 다시 요청하는 것과 기존 작업 상태를 변경하는 것은 중앙 도메인의 판단이다.

## 공통 호출·응답

SDK 입력 검증 → 공통 등록 콜백 → 지연 세션 준비/신뢰 context 주입 → 명시적 HTTP 경로 → 공통 출력 검증/매핑 순서다. 각 도구 핸들러는 HTTP·토큰·세션을 직접 다루지 않는다. taskId/status/recipient/delivery 등 성공 data의 확장 필드를 그대로 보존한다.

```json
{"requestId":"UUID","data":{"taskId":"task-1","status":"PENDING","delivery":{"status":"FAILED"}}}
```

업무 실행 실패는 `isError: true`와 다음 structuredContent를 반환한다. content[0].text에도 같은 JSON을 넣는다. SDK의 입력 거부는 해당 프로토콜의 표준 validation 오류 경로를 사용하며 브로커에 요청하지 않는다.

```json
{"error":{"code":"BROKER_TIMEOUT","message":"Local broker response timed out; the business outcome is unknown.","requestId":"UUID","outcome":"unknown"}}
```

알려진 taskId는 오류에 보존한다. HTTP 원문 오류와 임의 예외 메시지는 반환하지 않는다. 타임아웃·취소는 중앙 작업 취소/실패를 뜻하지 않으며 업무를 자동 재전송하지 않는다. list/get은 작업을 수락하거나 상태를 변경하지 않는다.

## 모듈 추가 방법

`src/examples/echo.ts`는 공통 등록 구조를 사용하는 실행 가능한 예제다. 기본 공개 목록에는 넣지 않는다. 새 모듈은 다음 형태로 작성한다.

```typescript
import { z } from 'zod';
import { defineTool } from '../tools/registry.js';

export const echoExample = defineTool({
  name: 'example_echo',
  description: 'Return a short message without external I/O.',
  inputSchema: z.strictObject({ message: z.string().min(1).max(200) }),
  outputSchema: z.strictObject({ message: z.string() }),
  annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  handler: async ({ message }) => ({ message }),
});
```

구성 지점에서 `createServer(logger, broker, [echoExample])`로 추가한다. 등록 코어를 수정하지 않는다. 모듈은 name·description·inputSchema·outputSchema·annotations·handler만 정의한다. 공통 함수는 중복 이름을 사전 거부하고 정상 결과를 검증하며 안전한 공통 오류로 변환한다. 업무 예제는 `businessTools(broker)`의 명시적 operation과 스키마 정의를 참고한다.

## 검증 및 남은 확인

- 가짜 브로커로 7개 도구의 성공 경로와 신뢰 context·requestId를 확인했다.
- 모든 도구의 잘못된 입력·actor 덮어쓰기가 네트워크 전 거부되는지 확인했다.
- 공통 실행 오류, 예제 추가, 출력 검증 실패 시 내부 정보 비노출을 확인했다.
- legacy·2026-07-28의 stdio 자식 프로세스에서 개인 설정 파일을 거쳐 7개 도구를 호출했다.
- 브로커 없이 진단 사용, 업무 처리 중 EOF로 타이머·요청 정리를 확인했다.

실제 Claude Code/Codex Host의 새 업무 도구 사용성, Rust 브로커와의 DTO·세션 계약 대조, 도구 담당자 확인, 중앙 서버와 두 Agent의 왕복은 아직 검증하지 않았다. 이전 -59의 실제 Host ping 검증과 구분한다.
