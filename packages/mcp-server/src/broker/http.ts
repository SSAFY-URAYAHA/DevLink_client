import { randomUUID } from 'node:crypto';
import { Agent, request as requestHttp } from 'node:http';
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
    // Credentials must never pass through an ambient HTTP(S)_PROXY or a global fetch dispatcher.
    const agent = new Agent({ keepAlive: false, proxyEnv: {} });
    try {
      const body = JSON.stringify({ ...payload, requestId });
      if (Buffer.byteLength(body) > 131_072) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      const response = await new Promise<{ status: number; body: Buffer }>((resolve, reject) => {
        const request = requestHttp(`${this.settings.brokerUrl}${path}`, {
          agent, method: options.method ?? 'POST', signal: controller.signal,
          headers: { authorization: `Bearer ${this.settings.localToken}`, 'content-type': 'application/json' },
        }, (incoming) => {
          const status = incoming.statusCode ?? 0;
          incoming.on('error', reject);
          if ((status >= 300 && status < 400) || status === 401 || status === 403) {
            reject(new BrokerFault(status === 401 ? 'BROKER_UNAUTHORIZED' : status === 403 ? 'BROKER_FORBIDDEN' : 'BROKER_PROTOCOL_ERROR', requestId));
            incoming.destroy();
            return;
          }
          const chunks: Buffer[] = [];
          let length = 0;
          incoming.on('data', (chunk: Buffer) => {
            length += chunk.length;
            if (length > 262_144) {
              reject(new BrokerFault('BROKER_PROTOCOL_ERROR', requestId));
              incoming.destroy();
            } else chunks.push(chunk);
          });
          incoming.on('end', () => resolve({ status, body: Buffer.concat(chunks) }));
        });
        request.on('error', reject);
        request.end(body);
      });
      const parsed = envelopeSchema.safeParse(JSON.parse(response.body.toString('utf8')));
      if (!parsed.success || parsed.data.requestId !== requestId) throw new BrokerFault('BROKER_PROTOCOL_ERROR', requestId);
      if (response.status < 200 || response.status >= 300 || parsed.data.error) {
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
      agent.destroy();
    }
  }

  close(): void {
    this.closed = true;
    for (const controller of this.pending) controller.abort();
  }
}
