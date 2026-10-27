import { ConfigError, loadConfig } from './config.js';
import { createLogger } from './logger.js';
import { startRuntime } from './runtime.js';

const startupLogger = createLogger();
try {
  const config = loadConfig();
  const logger = createLogger(process.stderr, config.logLevel);
  const runtime = startRuntime(config, { logger });
  const onSignal = () => {
    void runtime.close().catch(() => {
      process.exitCode = 1;
      logger.error('runtime.shutdown_failed', { code: 'SHUTDOWN_FAILED' });
    });
  };
  process.once('SIGINT', onSignal);
  process.once('SIGTERM', onSignal);
  void runtime.closed.then(() => {
    process.off('SIGINT', onSignal);
    process.off('SIGTERM', onSignal);
  });
} catch (error) {
  process.exitCode = 1;
  startupLogger.error('runtime.start_failed', error instanceof ConfigError
    ? { code: error.code, fields: error.fields }
    : { code: 'STARTUP_FAILED' });
}
