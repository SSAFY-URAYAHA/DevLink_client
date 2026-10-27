import type { Writable } from 'node:stream';
import type { RuntimeConfig } from './config.js';

type Level = RuntimeConfig['logLevel'];
type Event =
  | 'runtime.started'
  | 'runtime.closed'
  | 'runtime.start_failed'
  | 'runtime.transport_error'
  | 'runtime.shutdown_failed'
  | 'tool.ping';

type Diagnostic = { code?: string; fields?: readonly string[] };

export interface Logger {
  debug(event: Event): void;
  info(event: Event): void;
  error(event: Event, diagnostic?: Diagnostic): void;
}

/** Accept known events and field names rather than arbitrary error messages or payloads. */
export function createLogger(
  stream: Pick<Writable, 'write'> = process.stderr,
  level: Level = 'info',
): Logger {
  const priority = { debug: 0, info: 1, error: 2 };
  const write = (entryLevel: Level, event: Event, diagnostic: Diagnostic = {}) => {
    if (priority[entryLevel] < priority[level]) return;
    stream.write(`${JSON.stringify({
      time: new Date().toISOString(), level: entryLevel, event, ...diagnostic,
    })}\n`);
  };
  return {
    debug: (event) => write('debug', event),
    info: (event) => write('info', event),
    error: (event, diagnostic) => write('error', event, diagnostic),
  };
}
