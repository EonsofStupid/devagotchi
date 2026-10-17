import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import vm from 'node:vm';
import { stripTypeScriptTypes } from 'node:module';

// Source-slice harness for pinned commit f508e524469c26f6366e17dbfd3ee5a69d112127.
// Executes the unmodified path-validation functions (filesystem.ts 111–423).
// Configuration and telemetry imports are stubbed; filesystem operations are real.
// This is not a build or end-to-end execution of the upstream MCP server.
const source = await fs.readFile(new URL('../../reference/DesktopCommanderMCP/src/tools/filesystem.ts', import.meta.url), 'utf8');
const sourceSlice = source.split('\n').slice(110, 423).join('\n').replace(/^export /gm, '');
const js = stripTypeScriptTypes(sourceSlice);
const root = await fs.mkdtemp(path.join(process.cwd(), 'dc-security-path-fixture-'));
const allowed = path.join(root, 'allowed');
const caseSibling = path.join(root, 'ALLOWED');
const unrelated = path.join(root, 'unrelated');
const config = { allowedDirectories: [allowed] };
const context = vm.createContext({
  fs, path, os, process, console,
  configManager: { getConfig: async () => config, setValue: async (key, value) => { config[key] = value; } },
  capture: () => {}, addToolCallPaths: () => {},
  withTimeout: async (promise) => await promise,
  FILE_OPERATION_TIMEOUTS: { PATH_VALIDATION: 10000 },
});
vm.runInContext(js, context);
try {
  await fs.mkdir(allowed);
  await fs.mkdir(caseSibling);
  await fs.mkdir(unrelated);
  await fs.writeFile(path.join(allowed, 'demo.txt'), 'allowed fixture');
  await fs.writeFile(path.join(caseSibling, 'demo.txt'), 'outside fixture');
  await fs.writeFile(path.join(unrelated, 'demo.txt'), 'unrelated fixture');
  await fs.symlink(unrelated, path.join(allowed, 'outside-link'));
  const aStat = await fs.stat(allowed);
  const bStat = await fs.stat(caseSibling);
  const results = [];
  for (const [label, requestedPath, expected] of [
    ['inside allowed root', path.join(allowed, 'demo.txt'), true],
    ['unrelated sibling', path.join(unrelated, 'demo.txt'), false],
    ['symlink to unrelated sibling', path.join(allowed, 'outside-link', 'demo.txt'), false],
    ['different-case sibling', path.join(caseSibling, 'demo.txt'), false],
  ]) {
    let accepted = false;
    try { await context.validatePath(requestedPath); accepted = true; } catch {}
    results.push({ label, expected, accepted, correct: accepted === expected });
  }
  console.log(JSON.stringify({
    harness: 'Pinned validatePath source with stubbed config/telemetry/timeout wrapper; real filesystem',
    platform: process.platform,
    distinctDirectoryInodes: aStat.ino !== bStat.ino,
    results,
  }, null, 2));
} finally {
  await fs.rm(root, { recursive: true, force: true });
}
