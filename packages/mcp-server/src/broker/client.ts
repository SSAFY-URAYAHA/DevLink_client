import { randomUUID } from 'node:crypto';
import { BrokerFault, operationRoutes, sessionSchema, terminalSessionFaults,
  type BrokerOperation, type BrokerResult, type BrokerSession, type FaultCode } from './contracts.js';
import { BrokerHttp } from './http.js';
import { readBrokerSettings, validateSettings, type BrokerSettings } from './settings.js';

export interface BrokerClientOptions {
  settingsPath?: string;
  settings?: BrokerSettings;
  requestTimeoutMs?: number;
  shutdownTimeoutMs?: number;
  heartbeatMs?: number;
  retryBaseMs?: number;
  retryLimit?: number;
}

/** Lazy activation: discovery and diagnostic calls never create or replace a session. */
export class BrokerClient {
  private readonly instanceId = randomUUID();
  private transportPromise: Promise<BrokerHttp> | undefined;
  private http: BrokerHttp | undefined;
  private session: BrokerSession | undefined;
  private agentId: string | undefined;
  private connecting: Promise<BrokerSession> | undefined;
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

  private observe(error: unknown): void {
    if (!(error instanceof BrokerFault) || this.closed) return;
    if (terminalSessionFaults.has(error.code)) {
      this.terminal = error.code;
      this.session = undefined;
      clearTimeout(this.timer);
    } else if (['BROKER_UNAVAILABLE', 'BROKER_TIMEOUT', 'SESSION_EXPIRED'].includes(error.code)) {
      this.session = undefined;
      clearTimeout(this.timer);
      if (this.retryCount < (this.options.retryLimit ?? 5)) {
        this.schedule(Math.min((this.options.retryBaseMs ?? 1000) * 2 ** this.retryCount++, 30_000));
      }
    }
  }

  private ensureSession(): Promise<BrokerSession> {
    if (this.closed) return Promise.reject(new BrokerFault('CLIENT_CLOSED', 'session'));
    if (this.terminal) return Promise.reject(new BrokerFault(this.terminal, 'session'));
    if (this.session) return Promise.resolve(this.session);
    this.connecting ??= (async () => {
      const http = await this.transport();
      const mode = this.claimAttempted ? 'recover' : 'fresh';
      this.claimAttempted = true;
      const data = await http.request('/v1/sessions', {
        requestId: randomUUID(), instanceId: this.instanceId, mode,
        ...(this.agentId ? { agentId: this.agentId } : {}),
      });
      const parsed = sessionSchema.safeParse(data);
      if (!parsed.success || (this.agentId && parsed.data.principal.agentId !== this.agentId)) throw new BrokerFault('BROKER_PROTOCOL_ERROR', 'session');
      if (this.closed || this.terminal) throw new BrokerFault('CLIENT_CLOSED', 'session');
      this.session = parsed.data;
      this.agentId = parsed.data.principal.agentId ?? this.agentId;
      this.schedule();
      return this.session;
    })().finally(() => { this.connecting = undefined; });
    return this.connecting;
  }

  async heartbeat(): Promise<void> {
    try {
      const session = await this.ensureSession();
      const http = await this.transport();
      await http.request(`/v1/sessions/${encodeURIComponent(session.sessionId)}/heartbeat`, {
        requestId: randomUUID(), instanceId: this.instanceId,
      });
      this.retryCount = 0;
      this.schedule();
    } catch (error) { this.observe(error); throw error; }
  }

  async execute(
    operation: BrokerOperation,
    args: Record<string, unknown>,
    options: { signal?: AbortSignal; mcpRequestId?: string | number } = {},
  ): Promise<BrokerResult> {
    const requestId = randomUUID();
    try {
      if (options.signal?.aborted) throw new BrokerFault('REQUEST_ABORTED', requestId);
      let session = await this.ensureSession();
      if (operation !== 'register_agent' && !session.principal.agentId) throw new BrokerFault('AGENT_UNREGISTERED', requestId);
      if (options.signal?.aborted) throw new BrokerFault('REQUEST_ABORTED', requestId);
      const http = await this.transport();
      const data = await http.request(operationRoutes[operation], {
        requestId, operation, arguments: args,
        context: { ...session.principal, sessionId: session.sessionId },
        ...(options.mcpRequestId !== undefined ? { mcpRequestId: options.mcpRequestId } : {}),
      }, options.signal ? { signal: options.signal } : {});
      if (this.closed) throw new BrokerFault('CLIENT_CLOSED', requestId);
      if (operation === 'register_agent') {
        if (typeof data.agentId !== 'string' || !data.agentId) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
        if (this.agentId && this.agentId !== data.agentId) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
        this.agentId = data.agentId;
        const bound = await http.request(`/v1/sessions/${encodeURIComponent(session.sessionId)}/bind`, {
          requestId: randomUUID(), agentId: this.agentId, instanceId: this.instanceId,
        });
        const parsed = sessionSchema.safeParse(bound);
        if (!parsed.success || parsed.data.sessionId !== session.sessionId || parsed.data.principal.agentId !== this.agentId) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
        session = parsed.data;
        if (this.closed || this.terminal) throw new BrokerFault('CLIENT_CLOSED', requestId);
        this.session = session;
      }
      this.retryCount = 0;
      return { requestId, data };
    } catch (error) {
      this.observe(error);
      if (error instanceof BrokerFault) throw new BrokerFault(error.code, requestId, error.taskId);
      throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
    }
  }

  close(): Promise<void> {
    this.closePromise ??= (async () => {
      this.closed = true;
      clearTimeout(this.timer);
      const session = this.session;
      this.session = undefined;
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
