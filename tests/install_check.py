#!/usr/bin/env python3
"""The release gate: a fresh machine installs Devagotchi with the README's own line, from a
run's own installers and archives served on 127.0.0.1, then an AI client's `devagotchi mcp`
runs a job.

1. The README's install line for this system (the same as llms.txt's) runs as a person or an
   AI would run it: on Linux from bash, then sh; on Windows from Command Prompt, then Windows
   PowerShell, PowerShell 7 and Git Bash where present. The release URL becomes the local
   server (Linux also drops curl's https-only flags, which a local server cannot meet); the
   installer must fetch this system's own archive from it. The line's second half is
   `devagotchi setup`.
2. `setup` sets up Claude Code (a stub `claude` records the call), Codex and Claude Desktop,
   and its own check says the server answered.
3. The broker that check started is killed, as a reboot would, leaving its endpoint file.
4. The exact command Codex was given starts as an MCP server, starts the broker again on its
   own, lists the tools and runs `git --version` to the end.

It installs into this machine's real home directory and edits its shell profiles or user
PATH, so it runs only on a GitHub-hosted runner, or on Linux with HOME pointed at a throwaway
directory that INSTALL_CHECK_HOME names too.
"""
import argparse
import base64
import functools
import http.server
import json
import os
from pathlib import Path
import platform
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

RELEASE = 'https://github.com/EonsofStupid/devagotchi/releases/latest/download/'
# curl's https-only flags, which a local server cannot meet.
LINUX_TLS = '--proto =https --tlsv1.2 '
WINDOWS = os.name == 'nt'
TRIPLES = {
    ('Linux', 'X64'): 'x86_64-unknown-linux-gnu',
    ('Linux', 'ARM64'): 'aarch64-unknown-linux-gnu',
    ('Windows', 'X64'): 'x86_64-pc-windows-msvc',
    ('Windows', 'ARM64'): 'aarch64-pc-windows-msvc',
}


def hosted():
    return (os.environ.get('GITHUB_ACTIONS') == 'true'
            and os.environ.get('RUNNER_ENVIRONMENT') == 'github-hosted')


def disposable():
    if hosted():
        return True
    # A throwaway HOME on Linux, with nothing pointing the clients' or shells' files elsewhere.
    elsewhere = [k for k in ('XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'XDG_DATA_HOME', 'CODEX_HOME',
                             'ZDOTDIR') if os.environ.get(k)]
    if elsewhere:
        sys.exit(f'install_check.py: unset {", ".join(elsewhere)} for a throwaway-home run')
    return not WINDOWS and os.environ.get('INSTALL_CHECK_HOME') == str(Path.home())


def this_triple():
    """This machine's target, from the runner's own description where there is one."""
    runner = (os.environ.get('RUNNER_OS'), os.environ.get('RUNNER_ARCH'))
    if runner in TRIPLES:
        return TRIPLES[runner]
    arch = 'ARM64' if platform.machine().lower() in ('aarch64', 'arm64') else 'X64'
    return TRIPLES[('Windows' if WINDOWS else 'Linux', arch)]


def readme_line(readme, installer):
    """The one line in the README that runs `installer`."""
    lines = [l.strip() for l in readme.read_text(encoding='utf-8').splitlines()
             if installer in l and 'setup' in l]
    assert len(lines) == 1, f'README has {len(lines)} install lines for {installer}: {lines}'
    return lines[0]


def serve(directory):
    """Serves `directory` on 127.0.0.1; returns its base URL and the paths asked for."""
    asked = []

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self):
            asked.append(self.path)
            super().do_GET()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(
        ('127.0.0.1', 0), functools.partial(Handler, directory=str(directory)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f'http://127.0.0.1:{server.server_address[1]}/', asked


def stub_claude(directory, log):
    """A `claude` that records what it was asked, where `devagotchi setup` looks for it."""
    if WINDOWS:
        (directory / 'claude.cmd').write_text(f'@echo %*>>"{log}"\r\n', encoding='utf-8')
    else:
        stub = directory / 'claude'
        stub.write_text(f'#!/bin/sh\necho "$@" >> "{log}"\n', encoding='utf-8')
        stub.chmod(0o755)


def shells(line, work):
    """(name, how to run `line` from that shell) for every shell here a person or AI uses."""
    if not WINDOWS:
        return [('bash', ['bash', '-c', line]), ('sh', ['sh', '-c', line])]
    script = work / 'line.ps1'
    script.write_text(line + '\r\nexit $LASTEXITCODE\r\n', encoding='utf-8')
    found = []
    # Git Bash first: on ARM64 Windows it installs the x64 build, and the native build the
    # shells after it install is the one the reboot below runs.
    program_files = Path(os.environ.get('ProgramFiles', r'C:\Program Files'))
    git_bash = program_files / 'Git' / 'bin' / 'bash.exe'
    if git_bash.is_file():
        sh = work / 'line.sh'
        sh.write_text(line + '\n', encoding='utf-8')
        found.append(('Git Bash', [str(git_bash), str(sh)]))
    found += [('Command Prompt', f'cmd /d /s /c "{line}"'),
              ('Windows PowerShell', ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass',
                                      '-File', str(script)])]
    if shutil.which('pwsh'):
        found.append(('PowerShell 7', ['pwsh', '-NoProfile', '-File', str(script)]))
    # Git Bash is the shell Claude Code uses on Windows: a runner without it proves less.
    assert not hosted() or len(found) == 4, f'missing a shell: {[name for name, _ in found]}'
    return found


def run(argv, env):
    return subprocess.run(argv, env=env, capture_output=True, encoding='utf-8',
                          errors='replace', timeout=600)


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


def powershell(script):
    """`script`'s output from Windows PowerShell, with its own module path (the runner's step
    shell is PowerShell 7, whose module path breaks Windows PowerShell's cmdlets)."""
    encoded = base64.b64encode(script.encode('utf-16-le')).decode()
    env = {k: v for k, v in os.environ.items() if k.upper() != 'PSMODULEPATH'}
    done = subprocess.run(['powershell', '-NoProfile', '-EncodedCommand', encoded], env=env,
                          capture_output=True, encoding='utf-8', errors='replace')
    assert done.returncode == 0, f'{script}: {done.stderr}'
    return done.stdout.strip()


def ours(exe):
    """PowerShell for the processes running this very `exe` (not another install's)."""
    path = str(exe).replace("'", "''")
    return f"Get-CimInstance Win32_Process | Where-Object {{ $_.ExecutablePath -eq '{path}' }}"


def brokers(exe):
    """Whether a broker started from `exe` (`devagotchi start …`) is running."""
    if WINDOWS:
        started = "Where-Object { $_.CommandLine -match '\\sstart(\\s|$)' }"
        count = powershell(f'@({ours(exe)} | {started}).Count')
        assert count.isdigit(), f'counting brokers printed {count!r}'
        return count != '0'
    found = subprocess.run(['pgrep', '-f', '--', f'{exe} start'], capture_output=True)
    return found.returncode == 0


def running(exe):
    """Whether any process runs `exe`'s image, cheaply. On Windows only a disposable runner
    reaches here, where every devagotchi.exe is this check's own."""
    if WINDOWS:
        listed = subprocess.run(['tasklist', '/FI', f'IMAGENAME eq {exe.name}', '/FO', 'CSV',
                                 '/NH'], capture_output=True, encoding='utf-8', errors='replace')
        assert listed.returncode == 0, listed.stderr
        return f'"{exe.name}"'.lower() in listed.stdout.lower()
    return brokers(exe)


def kill_brokers(exe):
    """What a reboot does to the broker `setup` started: gone, its endpoint file left."""
    if WINDOWS:
        # PowerShell takes tens of seconds to start on the ARM64 runner: taskkill and tasklist.
        subprocess.run(['taskkill', '/F', '/IM', exe.name], capture_output=True)
    else:
        subprocess.run(['pkill', '-KILL', '-f', '--', f'{exe} start'], capture_output=True)
    deadline = time.monotonic() + 15
    while running(exe) and time.monotonic() < deadline:
        time.sleep(0.2)
    assert not running(exe), 'the broker is still running'


def set_up(done):
    """`setup`'s own report, in the install line's output."""
    assert done.returncode == 0, f'the install line exited {done.returncode}'
    answered = re.search(r'Check: Devagotchi answered with (\d+) tools', done.stdout)
    assert answered and int(answered.group(1)) >= 5, 'setup\'s own check did not pass'
    for client in ('Claude Code', 'Codex', 'Claude Desktop'):
        assert re.search(rf'^  {client}: (?!not set up)', done.stdout, re.M), \
            f'{client} was not set up'


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--served', type=Path, required=True,
                        help='the installers and the archives, as a release has them')
    root = Path(__file__).resolve().parent.parent
    parser.add_argument('--readme', type=Path, default=root / 'README.md')
    parser.add_argument('--llms', type=Path, default=root / 'llms.txt')
    options = parser.parse_args()
    if not disposable():
        sys.exit('install_check.py installs into your real home directory: it runs only on a '
                 'GitHub-hosted runner, or on Linux with HOME and INSTALL_CHECK_HOME naming '
                 'the same throwaway directory')

    home = Path.home()
    exe = home / '.local' / 'bin' / ('devagotchi.exe' if WINDOWS else 'devagotchi')
    assert not exe.exists(), f'{exe} exists already: this machine is not fresh'
    triple = this_triple()
    base, asked = serve(options.served)
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

    published = readme_line(options.readme, 'devagotchi-installer.ps1' if WINDOWS
                            else 'devagotchi-installer.sh')
    assert f'`{published}`' in options.llms.read_text(encoding='utf-8'), \
        'llms.txt and the README give different install lines'
    assert published.count(RELEASE) == 1, f'the line no longer fetches {RELEASE}: {published}'
    line = published.replace(RELEASE, base)
    if not WINDOWS:
        assert LINUX_TLS in line, line
        line = line.replace(LINUX_TLS, '')
    assert 'github.com' not in line, line
    env = {**os.environ, 'DEVAGOTCHI_DOWNLOAD_URL': base.rstrip('/'),
           'PATH': str(stubs) + os.pathsep + os.environ['PATH']}
    print(f'install line, served locally: {line}', flush=True)

    for n, (shell, argv) in enumerate(shells(line, work)):
        asked.clear()
        began = time.monotonic()
        done = run(argv, env)
        print(f'--- from {shell}: exit {done.returncode} in {time.monotonic() - began:.1f} s',
              done.stdout, done.stderr, sep='\n', flush=True)
        set_up(done)
        archives = [p for p in asked if p.startswith('/devagotchi-') and 'installer' not in p]
        expected = [triple]
        if shell == 'Git Bash' and triple == TRIPLES[('Windows', 'ARM64')]:
            # Git for Windows is an x64 program on ARM64 Windows, so the PowerShell it starts
            # reports x64 and dist's installer takes the x64 build, which runs under emulation
            # (dist's own note; the README says so).
            expected.append(TRIPLES[('Windows', 'X64')])
        assert archives and all(any(p.startswith(f'/devagotchi-{t}.') for t in expected)
                                for p in archives), \
            f'the installer fetched {asked}, not this system\'s {triple} archive'
        print(f'    fetched {", ".join(archives)}', flush=True)
        assert exe.is_file(), f'{exe} was not installed'
        if n == 0:
            calls = claude_log.read_text(encoding='utf-8', errors='replace')
            assert f'mcp add --scope user devagotchi -- {exe} mcp' in calls, calls
            assert brokers(exe), 'setup\'s check left no broker running'
        # Windows cannot replace a running executable: stop it before the next shell
        # installs over it (and before the reboot below).
        kill_brokers(exe)

    # A download that fails (a network blip, a rate limit) must fail the line, not run the
    # setup already installed and report success.
    gone = published.replace(RELEASE, base + 'gone/')
    if not WINDOWS:
        gone = gone.replace(LINUX_TLS, '')
    for shell, argv in shells(gone, work):
        done = run(argv, env)
        print(f'--- failed download, from {shell}: exit {done.returncode}', flush=True)
        assert done.returncode != 0 and 'Check:' not in done.stdout, \
            f'from {shell}, a failed download still ran setup:\n{done.stdout}{done.stderr}'
        assert not running(exe), f'from {shell}, a failed download started a broker'

    codex = tomllib.loads((home / '.codex' / 'config.toml').read_text(encoding='utf-8'))
    server = codex['mcp_servers']['devagotchi']
    assert Path(server['command']) == exe and server['args'] == ['mcp'], server
    desktop_config = json.loads((desktop / 'Claude' / 'claude_desktop_config.json')
                                .read_text(encoding='utf-8'))
    assert Path(desktop_config['mcpServers']['devagotchi']['command']) == exe, desktop_config

    # After a reboot: nothing running, and a client starts the server with its own
    # environment, not the installer's shell's.
    client_env = {k: v for k, v in os.environ.items() if not k.startswith('DEVAGOTCHI_')}
    began = time.monotonic()
    mcp = Mcp([server['command'], *server['args']], client_env)
    try:
        mcp.rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
                               'clientInfo': {'name': 'install-check', 'version': '0'}})
        # What a client waits before its server answers: the broker starts in it.
        print(f'after a reboot, the server answered in {time.monotonic() - began:.1f} s',
              flush=True)
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
    print(f'install check: passed ({triple})', flush=True)


if __name__ == '__main__':
    main()
