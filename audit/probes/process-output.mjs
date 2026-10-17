import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { stripTypeScriptTypes } from 'node:module';
import { EventEmitter } from 'node:events';
import { fileURLToPath } from 'node:url';

// Source-level probes: exact pinned TerminalManager body, with module imports
// replaced by a deterministic fake child process. This is not the project test suite.
const root = fileURLToPath(new URL('../../reference/DesktopCommanderMCP/', import.meta.url));
const stripModule = (text) => stripTypeScriptTypes(text.replace(/^import .*;\s*$/gm, '')).replace(/^export /gm, '');
const detection = stripModule(fs.readFileSync(path.join(root, 'src/utils/process-detection.ts'), 'utf8'));
const terminal = stripModule(fs.readFileSync(path.join(root, 'src/terminal-manager.ts'), 'utf8'));
let latestChild;
let nextPid = 987650;
const context = vm.createContext({
  console, process, Date, setTimeout, clearTimeout, setInterval, clearInterval, setImmediate,
  DEFAULT_COMMAND_TIMEOUT: 5, MAX_PROCESS_WAIT_MS: 100,
  configManager: { getConfig: async () => ({}) },
  capture: () => {}, getDefaultShell: () => 'fake',
  getShellSpawnArgs: () => ({ executable: 'fake', args: [] }),
  terminateProcessTree: async () => true,
  spawn: () => {
    const child = new EventEmitter();
    child.pid = ++nextPid;
    child.exitCode = null;
    child.signalCode = null;
    child.stdout = new EventEmitter();
    child.stderr = new EventEmitter();
    child.stdin = { destroyed: false, write: () => false };
    latestChild = child;
    return child;
  },
});
vm.runInContext(detection + '\n' + terminal + '\nglobalThis.SourceTerminalManager = TerminalManager;', context);
const Manager = context.SourceTerminalManager;
const manager = new Manager();

const start = manager.executeCommand('fake', 10, undefined, true);
await new Promise(setImmediate);
latestChild.stdout.emit('data', Buffer.from('initial\n'));
const result = await start;
const firstEvents = result.timingInfo.outputEvents.length;
const euro = Buffer.from('€');
latestChild.stdout.emit('data', euro.subarray(0, 1));
latestChild.stdout.emit('data', euro.subarray(1));
const utf = manager.readOutputPaginated(result.pid, 0, 100);
const secondReader = manager.readOutputPaginated(result.pid, 0, 100);
const inputReported = manager.sendInputToProcess(result.pid, 'x');
const facts = {
  source: 'f508e524469c26f6366e17dbfd3ee5a69d112127/src/terminal-manager.ts',
  methodology: 'Real source body, Node 24 TS erasure, fake child process and config dependencies; not full project tests',
  splitUtf8: { expected: '€', received: utf.lines[1], corrupted: utf.lines[1] !== '€' },
  sharedReadCursor: { firstReaderLines: utf.lines, secondReaderLines: secondReader.lines },
  timingAfterWaitReturned: { eventsAtReturn: firstEvents, eventsAfterTwoLaterChunks: result.timingInfo.outputEvents.length },
  writeReturnedFalse: { reportedSent: inputReported, note: 'write(false) signals backpressure, not a send failure; source does not await drain' },
};
latestChild.exitCode = 0;
latestChild.emit('exit', 0, null);
console.log(JSON.stringify(facts, null, 2));
