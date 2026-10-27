import { serveStdio, StdioServerTransport } from '@modelcontextprotocol/server/stdio';
import type { Readable, Writable } from 'node:stream';
import type { RuntimeConfig } from './config.js';
import { createLogger, type Logger } from './logger.js';
import { createServer } from './server.js';

export interface Runtime {
  close(): Promise<void>;
  readonly closed: Promise<void>;
}

export function startRuntime(
  config: RuntimeConfig,
  options: { input?: Readable; output?: Writable; logger?: Logger } = {},
): Runtime {
  const logger = options.logger ?? createLogger(process.stderr, config.logLevel);
  // Validate construction/registration before listening to any protocol messages.
  let unclaimedServer: ReturnType<typeof createServer> | undefined = createServer(logger);
  const transport = new StdioServerTransport(options.input, options.output);
  const handle = serveStdio(() => {
    const server = unclaimedServer ?? createServer(logger);
    unclaimedServer = undefined;
    return server;
  }, {
    transport,
    legacy: 'serve',
    onerror: () => logger.error('runtime.transport_error', { code: 'MCP_TRANSPORT_ERROR' }),
  });

  let resolveClosed!: () => void;
  const closed = new Promise<void>((resolve) => { resolveClosed = resolve; });
  let closing: Promise<void> | undefined;
  const close = (): Promise<void> => {
    // Defer teardown so SDK onclose cannot recursively start a second close.
    closing ??= Promise.resolve().then(async () => {
      try {
        await handle.close();
        await unclaimedServer?.close();
        unclaimedServer = undefined;
      } finally {
        logger.info('runtime.closed');
        resolveClosed();
      }
    });
    return closing;
  };

  // Keep the SDK's teardown callback: it owns the pinned instance and transport.
  const sdkOnClose = transport.onclose;
  transport.onclose = () => {
    sdkOnClose?.();
    void close().catch(() => logger.error('runtime.shutdown_failed', { code: 'SHUTDOWN_FAILED' }));
  };
  logger.info('runtime.started');
  return { close, closed };
}
