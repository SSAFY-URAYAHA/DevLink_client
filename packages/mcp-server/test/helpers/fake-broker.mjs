import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';

/** Test double for the MCP-side draft contract. This is not the Rust implementation. */
export async function fakeBroker() {
  const token = 'test-local-token-private-1234';
  const calls = [];
  const sessions = new Map();
  const owners = new Map();
  const instances = new Map();
  let now = 0;
  let hook;
  const server = createServer(async (req, res) => {
    try {
      let text = '';
      for await (const chunk of req) text += chunk;
      const body = JSON.parse(text);
      const path = req.url;
      calls.push({ path, body, method: req.method });
      const send = (data, status = 200) => {
        if (!res.destroyed) res.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify({ requestId: body.requestId, ...data }));
      };
      const fail = (code) => send({ error: { code } }, 409);
      if (req.headers.authorization !== `Bearer ${token}`) return send({ error: { code: 'AUTH' } }, 401);
      if (hook && await hook({ path, body, req, res, send })) return;
      if (path === '/v1/sessions') {
        const previous = sessions.get(instances.get(body.instanceId));
        if (previous && !previous.replaced && now - previous.seen < 90_000) return send({ data: previous.view });
        const owner = sessions.get(owners.get(body.agentId));
        if (owner && now - owner.seen < 90_000) {
          if (body.mode === 'recover') return fail('SESSION_CONFLICT');
          owner.replaced = true;
        }
        const sessionId = randomUUID();
        const view = { sessionId, principal: { userId: 'trusted-user', teamId: 'trusted-team', ...(body.agentId ? { agentId: body.agentId } : {}) } };
        sessions.set(sessionId, { view, instanceId: body.instanceId, seen: now });
        instances.set(body.instanceId, sessionId);
        if (body.agentId) owners.set(body.agentId, sessionId);
        return send({ data: view });
      }
      const id = path.startsWith('/v1/sessions/') ? path.split('/')[3] : body.context?.sessionId;
      const session = sessions.get(id);
      if (!session || now - session.seen >= 90_000) return fail('SESSION_EXPIRED');
      if (session.replaced) return fail('SESSION_REPLACED');
      if (path.startsWith('/v1/sessions/')) {
        if (body.instanceId !== session.instanceId) return fail('SESSION_CONFLICT');
        if (req.method === 'DELETE') {
          if (owners.get(session.view.principal.agentId) === id) owners.delete(session.view.principal.agentId);
          sessions.delete(id);
          return send({ data: {} });
        }
        if (path.endsWith('/bind')) {
          const owner = sessions.get(owners.get(body.agentId));
          if (owner && owner !== session && now - owner.seen < 90_000) return fail('SESSION_CONFLICT');
          session.view.principal.agentId = body.agentId;
          owners.set(body.agentId, id);
        }
        session.seen = now; // Broker clock only; client timestamps have no authority.
        return send({ data: session.view });
      }
      const principal = session.view.principal;
      if (body.context.userId !== principal.userId || body.context.teamId !== principal.teamId || body.context.agentId !== principal.agentId) return send({ error: { code: 'FORBIDDEN' } }, 403);
      if (body.operation === 'register_agent') return send({ data: { agentId: principal.agentId ?? 'registered-agent' } });
      if (!principal.agentId) return fail('AGENT_UNREGISTERED');
      return send({ data: { taskId: 'task-1', status: 'PENDING', delivery: { status: 'FAILED' }, echo: body.arguments } });
    } catch {
      if (!res.destroyed) res.writeHead(500).end();
    }
  });
  server.listen(0, '127.0.0.1');
  await new Promise((resolve) => server.once('listening', resolve));
  return {
    calls, sessions, owners,
    settings: { schemaVersion: 1, brokerUrl: `http://127.0.0.1:${server.address().port}`, localToken: token },
    setHook(fn) { hook = fn; },
    advance(ms) { now += ms; },
    restart() { server.closeAllConnections(); sessions.clear(); owners.clear(); instances.clear(); },
    async close() { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); },
  };
}
