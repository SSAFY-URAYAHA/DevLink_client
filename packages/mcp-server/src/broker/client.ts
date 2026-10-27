import { randomUUID } from 'node:crypto';
import { BrokerFault, operationRoutes, terminalSessionFaults,
  type BrokerOperation, type BrokerResult, type FaultCode } from './contracts.js';
import { identifierSchema, immutableSession, sessionSchema, type BrokerSession } from './schemas.js';
import { BrokerHttp } from './http.js';
import { readBrokerSettings, validateSettings, type BrokerSettings } from './settings.js';
import type { Logger } from '../logger.js';

export interface BrokerClientOptions {
  settingsPath?: string;
  settings?: BrokerSettings;
  requestTimeoutMs?: number;
  shutdownTimeoutMs?: number;
  heartbeatMs?: number;
  retryBaseMs?: number;
  retryLimit?: number;
  logger?: Logger;
}
type CallOptions = { signal?: AbortSignal; mcpRequestId?: string | number };
const actorFields = ['userId', 'teamId', 'agentId', 'sessionId', 'principal', 'context'];

function cancellable<T>(result: Promise<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return result;
  return new Promise<T>((resolve, reject) => {
    const cancel = () => reject(new BrokerFault('REQUEST_ABORTED', 'registration'));
    if (signal.aborted) cancel();
    else signal.addEventListener('abort', cancel, { once: true });
    void result.then((value) => { signal.removeEventListener('abort', cancel); resolve(value); },
      (error: unknown) => { signal.removeEventListener('abort', cancel); reject(error); });
  });
}

/** One client per MCP process; only authenticated broker responses may change identity. */
export class BrokerClient {
  private readonly instanceId = randomUUID();
  private transportPromise: Promise<BrokerHttp> | undefined;
  private http: BrokerHttp | undefined;
  private session: BrokerSession | undefined;
  private identity: Readonly<{ userId: string; teamId: string }> | undefined;
  private agentId: string | undefined;
  private pendingAgentId: string | undefined;
  private toolType: string | undefined;
  private connecting: Promise<BrokerSession> | undefined;
  private registrationTail: Promise<void> | undefined;
  private epoch = 0;
  private claimAttempted = false;
  private terminal: FaultCode | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private retryCount = 0;
  private closed = false;
  private closePromise: Promise<void> | undefined;

  constructor(private readonly options: BrokerClientOptions = {}) {}

  get pendingCount(): number { return this.http?.pendingCount ?? 0; }
  get state(): string { return this.closed ? 'closed' : this.terminal ?? (this.session?.principal.agentId ? 'bound' : this.session ? 'unbound' : 'inactive'); }

  private async transport(): Promise<BrokerHttp> {
    this.transportPromise ??= (async () => {
      let settings: BrokerSettings;
      try {
        settings = this.options.settings ? validateSettings(this.options.settings)
          : await readBrokerSettings(this.options.settingsPath ?? '');
      } catch { throw new BrokerFault('CREDENTIAL_FILE_INVALID', 'settings'); }
      if (this.closed) throw new BrokerFault('CLIENT_CLOSED', 'settings');
      this.agentId ??= settings.agentId;
      return this.http = new BrokerHttp(settings, this.options.requestTimeoutMs);
    })();
    return this.transportPromise;
  }

  private schedule(delay = this.options.heartbeatMs ?? 30_000): void {
    clearTimeout(this.timer);
    if (this.closed || this.terminal) return;
    this.timer = setTimeout(() => { void this.heartbeat().catch(() => {}); }, delay);
    this.timer.unref();
  }

  private discard(snapshot?: BrokerSession): void {
    if (snapshot && this.session !== snapshot) return;
    this.session = undefined;
    ++this.epoch;
    clearTimeout(this.timer);
  }

  private observe(error: unknown, snapshot?: BrokerSession): void {
    if (!(error instanceof BrokerFault) || this.closed) return;
    // Old requests may report errors, but cannot revoke a newer binding or lease.
    if (snapshot && this.session !== snapshot) return;
    if (terminalSessionFaults.has(error.code)) {
      this.terminal = error.code;
      this.discard();
    } else if (['BROKER_UNAVAILABLE', 'BROKER_TIMEOUT', 'SESSION_EXPIRED'].includes(error.code)) {
      this.discard();
      this.retry();
    }
  }

  private retry(): void {
    if (this.retryCount < (this.options.retryLimit ?? 5)) {
      this.schedule(Math.min((this.options.retryBaseMs ?? 1000) * 2 ** this.retryCount++, 30_000));
    }
  }

  private checkedSession(value: unknown, requestId: string, refresh = false): BrokerSession {
    const result = sessionSchema.safeParse(value);
    if (!result.success) throw new BrokerFault('BROKER_CONTEXT_INVALID', requestId);
    const { principal } = result.data;
    if ((this.identity && (principal.userId !== this.identity.userId || principal.teamId !== this.identity.teamId))
      || (this.agentId && principal.agentId !== this.agentId)
      || (this.pendingAgentId && principal.agentId && principal.agentId !== this.pendingAgentId)
      || (this.toolType && (!principal.toolType || (!refresh && principal.toolType !== this.toolType)))) {
      throw new BrokerFault('BROKER_CONTEXT_INVALID', requestId);
    }
    return immutableSession(result.data);
  }

  private publish(session: BrokerSession): void {
    this.identity ??= Object.freeze({ userId: session.principal.userId, teamId: session.principal.teamId });
    this.agentId = session.principal.agentId ?? this.agentId;
    if (session.principal.agentId) this.pendingAgentId = undefined;
    this.toolType = session.principal.toolType ?? this.toolType;
    this.session = session;
    ++this.epoch;
    this.schedule();
  }

  private assertCurrent(snapshot: BrokerSession, requestId: string): void {
    if (this.closed) throw new BrokerFault('CLIENT_CLOSED', requestId);
    if (this.terminal) throw new BrokerFault(this.terminal, requestId);
    if (this.session !== snapshot) throw new BrokerFault('SESSION_CONTEXT_CHANGED', requestId);
  }

  private ensureSession(): Promise<BrokerSession> {
    if (this.closed) return Promise.reject(new BrokerFault('CLIENT_CLOSED', 'session'));
    if (this.terminal) return Promise.reject(new BrokerFault(this.terminal, 'session'));
    if (this.session) return Promise.resolve(this.session);
    this.connecting ??= (async () => {
      const epoch = this.epoch;
      const http = await this.transport();
      const requestId = randomUUID();
      const mode = this.claimAttempted ? 'recover' : 'fresh';
      this.claimAttempted = true;
      const data = await http.request('/v1/sessions', {
        requestId, instanceId: this.instanceId, mode,
        ...((this.agentId ?? this.pendingAgentId) ? { agentId: this.agentId ?? this.pendingAgentId } : {}),
      });
      const session = this.checkedSession(data, requestId);
      if (this.closed) throw new BrokerFault('CLIENT_CLOSED', requestId);
      if (this.terminal) throw new BrokerFault(this.terminal, requestId);
      if (epoch !== this.epoch) throw new BrokerFault('SESSION_CONTEXT_CHANGED', requestId);
      this.publish(session);
      this.options.logger?.debug('broker.session_ready', { requestId });
      return session;
    })().catch((error: unknown) => {
      // A shared connection failure consumes one retry, regardless of waiter count.
      this.observe(error);
      throw error;
    }).finally(() => { this.connecting = undefined; });
    return this.connecting;
  }

  async heartbeat(): Promise<void> {
    let snapshot: BrokerSession | undefined;
    try {
      snapshot = await this.ensureSession();
      const http = await this.transport();
      await http.request(`/v1/sessions/${encodeURIComponent(snapshot.sessionId)}/heartbeat`, {
        requestId: randomUUID(), instanceId: this.instanceId,
      });
      if (this.session === snapshot) {
        this.retryCount = 0;
        this.schedule();
      }
    } catch (error) { if (snapshot) this.observe(error, snapshot); throw error; }
  }

  private enqueueRegistration(run: () => Promise<BrokerResult>): Promise<BrokerResult> {
    const result = (this.registrationTail ?? Promise.resolve()).then(run);
    const settled = result.then(() => {}, () => {});
    this.registrationTail = settled;
    void settled.then(() => { if (this.registrationTail === settled) this.registrationTail = undefined; });
    return result;
  }

  private async waitForRegistration(signal?: AbortSignal): Promise<void> {
    while (this.registrationTail) {
      await cancellable(this.registrationTail, signal);
    }
  }

  async execute(operation: BrokerOperation, args: Record<string, unknown>, options: CallOptions = {}): Promise<BrokerResult> {
    const requestId = randomUUID();
    const callOptions = { ...options };
    this.options.logger?.debug('broker.call_started', { requestId, operation });
    try {
      if (!Object.hasOwn(operationRoutes, operation)) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      const input = structuredClone(args);
      if (actorFields.some((field) => Object.hasOwn(input, field))) throw new BrokerFault('CALLER_CONTEXT_OVERRIDE', requestId);
      if (callOptions.signal?.aborted) throw new BrokerFault('REQUEST_ABORTED', requestId);
      const run = () => this.perform(operation, input, requestId, callOptions);
      let result: BrokerResult;
      if (operation === 'register_agent') result = await cancellable(this.enqueueRegistration(run), callOptions.signal);
      else {
        await this.waitForRegistration(callOptions.signal);
        result = await run();
      }
      this.options.logger?.debug('broker.call_succeeded', { requestId, operation });
      return result;
    } catch (error) {
      const fault = error instanceof BrokerFault ? new BrokerFault(error.code, requestId, error.taskId)
        : new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      this.options.logger?.error('broker.call_failed', { requestId, operation, code: fault.code });
      throw fault;
    }
  }

  private async perform(operation: BrokerOperation, args: Record<string, unknown>, requestId: string, options: CallOptions): Promise<BrokerResult> {
    let snapshot: BrokerSession | undefined;
    let binding = false;
    try {
      if (options.signal?.aborted) throw new BrokerFault('REQUEST_ABORTED', requestId);
      snapshot = await this.ensureSession();
      if (operation !== 'register_agent' && !snapshot.principal.agentId) throw new BrokerFault('AGENT_UNREGISTERED', requestId);
      this.assertCurrent(snapshot, requestId);
      if (options.signal?.aborted) throw new BrokerFault('REQUEST_ABORTED', requestId);
      const http = await this.transport();
      const signalOptions = options.signal ? { signal: options.signal } : {};
      const data = await http.request(operationRoutes[operation], {
        requestId, operation, arguments: args,
        context: { ...snapshot.principal, sessionId: snapshot.sessionId },
        ...(options.mcpRequestId !== undefined ? { mcpRequestId: options.mcpRequestId } : {}),
      }, signalOptions);
      if (this.closed) throw new BrokerFault('CLIENT_CLOSED', requestId);
      if (operation === 'register_agent') {
        const agent = identifierSchema.safeParse(data.agentId);
        const expectedAgentId = this.agentId ?? this.pendingAgentId;
        if (!agent.success || (expectedAgentId && expectedAgentId !== agent.data)) throw new BrokerFault('BROKER_CONTEXT_INVALID', requestId);
        // An issued ID survives lease loss, but is not verified binding until bind succeeds.
        if (!this.agentId) this.pendingAgentId = agent.data;
        this.assertCurrent(snapshot, requestId);
        binding = true;
        const bound = await http.request(`/v1/sessions/${encodeURIComponent(snapshot.sessionId)}/bind`, {
          requestId: randomUUID(), agentId: agent.data, instanceId: this.instanceId,
        }, signalOptions);
        this.assertCurrent(snapshot, requestId);
        const session = this.checkedSession(bound, requestId, true);
        if (session.sessionId !== snapshot.sessionId || session.principal.agentId !== agent.data) throw new BrokerFault('BROKER_CONTEXT_INVALID', requestId);
        this.publish(session);
      }
      this.retryCount = 0;
      return { requestId, data };
    } catch (error) {
      if (snapshot) this.observe(error, snapshot);
      if (binding && this.session === snapshot) { this.discard(snapshot); this.retry(); }
      throw error;
    }
  }

  close(): Promise<void> {
    this.closePromise ??= (async () => {
      this.closed = true;
      const session = this.session;
      this.discard();
      this.http?.close();
      if (session && this.http) {
        await this.http.request(`/v1/sessions/${encodeURIComponent(session.sessionId)}`, {
          requestId: randomUUID(), instanceId: this.instanceId,
        }, { method: 'DELETE', timeoutMs: this.options.shutdownTimeoutMs ?? 1000, duringClose: true }).catch(() => {});
      }
    })();
    return this.closePromise;
  }
}
