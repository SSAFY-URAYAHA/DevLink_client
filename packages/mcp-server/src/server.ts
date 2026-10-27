import { McpServer } from '@modelcontextprotocol/server';
import type { Logger } from './logger.js';
import { registerPing } from './tools/ping.js';

export const SERVER_INFO = Object.freeze({ name: 'devlink-mcp', version: '0.1.0' });

export function createServer(logger: Logger): McpServer {
  const server = new McpServer(SERVER_INFO);
  registerPing(server, logger, SERVER_INFO.version);
  return server;
}
