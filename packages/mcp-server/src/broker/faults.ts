import { identifierSchema } from './schemas.js';

export type FaultCode =
  | 'BROKER_UNAVAILABLE' | 'BROKER_CONNECTION_CLOSED' | 'BROKER_TIMEOUT' | 'BROKER_PROTOCOL_ERROR'
  | 'BROKER_UNAUTHORIZED' | 'LOCAL_AUTH_FORBIDDEN' | 'BROKER_FORBIDDEN' | 'BROKER_REJECTED' | 'BROKER_SERVER_ERROR'
  | 'SESSION_REPLACED' | 'SESSION_CONFLICT' | 'SESSION_EXPIRED'
  | 'AGENT_UNREGISTERED' | 'CREDENTIAL_FILE_INVALID' | 'CLIENT_CLOSED'
  | 'REQUEST_ABORTED' | 'BROKER_CONTEXT_INVALID' | 'SESSION_CONTEXT_CHANGED' | 'CALLER_CONTEXT_OVERRIDE'
  | 'CENTRAL_UNAUTHENTICATED' | 'VALIDATION_FAILED' | 'RECEIVER_NOT_FOUND' | 'RECEIVER_OFFLINE'
  | 'TASK_NOT_FOUND' | 'TASK_STATE_CONFLICT' | 'TASK_SAVE_FAILED'
  | 'CENTRAL_UNAVAILABLE' | 'CENTRAL_TIMEOUT' | 'TOOL_EXECUTION_FAILED';
export type FaultCategory = 'input' | 'authentication' | 'session' | 'transport' | 'protocol' | 'domain' | 'internal';
export type Outcome = 'unknown' | 'rejected';
export type Correlation = { requestKey?: string; taskId?: string; mcpRequestId?: string | number };

const definitions: Record<FaultCode, readonly [FaultCategory, Outcome, string]> = {
  BROKER_UNAVAILABLE: ['transport', 'unknown', 'Local broker is unavailable.'],
  BROKER_CONNECTION_CLOSED: ['transport', 'unknown', 'The broker connection closed; the business outcome is unknown.'],
  BROKER_TIMEOUT: ['transport', 'unknown', 'Local broker response timed out; the business outcome is unknown.'],
  BROKER_PROTOCOL_ERROR: ['protocol', 'unknown', 'Local broker response does not match the contract.'],
  BROKER_UNAUTHORIZED: ['authentication', 'rejected', 'Local credentials were rejected. Restart MCP after checking the CLI settings.'],
  LOCAL_AUTH_FORBIDDEN: ['authentication', 'rejected', 'The broker denied these local credentials. Check the CLI profile and restart MCP.'],
  BROKER_FORBIDDEN: ['domain', 'rejected', 'The broker denied this operation.'],
  BROKER_REJECTED: ['domain', 'rejected', 'The broker rejected the operation.'],
  BROKER_SERVER_ERROR: ['domain', 'unknown', 'The broker reported an unclassified server failure; the business outcome is unknown.'],
  SESSION_REPLACED: ['session', 'rejected', 'This session was replaced. Automatic recovery has stopped.'],
  SESSION_CONFLICT: ['session', 'rejected', 'Another instance owns this agent. Recovery cannot replace it.'],
  SESSION_EXPIRED: ['session', 'rejected', 'The local session expired.'],
  AGENT_UNREGISTERED: ['session', 'rejected', 'Register the agent before calling business tools.'],
  CREDENTIAL_FILE_INVALID: ['authentication', 'rejected', 'The broker settings file is missing, invalid, or not private.'],
  CLIENT_CLOSED: ['transport', 'unknown', 'The MCP broker client has closed.'],
  REQUEST_ABORTED: ['transport', 'unknown', 'The local request was aborted; this does not cancel a central task.'],
  BROKER_CONTEXT_INVALID: ['authentication', 'unknown', 'The broker returned missing or inconsistent authenticated identity. Restart MCP after checking authentication.'],
  SESSION_CONTEXT_CHANGED: ['session', 'unknown', 'The session changed while the request was in flight; its business outcome is unknown.'],
  CALLER_CONTEXT_OVERRIDE: ['input', 'rejected', 'Tool arguments cannot override authenticated caller identity.'],
  CENTRAL_UNAUTHENTICATED: ['authentication', 'rejected', 'Central authentication is required. Check the CLI authentication state.'],
  VALIDATION_FAILED: ['domain', 'rejected', 'The server rejected the business input.'],
  RECEIVER_NOT_FOUND: ['domain', 'rejected', 'The recipient does not exist.'],
  RECEIVER_OFFLINE: ['domain', 'rejected', 'The recipient is offline.'],
  TASK_NOT_FOUND: ['domain', 'rejected', 'The requested task does not exist.'],
  TASK_STATE_CONFLICT: ['domain', 'rejected', 'The task does not allow this state transition.'],
  TASK_SAVE_FAILED: ['domain', 'rejected', 'The server reported a task storage failure. Check the request outcome.'],
  CENTRAL_UNAVAILABLE: ['transport', 'unknown', 'The central server is unavailable; the business outcome is unknown.'],
  CENTRAL_TIMEOUT: ['transport', 'unknown', 'The central server response timed out; the business outcome is unknown.'],
  TOOL_EXECUTION_FAILED: ['internal', 'unknown', 'The tool could not produce a valid result.'],
};

export function safeIdentifier(value: unknown): string | undefined {
  const result = identifierSchema.safeParse(value);
  return result.success ? result.data : undefined;
}

/** All messages/categories come from local constants; remote messages are never copied. */
export class BrokerFault extends Error {
  readonly category: FaultCategory;
  readonly outcome: Outcome;
  readonly taskId: string | undefined;
  readonly requestKey: string | undefined;
  readonly mcpRequestId: string | number | undefined;
  constructor(readonly code: FaultCode, readonly requestId: string, taskId?: string,
    details: Correlation & { outcome?: Outcome } = {}) {
    super(definitions[code][2]);
    this.name = 'BrokerFault';
    this.category = definitions[code][0];
    this.outcome = details.outcome ?? definitions[code][1];
    this.taskId = safeIdentifier(taskId ?? details.taskId);
    this.requestKey = safeIdentifier(details.requestKey);
    this.mcpRequestId = details.mcpRequestId;
  }
}

export const terminalSessionFaults = new Set<FaultCode>([
  'SESSION_REPLACED', 'SESSION_CONFLICT', 'BROKER_UNAUTHORIZED', 'LOCAL_AUTH_FORBIDDEN',
  'CREDENTIAL_FILE_INVALID', 'BROKER_CONTEXT_INVALID',
]);
