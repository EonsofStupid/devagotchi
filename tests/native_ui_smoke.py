#!/usr/bin/env python3
"""Linux native Tauri -> real broker smoke test, using only disposable fixtures.

Requires a production-assets native debug build (custom-protocol), tauri-driver
2.1.0, WebKitWebDriver, xvfb-run, xauth, and dbus-run-session. No app test hooks,
Vite server, mocked invoke calls, Selenium dependency, or user desktop session.
See https://v2.tauri.app/develop/tests/webdriver/manual-setup/ .

The harness speaks W3C WebDriver over loopback using Python's standard library.
Test logs, screenshots and a machine-readable report stay in --artifacts-dir.
The --disable-webkit-sandbox-for-container flag is an explicit test-environment
workaround only; a run using it does not validate WebKit sandbox behavior.
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid

from mcp_smoke import McpClient, wait_until


ELEMENT_KEY = 'element-6066-11e4-a52e-4f735466cecf'


class WebDriverError(RuntimeError):
    def __init__(self, value):
        self.code = value.get('error', 'webdriver_error')
        super().__init__(f"{self.code}: {value.get('message', value)}")


class NativeDriver:
    def __init__(self, port):
        self.base = f'http://127.0.0.1:{port}'
        self.session_id = None

    def request(self, method, path, body=None, timeout=12):
        data = None if body is None else json.dumps(body).encode()
        request = Request(self.base + path, data=data, method=method,
                          headers={'Content-Type': 'application/json'})
        try:
            with urlopen(request, timeout=timeout) as response:
                value = json.load(response).get('value')
        except HTTPError as error:
            value = json.load(error).get('value', {})
            raise WebDriverError(value) from error
        if isinstance(value, dict) and 'error' in value:
            raise WebDriverError(value)
        return value

    def open(self, application, broker_socket):
        result = self.request('POST', '/session', {
            'capabilities': {'alwaysMatch': {
                'browserName': 'wry',
                'tauri:options': {'application': str(application),
                                  'args': ['--socket', str(broker_socket)]},
            }}}, timeout=45)
        self.session_id = result['sessionId']
        self.request('POST', self.route('/timeouts'),
                     {'implicit': 0, 'pageLoad': 15000, 'script': 5000})
        return result['capabilities']

    def route(self, suffix):
        assert self.session_id, 'No native WebDriver session'
        return '/session/' + self.session_id + suffix

    def execute(self, script, *args):
        # Read-only DOM inspection. App actions below use real WebDriver clicks.
        return self.request('POST', self.route('/execute/sync'),
                            {'script': script, 'args': list(args)})

    def element(self, selector):
        return self.request('POST', self.route('/element'),
                            {'using': 'css selector', 'value': selector})[ELEMENT_KEY]

    def click(self, selector):
        element = self.element(selector)
        self.request('POST', self.route('/element/' + element + '/click'), {})

    def rapid_clicks(self, selectors):
        # Capture real control positions, then dispatch native pointer events in
        # one WebDriver action sequence, without round trips between clicks.
        points = self.execute('return arguments[0].map(selector => { '
            'const e = document.querySelector(selector); if (!e) throw new Error(selector); '
            'const r = e.getBoundingClientRect(); '
            'return {x: Math.floor(r.left + r.width / 2), y: Math.floor(r.top + r.height / 2)}; '
            '});', selectors)
        actions = []
        for point in points:
            actions.extend([
                {'type': 'pointerMove', 'duration': 0, 'origin': 'viewport', **point},
                {'type': 'pointerDown', 'button': 0},
                {'type': 'pointerUp', 'button': 0},
            ])
        self.request('POST', self.route('/actions'), {'actions': [{
            'type': 'pointer', 'id': 'native-selection-mouse',
            'parameters': {'pointerType': 'mouse'}, 'actions': actions,
        }]})

    def text(self, selector):
        return self.execute('return document.querySelector(arguments[0])?.textContent ?? null;', selector)

    def exists(self, selector):
        return self.execute('return document.querySelector(arguments[0]) !== null;', selector)

    def enabled(self, selector):
        return self.execute('const e = document.querySelector(arguments[0]); return !!e && !e.disabled;', selector)

    def capture(self, directory, name):
        # WebKit can report an updated DOM before GTK has painted that frame.
        self.request('POST', self.route('/execute/async'), {
            'script': 'const done = arguments[arguments.length - 1]; '
                      'requestAnimationFrame(() => requestAnimationFrame(() => done(null)));',
            'args': [],
        })
        time.sleep(0.2)
        (directory / (name + '.html')).write_text(
            self.request('GET', self.route('/source')), encoding='utf-8')
        (directory / (name + '.png')).write_bytes(base64.b64decode(
            self.request('GET', self.route('/screenshot')), validate=True))

    def close(self):
        if self.session_id:
            try:
                self.request('DELETE', self.route(''), {}, timeout=8)
            finally:
                self.session_id = None


def free_ports():
    # Reserve both together so the intermediary and native port are distinct.
    with socket.socket() as first, socket.socket() as second:
        first.bind(('127.0.0.1', 0))
        second.bind(('127.0.0.1', 0))
        return first.getsockname()[1], second.getsockname()[1]


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def stop_owned_process(process, timeout=10, group=False):
    if process is None or process.poll() is not None:
        return
    # Group signalling is used only while our own unreaped group leader exists.
    if group:
        os.killpg(process.pid, signal.SIGTERM)
    else:
        process.terminate()
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        if group:
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
        process.wait(timeout=3)


def main():
    project = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path,
                        default=project / 'apps/desktop/src-tauri/target/debug/devagotchi')
    parser.add_argument('--devagotchi', type=Path, default=project / 'target/debug/devagotchi', help='the one binary: `devagotchi broker run` and `devagotchi mcp`')
    parser.add_argument('--tauri-driver', default='tauri-driver')
    parser.add_argument('--frontend-dist', type=Path, default=project / 'apps/desktop/dist')
    parser.add_argument('--artifacts-dir', type=Path, required=True)
    parser.add_argument('--disable-webkit-sandbox-for-container', action='store_true')
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('This harness exercises the Linux provider and WebKitGTK only.')
    for name in ('app', 'broker', 'adapter'):
        value = getattr(args, name).resolve(strict=True)
        if not value.is_file() or not os.access(value, os.X_OK):
            parser.error(f'{name} is not an executable file: {value}')
        setattr(args, name, value)
    required = {name: shutil.which(name) for name in
                ('xvfb-run', 'xauth', 'dbus-run-session', 'WebKitWebDriver')}
    required['tauri-driver'] = shutil.which(args.tauri_driver)
    missing = [name for name, value in required.items() if not value]
    if missing:
        parser.error('Missing native test tools: ' + ', '.join(missing))

    artifacts = args.artifacts_dir.resolve()
    artifacts.mkdir(parents=True, exist_ok=True, mode=0o700)
    index = (args.frontend_dist / 'index.html').read_text(encoding='utf-8')
    expected_scripts = re.findall(r'<script\b[^>]*\bsrc="([^"]+)"', index)
    assert expected_scripts, 'Production frontend has no compiled script asset'
    expected_scripts = [Path(value).name for value in expected_scripts]
    started = time.monotonic()
    checks = []
    report = {
        'status': 'running', 'platform': sys.platform,
        'python_version': sys.version.split()[0],
        'native_binary': str(args.app),
        'native_binary_sha256': sha256(args.app),
        'devagotchi_binary_sha256': sha256(args.devagotchi),
        'tauri_driver_sha256': sha256(Path(required['tauri-driver'])),
        'expected_script_assets': expected_scripts,
        'webkit_sandbox_disabled_for_container': args.disable_webkit_sandbox_for_container,
        'checks': checks,
    }
    if shutil.which('dpkg-query'):
        versions = subprocess.run(['dpkg-query', '-W', '-f=${Package}=${Version}\n',
                                   'webkit2gtk-driver', 'libwebkit2gtk-4.1-0', 'xvfb', 'xauth'],
                                  capture_output=True, text=True, check=True)
        report['system_test_packages'] = versions.stdout.splitlines()

    def passed(name, **evidence):
        checks.append({'name': name, 'status': 'passed', **evidence})
        print('PASS ' + name, flush=True)

    broker = driver_process = client = driver = None
    with tempfile.TemporaryDirectory(prefix='devagotchi-native-') as temporary:
        root = Path(temporary)
        workspace = root / 'workspace'
        workspace.mkdir(mode=0o700)
        state = root / 'state'
        broker_socket = state / 'broker.sock'
        executable = str(Path(sys.executable).resolve())
        config = root / 'config.json'
        config.write_text(json.dumps({
            'workspace': str(workspace), 'allowed_executables': [executable],
            'output_limit_bytes': 8192, 'max_operations': 16, 'max_concurrent': 2,
        }))
        broker_log_path = artifacts / 'broker.stderr.log'
        driver_log_path = artifacts / 'native-driver.log'
        with broker_log_path.open('wb') as broker_log, driver_log_path.open('wb') as driver_log:
            try:
                broker = subprocess.Popen([
                    str(args.devagotchi), 'broker', 'run', '--state-dir', str(state), '--config', str(config)],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=broker_log)

                def broker_ready():
                    if broker.poll() is not None:
                        raise RuntimeError(broker_log_path.read_text(errors='replace'))
                    return 'devagotchi broker ready:' in broker_log_path.read_text(errors='replace')

                wait_until(broker_ready)
                client = McpClient(args.devagotchi, broker_socket)
                history_marker = 'native-history-' + uuid.uuid4().hex[:12]
                historic = client.tool('operation_start', {
                    'idempotency_key': str(uuid.uuid4()), 'executable': executable,
                    'args': ['-c', f'print({history_marker!r})'],
                    'cwd': str(workspace), 'deadline_ms': 5000,
                })['operation']['operation_id']
                wait_until(lambda: client.tool('operation_get', {'operation_id': historic})['state'] == 'succeeded')

                label = 'native live fixture ' + uuid.uuid4().hex[:12]
                heartbeat = workspace / 'heartbeat.bin'
                fixture = workspace / 'native_fixture.py'
                fixture.write_text(
                    'import os, pathlib, signal, sys, time\n'
                    'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                    'heartbeat = pathlib.Path(sys.argv[1])\n'
                    'print("native-ready: " + sys.argv[2] + " € 🍀", flush=True)\n'
                    'print("native-stderr-visible", file=sys.stderr, flush=True)\n'
                    'print("<strong data-native-output=literal>plain text only</strong>", flush=True)\n'
                    'while True:\n'
                    ' with heartbeat.open("ab", buffering=0) as output: output.write(b".")\n'
                    ' print("native-heartbeat", flush=True)\n'
                    ' time.sleep(0.15)\n', encoding='utf-8')
                fixture_args = [str(fixture), str(heartbeat), label]
                operation_id = client.tool('operation_start', {
                    'idempotency_key': str(uuid.uuid4()), 'executable': executable,
                    'args': fixture_args, 'cwd': str(workspace), 'deadline_ms': 120000,
                })['operation']['operation_id']
                wait_until(lambda: heartbeat.exists() and heartbeat.stat().st_size > 1)
                report['fixture_operation_id'] = operation_id
                report['historic_operation_id'] = historic
                passed('real_mcp_commands_admitted_before_native_launch')

                environment = os.environ.copy()
                environment.pop('DEVAGOTCHI_SOCKET', None)
                for name in ('XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
                    path = root / name.lower()
                    path.mkdir(mode=0o700)
                    environment[name] = str(path)
                environment['GDK_BACKEND'] = 'x11'
                environment['LIBGL_ALWAYS_SOFTWARE'] = '1'
                if args.disable_webkit_sandbox_for_container:
                    environment['WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS'] = '1'
                else:
                    environment.pop('WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS', None)
                port, native_port = free_ports()
                driver_process = subprocess.Popen([
                    required['xvfb-run'], '-a', '-s', '-screen 0 1600x1000x24',
                    required['dbus-run-session'], '--', required['tauri-driver'],
                    '--port', str(port), '--native-port', str(native_port),
                    '--native-driver', required['WebKitWebDriver'],
                ], cwd=root, env=environment, stdin=subprocess.DEVNULL,
                    stdout=driver_log, stderr=driver_log, start_new_session=True)
                driver = NativeDriver(port)

                def driver_ready():
                    if driver_process.poll() is not None:
                        raise RuntimeError(driver_log_path.read_text(errors='replace'))
                    try:
                        return driver.request('GET', '/status', timeout=1).get('ready')
                    except (URLError, OSError):
                        return False

                wait_until(driver_ready, timeout=15)
                report['webdriver_capabilities'] = driver.open(args.app, broker_socket)
                wait_until(lambda: driver.exists('.connection-pill.online'), timeout=20)
                assets = driver.execute('return {url: location.href, title: document.title, '
                    'scripts: Array.from(document.scripts, s => s.src), '
                    'stylesheets: Array.from(document.styleSheets, s => s.href)};')
                assert assets['url'].startswith(('tauri://localhost', 'http://tauri.localhost/')), assets
                actual_scripts = [value.rsplit('/', 1)[-1] for value in assets['scripts'] if value]
                assert set(expected_scripts) <= set(actual_scripts), assets
                assert any('/assets/' in (href or '') for href in assets['stylesheets']), assets
                assert 'Devagotchi' in assets['title'], assets
                passed('compiled_assets_in_real_native_webkit', **assets)

                row = f'button.command-row[data-operation-id="{operation_id}"]'
                historic_row = f'button.command-row[data-operation-id="{historic}"]'
                wait_until(lambda: driver.exists(row) and driver.exists(historic_row))
                driver.click(row)
                wait_until(lambda: driver.text('.operation-id-row code') == operation_id)
                actual_args = json.loads(driver.text('.invocation-panel pre'))
                assert actual_args == fixture_args, actual_args
                actual_paths = driver.execute('return Array.from(document.querySelectorAll('
                    '".invocation-panel dd code"), e => e.textContent);')
                assert actual_paths == [executable, str(workspace)], actual_paths
                passed('native_command_list_and_exact_request_match_broker', operation_id=operation_id)

                expected = ['native-ready: ' + label + ' € 🍀', 'native-stderr-visible',
                            '<strong data-native-output=literal>plain text only</strong>']
                wait_until(lambda: all(value in (driver.text('.console-output') or '') for value in expected))
                assert not driver.exists('[data-native-output]'), 'Process output was interpreted as HTML'
                assert driver.exists('.console-output .stderr'), 'stderr stream styling was not observed'
                driver.capture(artifacts, 'native-running')
                passed('real_streamed_unicode_and_stderr_render_as_plain_text')

                # Regression: changing selection must never leave the previous
                # operation as the target of the newly selected row's controls.
                heartbeat_b = workspace / 'heartbeat-b.bin'
                operation_b = client.tool('operation_start', {
                    'idempotency_key': str(uuid.uuid4()), 'executable': executable,
                    'args': [str(fixture), str(heartbeat_b), 'selection fixture B'],
                    'cwd': str(workspace), 'deadline_ms': 120000,
                })['operation']['operation_id']
                report['selection_fixture_operation_id'] = operation_b
                row_b = f'button.command-row[data-operation-id="{operation_b}"]'
                wait_until(lambda: heartbeat_b.exists() and heartbeat_b.stat().st_size > 1)
                wait_until(lambda: driver.exists(row_b), timeout=8)
                driver.click(row)
                wait_until(lambda: driver.text('.operation-id-row code') == operation_id
                           and driver.enabled('button.cancel-button'))
                driver.rapid_clicks([row, row_b, row, row_b, 'button.cancel-button'])
                assert driver.exists(row_b + '[aria-pressed="true"]'), 'Rapid switch did not select fixture B'

                def assert_a_active():
                    value = client.tool('operation_get', {'operation_id': operation_id})
                    assert value['state'] == 'running' and not value['cancellation_requested'], value
                    return value

                assert_a_active()
                wait_until(lambda: driver.text('.operation-id-row code') == operation_b)
                initial_b = client.tool('operation_get', {'operation_id': operation_b})
                # A guarded placeholder may correctly ignore the immediate
                # pointer click until matching B details have arrived.
                if not initial_b['cancellation_requested']:
                    assert initial_b['state'] == 'running', initial_b
                    wait_until(lambda: driver.enabled('button.cancel-button'))
                    driver.click('button.cancel-button')

                def b_cancelled():
                    value = client.tool('operation_get', {'operation_id': operation_b})
                    if value['state'] in ('accepted', 'running'):
                        return None
                    assert value['state'] == 'cancelled' and value['cancellation_requested'], value
                    return value

                stopped_b = wait_until(b_cancelled, timeout=10)
                a_size = heartbeat.stat().st_size
                b_size = heartbeat_b.stat().st_size
                time.sleep(0.65)
                assert heartbeat_b.stat().st_size == b_size, 'Selected fixture B kept writing after cancellation'
                assert heartbeat.stat().st_size > a_size, 'Unselected fixture A stopped advancing'
                assert_a_active()
                wait_until(lambda: driver.exists('.detail-title-row .state-cancelled'))
                driver.capture(artifacts, 'native-selected-cancelled')
                passed('rapid_selection_cancels_only_selected_operation',
                       selected_operation_id=operation_b, selected_state=stopped_b['state'],
                       other_operation_id=operation_id, other_state='running',
                       other_heartbeat_advanced=True, rapid_pointer_clicks=5,
                       immediate_click_requested_cancellation=initial_b['cancellation_requested'])

                driver.click(row)
                wait_until(lambda: driver.text('.operation-id-row code') == operation_id)
                wait_until(lambda: driver.enabled('button.cancel-button'))
                driver.click('button.cancel-button')

                def cancelled():
                    value = client.tool('operation_get', {'operation_id': operation_id})
                    if value['state'] in ('accepted', 'running'):
                        return None
                    assert value['state'] == 'cancelled', value
                    assert value['cancellation_requested'], value
                    return value

                settled = wait_until(cancelled, timeout=10)
                heartbeat_size = heartbeat.stat().st_size
                time.sleep(0.65)
                assert heartbeat.stat().st_size == heartbeat_size, 'Fixture kept writing after UI cancellation'
                wait_until(lambda: driver.exists('.detail-title-row .state-cancelled'))
                assert not driver.exists('button.cancel-button'), 'Completed command still offered cancellation'
                driver.capture(artifacts, 'native-cancelled')
                passed('native_cancel_reaches_broker_and_stops_fixture',
                       state=settled['state'], cancellation_requested=settled['cancellation_requested'],
                       output_complete=settled['output_complete'], heartbeat_bytes=heartbeat_size)

                stop_owned_process(broker)
                expected_error = client.tool('operation_get', {'operation_id': operation_id}, error=True)
                wait_until(lambda: driver.exists('.connection-pill.offline'), timeout=10)
                assert driver.exists('.connection-banner[role="status"]'), 'No visible stale-connection notice'
                assert driver.exists(row), 'Last received history unexpectedly disappeared'
                driver.click('nav .nav-item:nth-child(3)')
                wait_until(lambda: driver.exists('.connection-view .error-note[role="alert"]'))
                connection_error = driver.text('.connection-view .error-note')
                assert expected_error['code'].replace('_', ' ') in connection_error, connection_error
                assert expected_error['message'] in connection_error, connection_error
                assert str(broker_socket) in driver.text('.connection-details'), 'Actual configured socket not visible'
                assert not driver.exists('.connection-overview.connected'), 'Unavailable broker shown as connected'
                driver.capture(artifacts, 'native-disconnected')
                passed('broker_loss_is_explicit_and_last_history_is_retained', error=connection_error)
                report['status'] = 'passed'
            except BaseException as error:
                report['status'] = 'failed'
                report['error'] = repr(error)
                report['traceback'] = traceback.format_exc()
                if driver and driver.session_id:
                    try:
                        driver.capture(artifacts, 'native-failure')
                    except Exception as capture_error:
                        report['capture_error'] = repr(capture_error)
                raise
            finally:
                cleanup_errors = []
                for name, action in (
                    ('webdriver_session', lambda: driver.close() if driver else None),
                    ('driver_process', lambda: stop_owned_process(driver_process, timeout=5, group=True)),
                    ('mcp_client', lambda: client.close() if client else None),
                    ('broker_process', lambda: stop_owned_process(broker)),
                ):
                    try:
                        action()
                    except Exception as error:
                        cleanup_errors.append({'action': name, 'error': repr(error)})
                if cleanup_errors:
                    report['cleanup_errors'] = cleanup_errors
                    report['status'] = 'failed'
                report['elapsed_seconds'] = round(time.monotonic() - started, 3)
                (artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    if report['status'] != 'passed':
        raise RuntimeError('Native smoke cleanup failed; see report.json')
    print(f"PASS {len(checks)} native checks; report: {artifacts / 'report.json'}", flush=True)


if __name__ == '__main__':
    main()
