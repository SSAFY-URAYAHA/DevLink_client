import { z } from 'zod';
import type { BrokerClient } from '../broker/client.js';
import type { BrokerOperation } from '../broker/contracts.js';
import { defineTool, type RegisteredDefinition } from './registry.js';

const id = z.string().trim().min(1).max(128);
const text = z.string().trim().min(1).max(16_000);
const taskStatus = z.enum(['PENDING', 'IN_PROGRESS', 'COMPLETED', 'FAILED']);
const brokerOutput = z.strictObject({ requestId: z.string(), data: z.record(z.string(), z.unknown()) });

/** Draft DTOs until the currently empty Notion API templates are agreed with tool owners. */
export function businessTools(broker: Pick<BrokerClient, 'execute'>): RegisteredDefinition[] {
  function tool<S extends z.ZodObject>(name: BrokerOperation, description: string, inputSchema: S, readOnly = false) {
    return defineTool({ name, description, inputSchema, outputSchema: brokerOutput,
      annotations: { readOnlyHint: readOnly, destructiveHint: !readOnly, idempotentHint: readOnly, openWorldHint: true },
      handler: (args, ctx) => broker.execute(name, args, { signal: ctx.signal, mcpRequestId: ctx.mcpRequestId }),
    });
  }
  return [
    tool('register_agent', 'Register this agent for the authenticated user/team, then bind its local session. Call before business tools on a new installation.', z.strictObject({
      name: z.string().trim().min(1).max(100), toolType: z.enum(['codex', 'claude_code']), role: z.string().trim().min(1).max(500).optional(),
    })),
    tool('list_agents', 'List agents visible to the authenticated team. Does not accept or complete tasks.', z.strictObject({}), true),
    tool('create_task', 'Request work from a recipient agent. A delivery failure is separate from the central task status. Timeout does not cancel the task; do not blindly resend.', z.strictObject({
      recipientAgentId: id, title: z.string().trim().min(1).max(200), description: text, taskContext: text.optional(),
    })),
    tool('update_task_status', 'Request a central task transition to IN_PROGRESS or FAILED. Use complete_task for completion. A failure reason is required for FAILED.', z.strictObject({
      taskId: id, status: z.enum(['IN_PROGRESS', 'FAILED']), reason: text.optional(), partiallyModified: z.boolean().optional(),
    }).superRefine((args, ctx) => {
      if (args.status === 'FAILED' && !args.reason) ctx.addIssue({ code: 'custom', path: ['reason'], message: 'A failure reason is required.' });
    })),
    tool('complete_task', 'Submit a task result to the central server. The server checks ownership and allowed state transitions.', z.strictObject({
      taskId: id,
      result: z.strictObject({ summary: text, changedFiles: z.array(z.string().min(1).max(1024)).max(500).optional(), verification: text.optional() }),
    })),
    tool('get_tasks', 'Read sent or received tasks without accepting or changing them.', z.strictObject({
      direction: z.enum(['sent', 'received']).default('received'), status: taskStatus.optional(),
      limit: z.number().int().min(1).max(100).default(20), cursor: z.string().min(1).max(512).optional(),
    }), true),
    tool('get_task_result', 'Read a task result without changing its state.', z.strictObject({ taskId: id }), true),
  ];
}
