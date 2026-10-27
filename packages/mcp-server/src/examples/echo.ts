import { z } from 'zod';
import { defineTool } from '../tools/registry.js';

/** Copy this module to add a tool; explicitly supply it to the server composition root. */
export const echoExample = defineTool({
  name: 'example_echo', description: 'Return a short message without external I/O. Development example; disabled by default.',
  inputSchema: z.strictObject({ message: z.string().min(1).max(200) }),
  outputSchema: z.strictObject({ message: z.string() }),
  annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  handler: async ({ message }) => ({ message }),
});
