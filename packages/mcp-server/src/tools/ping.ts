import type { McpServer } from '@modelcontextprotocol/server';
import { z } from 'zod';
import type { Logger } from '../logger.js';

export function registerPing(server: McpServer, logger: Logger, version: string): void {
  server.registerTool('devlink_ping', {
    description: 'Check the local DevLink MCP runtime. Does not connect to a broker or change files.',
    inputSchema: z.strictObject({}),
    outputSchema: z.object({ status: z.literal('ok'), version: z.string() }),
    annotations: {
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  }, async () => {
    logger.debug('tool.ping');
    const result = { status: 'ok' as const, version };
    return {
      content: [{ type: 'text', text: JSON.stringify(result) }],
      structuredContent: result,
    };
  });
}
