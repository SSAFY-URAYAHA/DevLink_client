import { z } from 'zod';
import type { Logger } from '../logger.js';
import { defineTool, type RegisteredDefinition } from './registry.js';

export function pingTool(logger: Logger, version: string): RegisteredDefinition {
  return defineTool({
    name: 'devlink_ping',
    description: 'Check the local DevLink MCP runtime. Does not connect to a broker or change files.',
    inputSchema: z.strictObject({}),
    outputSchema: z.object({ status: z.literal('ok'), version: z.string() }),
    annotations: {
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
    handler: async () => {
      logger.debug('tool.ping');
      return { status: 'ok' as const, version };
    },
  });
}
