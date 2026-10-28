export { principalSchema, sessionSchema, type BrokerSession } from './schemas.js';

/** MCP-side v0.1 contract; verify these routes against the Rust broker before integration. */
export const operationRoutes = {
  register_agent: '/v1/agents/register',
  list_agents: '/v1/agents/list',
  create_task: '/v1/tasks/create',
  update_task_status: '/v1/tasks/status',
  complete_task: '/v1/tasks/complete',
  get_tasks: '/v1/tasks/list',
  get_task_result: '/v1/tasks/result',
} as const;
export type BrokerOperation = keyof typeof operationRoutes;

export type BrokerResult = { requestId: string; data: Record<string, unknown> };

export type FaultCode =
  | 'BROKER_UNAVAILABLE' | 'BROKER_TIMEOUT' | 'BROKER_PROTOCOL_ERROR'
  | 'BROKER_UNAUTHORIZED' | 'BROKER_FORBIDDEN' | 'BROKER_REJECTED'
  | 'SESSION_REPLACED' | 'SESSION_CONFLICT' | 'SESSION_EXPIRED'
  | 'AGENT_UNREGISTERED' | 'CREDENTIAL_FILE_INVALID' | 'CLIENT_CLOSED'
  | 'REQUEST_ABORTED' | 'BROKER_CONTEXT_INVALID' | 'SESSION_CONTEXT_CHANGED' | 'CALLER_CONTEXT_OVERRIDE';

const messages: Record<FaultCode, string> = {
  BROKER_UNAVAILABLE: 'Local broker is unavailable.',
  BROKER_TIMEOUT: 'Local broker response timed out; the business outcome is unknown.',
  BROKER_PROTOCOL_ERROR: 'Local broker response does not match the contract.',
  BROKER_UNAUTHORIZED: 'Local credentials were rejected. Restart MCP after checking the CLI settings.',
  BROKER_FORBIDDEN: 'The broker denied this operation.',
  BROKER_REJECTED: 'The broker rejected the operation.',
  SESSION_REPLACED: 'This session was replaced. Automatic recovery has stopped.',
  SESSION_CONFLICT: 'Another instance owns this agent. Recovery cannot replace it.',
  SESSION_EXPIRED: 'The local session expired.',
  AGENT_UNREGISTERED: 'Register the agent before calling business tools.',
  CREDENTIAL_FILE_INVALID: 'The broker settings file is missing, invalid, or not private.',
  CLIENT_CLOSED: 'The MCP broker client has closed.',
  REQUEST_ABORTED: 'The local request was aborted; this does not cancel a central task.',
  BROKER_CONTEXT_INVALID: 'The broker returned missing or inconsistent authenticated identity. Restart MCP after checking authentication.',
  SESSION_CONTEXT_CHANGED: 'The session changed while the request was in flight; its business outcome is unknown.',
  CALLER_CONTEXT_OVERRIDE: 'Tool arguments cannot override authenticated caller identity.',
};

export class BrokerFault extends Error {
  readonly outcome: 'unknown' | 'rejected';
  constructor(
    readonly code: FaultCode,
    readonly requestId: string,
    readonly taskId?: string,
  ) {
    super(messages[code]);
    this.name = 'BrokerFault';
    this.outcome = ['BROKER_TIMEOUT', 'BROKER_UNAVAILABLE', 'BROKER_PROTOCOL_ERROR', 'BROKER_CONTEXT_INVALID', 'SESSION_CONTEXT_CHANGED', 'REQUEST_ABORTED', 'CLIENT_CLOSED'].includes(code)
      ? 'unknown' : 'rejected';
  }
}

export const terminalSessionFaults = new Set<FaultCode>([
  'SESSION_REPLACED', 'SESSION_CONFLICT', 'BROKER_UNAUTHORIZED', 'BROKER_FORBIDDEN', 'CREDENTIAL_FILE_INVALID',
  'BROKER_CONTEXT_INVALID',
]);
