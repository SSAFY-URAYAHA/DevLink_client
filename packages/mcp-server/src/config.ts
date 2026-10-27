import { parseArgs } from 'node:util';
import { z } from 'zod';

const configSchema = z.object({
  transport: z.literal('stdio'),
  logLevel: z.enum(['debug', 'info', 'error']).default('info'),
});

export type RuntimeConfig = Readonly<z.infer<typeof configSchema>>;

export class ConfigError extends Error {
  readonly code = 'CONFIG_INVALID';

  constructor(readonly fields: readonly string[]) {
    super('Invalid MCP runtime configuration');
    this.name = 'ConfigError';
  }
}

/** Only read runtime settings; never copy credentials or arbitrary environment values. */
export function loadConfig(
  args: string[] = process.argv.slice(2),
  env: NodeJS.ProcessEnv = process.env,
): RuntimeConfig {
  let values;
  try {
    ({ values } = parseArgs({
      args,
      options: {
        transport: { type: 'string' },
        'log-level': { type: 'string' },
      },
      strict: true,
      allowPositionals: false,
    }));
  } catch {
    // parseArgs errors can echo input values, so expose only a safe field label.
    throw new ConfigError(['arguments']);
  }

  const parsed = configSchema.safeParse({
    transport: values.transport ?? env.DEVLINK_MCP_TRANSPORT,
    logLevel: values['log-level'] ?? env.DEVLINK_MCP_LOG_LEVEL,
  });
  if (!parsed.success) {
    throw new ConfigError([...new Set(parsed.error.issues.map((issue) => String(issue.path[0])))]);
  }
  return Object.freeze(parsed.data);
}
