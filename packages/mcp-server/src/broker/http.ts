import { randomUUID } from 'node:crypto';
import { z } from 'zod';
import { BrokerFault, type FaultCode } from './contracts.js';
import type { BrokerSettings } from './settings.js';

const envelopeSchema = z.object({ requestId: z.string(), data: z.record(z.string(), z.unknown()).optional(),
  error: z.object({ code: z.string(), taskId: z.string().optional() }).optional() });
const remoteCodes = new Set<FaultCode>([
  'SESSION_REPLACED', 'SESSION_CONFLICT', 'SESSION_EXPIRED', 'AGENT_UNREGISTERED',
]);

export class BrokerHttp {
  private readonly pending = new Set<AbortController>();
  private closed = false;
  constructor(private readonly settings: BrokerSettings, private readonly timeoutMs = 10_000) {}
  get pendingCount(): number { return this.pending.size; }

  async request(
    path: string,
    payload: Record<string, unknown>,
    options: { method?: 'POST' | 'DELETE'; signal?: AbortSignal; timeoutMs?: number; duringClose?: boolean } = {},
  ): Promise<Record<string, unknown>> {
    const requestId = typeof payload.requestId === 'string' ? payload.requestId : randomUUID();
    if (this.closed && !options.duringClose) throw new BrokerFault('CLIENT_CLOSED', requestId);
    if (!path.startsWith('/v1/') || path.startsWith('//') || path.includes('..')) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
    const controller = new AbortController();
    const cancel = () => controller.abort();
    if (options.signal?.aborted) controller.abort();
    options.signal?.addEventListener('abort', cancel, { once: true });
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; controller.abort(); }, options.timeoutMs ?? this.timeoutMs);
    this.pending.add(controller);
    try {
      const body = JSON.stringify({ ...payload, requestId });
      if (Buffer.byteLength(body) > 131_072) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      const response = await fetch(`${this.settings.brokerUrl}${path}`, {
        method: options.method ?? 'POST', redirect: 'manual', signal: controller.signal,
        headers: { authorization: `Bearer ${this.settings.localToken}`, 'content-type': 'application/json' }, body,
      });
      if (response.status >= 300 && response.status < 400) {
        await response.body?.cancel();
        throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      }
      if (response.status === 401 || response.status === 403) {
        await response.body?.cancel();
        throw new BrokerFault(response.status === 401 ? 'BROKER_UNAUTHORIZED' : 'BROKER_FORBIDDEN', requestId);
      }
      const reader = response.body?.getReader();
      const chunks: Uint8Array[] = [];
      let length = 0;
      if (reader) {
        try {
          while (true) {
            const part = await reader.read();
            if (part.done) break;
            length += part.value.byteLength;
            if (length > 262_144) { await reader.cancel(); throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId); }
            chunks.push(part.value);
          }
        } finally { reader.releaseLock(); }
      }
      const parsed = envelopeSchema.safeParse(JSON.parse(Buffer.concat(chunks).toString('utf8')));
      if (!parsed.success || parsed.data.requestId !== requestId) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      if (!response.ok || parsed.data.error) {
        const remote = parsed.data.error;
        const code: FaultCode = response.status === 401 ? 'BROKER_UNAUTHORIZED'
          : response.status === 403 ? 'BROKER_FORBIDDEN'
          : remote && remoteCodes.has(remote.code as FaultCode) ? remote.code as FaultCode : 'BROKER_REJECTED';
        throw new BrokerFault(code, requestId, remote?.taskId);
      }
      if (!parsed.data.data || parsed.data.error) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      return parsed.data.data;
    } catch (error) {
      if (error instanceof BrokerFault) throw error;
      if (timedOut) throw new BrokerFault('BROKER_TIMEOUT', requestId);
      if (controller.signal.aborted) throw new BrokerFault(this.closed ? 'CLIENT_CLOSED' : 'REQUEST_ABORTED', requestId);
      if (error instanceof SyntaxError) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      throw new BrokerFault('BROKER_UNAVAILABLE', requestId);
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener('abort', cancel);
      this.pending.delete(controller);
    }
  }

  close(): void {
    this.closed = true;
    for (const controller of this.pending) controller.abort();
  }
}
