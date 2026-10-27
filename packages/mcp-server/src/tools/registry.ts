import { randomUUID } from 'node:crypto';
import type { McpServer, ToolAnnotations } from '@modelcontextprotocol/server';
import { z } from 'zod';
import { BrokerFault } from '../broker/contracts.js';

export interface ToolContext { signal: AbortSignal; mcpRequestId: string | number }
export interface RegisteredDefinition { readonly name: string; register(server: McpServer): void }

/** SDK validates the published input schema before this callback can perform any I/O. */
export function defineTool<S extends z.ZodObject>(definition: {
  name: string;
  description: string;
  inputSchema: S;
  outputSchema: z.ZodObject;
  annotations: ToolAnnotations;
  handler(args: z.output<S>, context: ToolContext): Promise<Record<string, unknown>>;
}): RegisteredDefinition {
  return {
    name: definition.name,
    register(server) {
      const publishedSchema: z.ZodObject = definition.inputSchema;
      server.registerTool(definition.name, {
        description: definition.description,
        inputSchema: publishedSchema,
        outputSchema: definition.outputSchema,
        annotations: definition.annotations,
      }, async (args, ctx) => {
        try {
          const result = await definition.handler(definition.inputSchema.parse(args), { signal: ctx.mcpReq.signal, mcpRequestId: ctx.mcpReq.id });
          const parsed = definition.outputSchema.safeParse(result);
          if (!parsed.success) throw new Error('Invalid tool output');
          return { content: [{ type: 'text' as const, text: JSON.stringify(parsed.data) }], structuredContent: parsed.data };
        } catch (error) {
          const fault = error instanceof BrokerFault;
          const result = { error: {
            code: fault ? error.code : 'TOOL_EXECUTION_FAILED',
            message: fault ? error.message : 'The tool could not produce a valid result.',
            requestId: fault ? error.requestId : randomUUID(),
            outcome: fault ? error.outcome : 'unknown',
            ...(fault && error.taskId ? { taskId: error.taskId } : {}),
          } };
          return { isError: true, content: [{ type: 'text' as const, text: JSON.stringify(result) }], structuredContent: result };
        }
      });
    },
  };
}

export function registerTools(server: McpServer, definitions: readonly RegisteredDefinition[]): void {
  const names = new Set<string>();
  for (const definition of definitions) {
    if (!/^[a-z][a-z0-9_]*$/.test(definition.name) || names.has(definition.name)) throw new Error('Invalid or duplicate tool name');
    names.add(definition.name);
  }
  for (const definition of definitions) definition.register(server);
}
