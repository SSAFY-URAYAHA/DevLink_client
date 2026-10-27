import assert from 'node:assert/strict';
import test from 'node:test';
import { ConfigError, loadConfig } from '../dist/config.js';

test('requires an explicit supported transport and reports only field names', () => {
  for (const env of [{}, { DEVLINK_MCP_TRANSPORT: 'http-secret' }]) {
    assert.throws(() => loadConfig([], env), (error) => {
      assert.ok(error instanceof ConfigError);
      assert.deepEqual(error.fields, ['transport']);
      assert.ok(!error.message.includes('secret'));
      return true;
    });
  }
});

test('arguments override environment settings and configuration is immutable', () => {
  const config = loadConfig(['--transport', 'stdio', '--log-level', 'error'], {
    DEVLINK_MCP_TRANSPORT: 'invalid', DEVLINK_MCP_LOG_LEVEL: 'debug',
  });
  assert.deepEqual(config, { transport: 'stdio', logLevel: 'error' });
  assert.ok(Object.isFrozen(config));
  assert.deepEqual(loadConfig([], { DEVLINK_MCP_TRANSPORT: 'stdio' }), {
    transport: 'stdio', logLevel: 'info',
  });
});

test('invalid log levels and unknown or incomplete arguments fail before startup', () => {
  assert.throws(() => loadConfig([], {
    DEVLINK_MCP_TRANSPORT: 'stdio', DEVLINK_MCP_LOG_LEVEL: 'token-secret',
  }), (error) => error instanceof ConfigError && error.fields[0] === 'logLevel');
  for (const args of [['--token-secret'], ['--transport'], ['unexpected-secret']]) {
    assert.throws(() => loadConfig(args, {}), (error) => {
      assert.deepEqual(error.fields, ['arguments']);
      assert.ok(!error.message.includes('secret'));
      return true;
    });
  }
});

test('broker credentials are represented only by an optional file path and validated lazily', () => {
  const config = loadConfig([], { DEVLINK_MCP_TRANSPORT: 'stdio', DEVLINK_BROKER_FILE: 'not-yet-published.json', DEVLINK_LOCAL_TOKEN: 'ignored-secret' });
  assert.equal(config.brokerSettingsPath, 'not-yet-published.json');
  assert.ok(!JSON.stringify(config).includes('ignored-secret'));
  assert.equal(loadConfig([], { DEVLINK_MCP_TRANSPORT: 'stdio', DEVLINK_BROKER_FILE: '' }).brokerSettingsPath, '');
});
