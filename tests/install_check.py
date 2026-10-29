#!/usr/bin/env python3
"""The release gate: a fresh machine installs Devagotchi with the README's own line, from a
run's own installers and archives served on 127.0.0.1, then an AI client's `devagotchi mcp`
runs a job.

1. The README's install line for this system runs in a fresh shell (the release URL becomes
   the local server; Linux also drops curl's https-only flags, which a local server cannot
   meet). Its second half is `devagotchi setup`.
2. `setup` sets up Claude Code (a stub `claude` records the call), Codex and Claude Desktop,
   and its own check says the server answered.
3. The broker that check started is killed, as a reboot would, leaving its endpoint file.
4. The exact command Codex was given starts as an MCP server, starts the broker again on its
   own, lists the tools and runs `git --version` to the end.

It installs into this machine's real home directory, so it runs only on a disposable one
(`CI=true`, as on GitHub's runners).
"""
import argparse
import base64
import functools
import http.server
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

RELEASE = 'https://github.com/EonsofStupid/devagotchi/releases/latest/download/'
WINDOWS = os.name == 'nt'


def readme_line(readme, installer):
    """The one line in the README that runs `installer`."""
    lines = [l.strip() for l in readme.read_text(encoding='utf-8').splitlines()
             if installer in l and 'setup' in l]
    assert len(lines) == 1, f'README has {len(lines)} install lines for {installer}: {lines}'
    return lines[0]


def serve(directory):
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(directory))
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f'http://127.0.0.1:{server.server_address[1]}/'


def stub_claude(directory, log):
    """A `claude` that records what it was asked, where `devagotchi setup` looks for it."""
    if WINDOWS:
        (directory / 'claude.cmd').write_text(f'@echo %*>>"{log}"\r\n', encoding='utf-8')
    else:
        stub = directory / 'claude'
        stub.write_text(f'#!/bin/sh\necho "$@" >> "{log}"\n', encoding='utf-8')
        stub.chmod(0o755)


class Mcp:
    """A stdio MCP client that reads on a thread, so it works on Windows pipes too."""

    def __init__(self, argv, env):
        self.process = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.lines = queue.Queue()
        self.stderr = []
        threading.Thread(target=self._read, args=(self.process.stdout, self.lines.put),
                         daemon=True).start()
        threading.Thread(target=self._read, args=(self.process.stderr, self.stderr.append),
                         daemon=True).start()
        self.counter = 0

    @staticmethod
    def _read(stream, put):
        for line in iter(stream.readline, b''):
            put(line)
        put(None)

    def rpc(self, method, params, timeout=60):
        self.counter += 1
        self.send({'jsonrpc': '2.0', 'id': self.counter, 'method': method, 'params': params})
        deadline = time.monotonic() + timeout
        while True:
            try:
                line = self.lines.get(timeout=max(0.1, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{method}: no answer in {timeout} s; stderr: {self.why()}')
            assert line is not None, f'{method}: the server exited; stderr: {self.why()}'
            reply = json.loads(line)
            if reply.get('id') == self.counter:
                assert 'error' not in reply, reply
                return reply['result']

    def send(self, value):
        self.process.stdin.write(json.dumps(value).encode() + b'\n')
        self.process.stdin.flush()

    def tool(self, name, arguments):
        result = self.rpc('tools/call', {'name': name, 'arguments': arguments})
        assert not result.get('isError'), result
        return result['structuredContent']

    def why(self):
        return b''.join(l for l in self.stderr if l).decode(errors='replace')

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()


def brokers(exe):
    """Whether a broker started from `exe` (`devagotchi start …`) is running."""
    if WINDOWS:
        script = (f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.Name -eq '{exe.name}' "
                  "-and $_.CommandLine -match '\\sstart(\\s|$)' }).Count")
        encoded = base64.b64encode(script.encode('utf-16-le')).decode()
        out = subprocess.run(['powershell', '-NoProfile', '-EncodedCommand', encoded],
                             capture_output=True, text=True).stdout.strip()
        return out not in ('', '0')
    return subprocess.run(['pgrep', '-f', '--', f'{exe} start'], capture_output=True).returncode == 0


def kill_brokers(exe):
    """What a reboot does to the broker `setup` started: gone, its endpoint file left."""
    if WINDOWS:
        subprocess.run(['taskkill', '/F', '/IM', exe.name], capture_output=True)
    else:
        subprocess.run(['pkill', '-KILL', '-f', '--', f'{exe} start'], capture_output=True)
    deadline = time.monotonic() + 10
    while brokers(exe) and time.monotonic() < deadline:
        time.sleep(0.2)
    assert not brokers(exe), 'the broker is still running'


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--served', type=Path, required=True,
                        help='the installers and the archives, as a release has them')
    parser.add_argument('--readme', type=Path,
                        default=Path(__file__).resolve().parent.parent / 'README.md')
    options = parser.parse_args()
    if os.environ.get('CI') != 'true':
        sys.exit('install_check.py installs into your real home directory; it runs only where CI=true')

    home = Path.home()
    exe = home / '.local' / 'bin' / ('devagotchi.exe' if WINDOWS else 'devagotchi')
    assert not exe.exists(), f'{exe} exists already: this machine is not fresh'
    base = serve(options.served)
    work = Path(tempfile.mkdtemp(prefix='install-check-'))
    stubs = work / 'stubs'
    stubs.mkdir()
    claude_log = work / 'claude-calls.txt'
    stub_claude(stubs, claude_log)
    # Codex and Claude Desktop are found by their directories; Claude Desktop's parent is
    # also where the broker's config goes.
    (home / '.codex').mkdir(exist_ok=True)
    xdg = os.environ.get('XDG_CONFIG_HOME', '')
    desktop = Path(os.environ['APPDATA']) if WINDOWS else (
        Path(xdg) if os.path.isabs(xdg) else home / '.config')
    (desktop / 'Claude').mkdir(parents=True, exist_ok=True)

    env = {**os.environ, 'DEVAGOTCHI_DOWNLOAD_URL': base.rstrip('/'),
           'PATH': str(stubs) + os.pathsep + os.environ['PATH']}
    if WINDOWS:
        line = readme_line(options.readme, 'devagotchi-installer.ps1').replace(RELEASE, base)
        script = work / 'install.ps1'
        script.write_text(line + '\r\nexit $LASTEXITCODE\r\n', encoding='utf-8')
        argv = ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(script)]
    else:
        line = readme_line(options.readme, 'devagotchi-installer.sh')
        assert "curl --proto '=https' --tlsv1.2 " in line, line
        line = line.replace("--proto '=https' --tlsv1.2 ", '').replace(RELEASE, base)
        argv = ['bash', '-c', 'set -o pipefail; ' + line]
    print(f'$ {line}', flush=True)
    began = time.monotonic()
    done = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=600)
    print(done.stdout, done.stderr, sep='\n', flush=True)
    assert done.returncode == 0, f'the install line exited {done.returncode}'
    print(f'install line: {time.monotonic() - began:.1f} s', flush=True)

    assert exe.is_file(), f'{exe} was not installed'
    answered = re.search(r'Check: Devagotchi answered with (\d+) tools', done.stdout)
    assert answered and int(answered.group(1)) >= 5, 'setup\'s own check did not pass'
    for client in ('Claude Code', 'Codex', 'Claude Desktop'):
        assert re.search(rf'^  {client}: (?!not set up)', done.stdout, re.M), f'{client} was not set up'
    calls = claude_log.read_text(encoding='utf-8', errors='replace')
    assert f'mcp add --scope user devagotchi -- {exe} mcp' in calls, calls

    codex = tomllib.loads((home / '.codex' / 'config.toml').read_text(encoding='utf-8'))
    server = codex['mcp_servers']['devagotchi']
    assert Path(server['command']) == exe and server['args'] == ['mcp'], server
    desktop_config = json.loads((desktop / 'Claude' / 'claude_desktop_config.json').read_text(encoding='utf-8'))
    assert Path(desktop_config['mcpServers']['devagotchi']['command']) == exe, desktop_config

    assert brokers(exe), 'setup\'s check left no broker running'
    kill_brokers(exe)
    # A client starts the server with its own environment, not the installer's shell's.
    client_env = {k: v for k, v in os.environ.items() if not k.startswith('DEVAGOTCHI_')}
    began = time.monotonic()
    mcp = Mcp([server['command'], *server['args']], client_env)
    try:
        mcp.rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                               'clientInfo': {'name': 'install-check', 'version': '0'}})
        mcp.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        names = {t['name'] for t in mcp.rpc('tools/list', {})['tools']}
        assert {'operation_start', 'operation_wait', 'operation_read'} <= names, names
        # The broker's first start granted the toolchains it found on PATH, git among them.
        grants = json.loads((desktop / 'devagotchi' / 'broker.json')
                            .read_text(encoding='utf-8'))['allowed_executables']
        git = next((g for g in grants if Path(g).stem.lower() == 'git'), None)
        assert git, f'git was not granted: {grants}'
        started = mcp.tool('operation_start', {'idempotency_key': 'install-check-git',
                                               'executable': git, 'args': ['--version'],
                                               'deadline_ms': 60000})
        operation = started['operation']['operation_id']
        view = mcp.tool('operation_wait', {'operation_id': operation, 'timeout_ms': 50000})
        assert view['state'] == 'succeeded', view
        page = mcp.tool('operation_read', {'operation_id': operation})
        text = ''.join(frame.get('text', '') for frame in page['frames'])
        assert text.startswith('git version'), page
        assert brokers(exe), 'no broker ran the job'
        print(f'after a reboot, an AI client\'s first job: {text.strip()!r} in '
              f'{time.monotonic() - began:.1f} s, its broker started on demand', flush=True)
    finally:
        mcp.close()
        kill_brokers(exe)
    print('install check: passed', flush=True)


if __name__ == '__main__':
    main()
