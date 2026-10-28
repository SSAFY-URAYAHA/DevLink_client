import { execFile } from 'node:child_process';
import { constants } from 'node:fs';
import { lstat, open } from 'node:fs/promises';
import { dirname, isAbsolute } from 'node:path';
import { promisify } from 'node:util';
import { z } from 'zod';
import { BrokerFault } from './contracts.js';
import { credentialSchema } from './schemas.js';

export type BrokerSettings = Readonly<z.infer<typeof credentialSchema>>;

export function validateSettings(value: unknown): BrokerSettings {
  const result = credentialSchema.safeParse(value);
  if (!result.success) throw new BrokerFault('CREDENTIAL_FILE_INVALID', 'settings');
  const parsed = result.data;
  // URL normalizes forms such as 127.1 and integer hostnames; require the original literal too.
  const url = new URL(parsed.brokerUrl);
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1'
    || url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new BrokerFault('CREDENTIAL_FILE_INVALID', 'settings');
  }
  return Object.freeze({ ...parsed, brokerUrl: parsed.brokerUrl.replace(/\/$/, '') });
}

/** CLI owns writing the snapshot. Reject links, broad permissions and oversized files. */
export async function verifyPrivateFile(path: string): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size > 16_384) throw new Error('Invalid settings file');
  const parent = await lstat(dirname(path));
  if (!parent.isDirectory() || parent.isSymbolicLink()) throw new Error('Invalid parent');
  if (process.platform !== 'win32') {
    if (info.uid !== process.getuid?.() || (info.mode & 0o077) !== 0) throw new Error('Not private');
    if ((parent.mode & 0o022) !== 0) throw new Error('Writable parent');
    return;
  }
  // No path interpolation into shell code. The child receives only the file path, never its token.
  const script = `
$ErrorActionPreference = 'Stop'
$p = $env:DEVLINK_ACL_CHECK_PATH
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$trusted = @($sid, 'S-1-5-18', 'S-1-5-32-544')
foreach ($target in @($p, [System.IO.Path]::GetDirectoryName($p))) {
  $acl = if ($target -eq $p) { [System.IO.File]::GetAccessControl($target) } else { [System.IO.Directory]::GetAccessControl($target) }
  if ($target -eq $p -and $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid) { exit 2 }
  foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
    if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) {
      if ($target -eq $p -or (($rule.FileSystemRights -band 852310) -ne 0)) { exit 3 }
    }
  }
}
[Console]::Write('private')
`;
  const { stdout } = await promisify(execFile)('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    env: { ...process.env, DEVLINK_ACL_CHECK_PATH: path }, windowsHide: true, timeout: 5000, maxBuffer: 4096,
  });
  if (stdout !== 'private') throw new Error('Not private');
}

export async function readBrokerSettings(path: string): Promise<BrokerSettings> {
  try {
    if (!isAbsolute(path)) throw new Error('Absolute path required');
    await verifyPrivateFile(path);
    const before = await lstat(path);
    const file = await open(path, process.platform === 'win32' ? 'r' : constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const info = await file.stat();
      if (!info.isFile() || info.size > 16_384 || before.ino !== info.ino || before.dev !== info.dev) throw new Error('Snapshot changed');
      if (process.platform !== 'win32' && (info.uid !== process.getuid?.() || (info.mode & 0o077) !== 0)) throw new Error('Not private');
      const buffer = Buffer.alloc(16_385);
      let length = 0;
      while (length < buffer.length) {
        const { bytesRead } = await file.read(buffer, length, buffer.length - length, null);
        if (bytesRead === 0) break;
        length += bytesRead;
      }
      if (length > 16_384) throw new Error('Oversized settings');
      const after = await file.stat();
      if (info.size !== after.size || info.mtimeMs !== after.mtimeMs) throw new Error('Snapshot changed');
      return validateSettings(JSON.parse(buffer.subarray(0, length).toString('utf8')));
    } finally {
      await file.close();
    }
  } catch {
    throw new BrokerFault('CREDENTIAL_FILE_INVALID', 'settings');
  }
}
