#!/usr/bin/env python3
"""Real broker/stdio-process acceptance checks. Uses only disposable owned fixtures."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time
import uuid


class McpClient:
    def __init__(self, adapter, socket):
        self.process = subprocess.Popen([str(adapter), 'mcp', '--socket', str(socket)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        self.buffer = b''
        self.counter = 0
        result = self.rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {},
            'clientInfo': {'name': 'devagotchi-acceptance', 'version': '0.1.0'}})
        assert result['protocolVersion'] == '2025-11-25', result
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})

    def send(self, value):
        self.process.stdin.write(json.dumps(value, ensure_ascii=False).encode() + b'\n')
        self.process.stdin.flush()

    def line(self, timeout=8):
        deadline = time.monotonic() + timeout
        while b'\n' not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.process.stdout], [], [], remaining)[0]:
                raise TimeoutError('MCP response timed out')
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError('Adapter exited: ' + self.process.stderr.read(8192).decode(errors='replace'))
            self.buffer += chunk
            if len(self.buffer) > 2 * 1024 * 1024:
                raise AssertionError('MCP response exceeded test safety bound')
        line, self.buffer = self.buffer.split(b'\n', 1)
        return json.loads(line)

    def rpc(self, method, params):
        self.counter += 1
        request_id = self.counter
        self.send({'jsonrpc': '2.0', 'id': request_id, 'method': method, 'params': params})
        for _ in range(32):
            reply = self.line()
            if reply.get('id') == request_id:
                assert 'error' not in reply, reply
                return reply['result']
        raise AssertionError('Too many unrelated MCP messages')

    def tool(self, name, arguments, *, error=False):
        result = self.rpc('tools/call', {'name': name, 'arguments': arguments})
        assert bool(result.get('isError', False)) == error, result
        assert isinstance(result.get('structuredContent'), dict), result
        return result['structuredContent']

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)
        self.process.stdout.close()
        self.process.stderr.close()


def wait_until(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.025)
    raise TimeoutError('Fixture condition was not observed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    project = Path(__file__).resolve().parents[1]
    parser.add_argument('--devagotchi', type=Path, default=project / 'target/debug/devagotchi', help='the one binary: `devagotchi broker run` and `devagotchi mcp`')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    results = []
    clients = []
    broker = None
    broker_log = None
    started = time.monotonic()

    def passed(name, **evidence):
        results.append({'name': name, 'status': 'passed', **evidence})

    with tempfile.TemporaryDirectory(prefix='devagotchi-mcp-') as temporary:
        root = Path(temporary)
        workspace = root / 'workspace'
        workspace.mkdir(mode=0o700)
        state = root / 'state'
        socket = state / 'broker.sock'
        log_path = root / 'broker.stderr'
        config = root / 'config.json'
        executable = str(Path(sys.executable).resolve())
        config.write_text(json.dumps({'workspace': str(workspace), 'allowed_executables': [executable],
            'output_limit_bytes': 8192, 'max_operations': 32, 'max_concurrent': 4}))

        def launch_broker():
            nonlocal broker, broker_log
            offset = log_path.stat().st_size if log_path.exists() else 0
            broker_log = log_path.open('ab', buffering=0)
            broker = subprocess.Popen([str(args.devagotchi.resolve()), 'broker', 'run', '--state-dir', str(state), '--config', str(config)],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=broker_log)
            def ready():
                if broker.poll() is not None:
                    raise RuntimeError(log_path.read_text())
                return 'devagotchi broker ready:' in log_path.read_text()[offset:]
            wait_until(ready)

        def stop_broker(force=False):
            nonlocal broker, broker_log
            if broker and broker.poll() is None:
                broker.kill() if force else broker.terminate()
                broker.wait(timeout=10)
            if broker_log:
                broker_log.close()
            broker = None
            broker_log = None

        def client():
            result = McpClient(args.devagotchi.resolve(), socket)
            clients.append(result)
            return result

        def start(c, code, deadline=5000, key=None):
            body = {'idempotency_key': key or str(uuid.uuid4()), 'executable': executable,
                'args': ['-c', code], 'cwd': str(workspace), 'deadline_ms': deadline}
            return c.tool('operation_start', body), body

        def settled(c, operation_id):
            # One held request; the broker answers when the operation settles.
            value = c.tool('operation_wait', {'operation_id': operation_id, 'timeout_ms': 8000})
            if value['state'] in ['accepted', 'running']:
                raise TimeoutError(f'operation did not settle within the wait: {value}')
            return value

        def all_output(c, operation_id):
            frames = []
            cursor = 0
            gap = False
            for _ in range(1024):
                page = c.tool('operation_read', {'operation_id': operation_id, 'cursor': cursor, 'max_bytes': 16384})
                frames.extend(page['frames'])
                gap |= page['gap']
                assert page['next_cursor'] >= cursor, page
                cursor = page['next_cursor']
                if not page['has_more']:
                    return frames, gap, page
            raise AssertionError('Output pagination did not terminate')

        try:
            launch_broker()
            c = client()
            catalog = c.rpc('tools/list', {})['tools']
            assert {t['name'] for t in catalog} == {'operation_start', 'operation_get', 'operation_read', 'operation_cancel', 'operation_wait',
                                                  'operation_list', 'run', 'read_file', 'list_directory', 'write_file', 'edit_file',
                                                  'read_multiple_files', 'get_file_info', 'create_directory', 'move_file'}
            assert all(t.get('outputSchema') for t in catalog), catalog
            passed('stdio_initialize_and_typed_tool_catalog', protocol='2025-11-25', tools=len(catalog))

            expected = 'Start: € 🍀\n'.encode()
            code = 'import os,time\n' + f'data={expected!r}\n' + 'for b in data:\n os.write(1,bytes([b]));time.sleep(0.01)\nos.write(2,b"stderr-ok\\n")'
            admitted, _ = start(c, code)
            operation_id = admitted['operation']['operation_id']
            end = settled(c, operation_id)
            assert end['state'] == 'succeeded' and end['output_complete'], end
            first, gap, _ = all_output(c, operation_id)
            second, gap2, _ = all_output(c, operation_id)
            assert first == second and not gap and not gap2
            raw = b''.join(base64.b64decode(f['bytes_base64']) for f in first if f['stream'] == 'stdout')
            text = ''.join(f['text'] for f in first if f['stream'] == 'stdout')
            assert raw == expected and text == expected.decode(), (raw, text)
            assert ''.join(f['text'] for f in first if f['stream'] == 'stderr') == 'stderr-ok\n'
            assert end['executable'] == executable and end['args'][0] == '-c' and end['cwd'] == str(workspace)
            passed('split_utf8_lossless_bytes_and_independent_readers', sha256=hashlib.sha256(raw).hexdigest(), frames=len(first))

            marker = workspace / 'once.txt'
            code = f'import pathlib,time\np=pathlib.Path({str(marker)!r})\nf=p.open("a");f.write("once\\n");f.close()\ntime.sleep(0.2)'
            admitted, request = start(c, code)
            duplicate = c.tool('operation_start', request)
            assert admitted['operation']['operation_id'] == duplicate['operation']['operation_id'] and duplicate['replayed']
            settled(c, admitted['operation']['operation_id'])
            assert marker.read_text() == 'once\n'
            conflict = c.tool('operation_start', {**request, 'args': ['-c', 'pass']}, error=True)
            assert conflict['code'] == 'idempotency_conflict', conflict
            passed('idempotent_retry_and_conflicting_request_rejection')

            marker = workspace / 'reconnect.txt'
            code = f'import pathlib,time\np=pathlib.Path({str(marker)!r})\np.write_text("started")\ntime.sleep(1)\np.write_text("finished")'
            admitted, _ = start(c, code)
            operation_id = admitted['operation']['operation_id']
            wait_until(marker.exists)
            c.close()
            c = client()
            end = settled(c, operation_id)
            assert end['state'] == 'succeeded' and marker.read_text() == 'finished', end
            passed('job_survives_stdio_client_disconnect', same_operation_id=True)

            pidfile = workspace / 'pids.json'
            code = ('import subprocess,sys,time,os,json,pathlib,signal\n'
                'child=subprocess.Popen([sys.executable,"-c","import time;time.sleep(20)"])\n'
                'def stop(*_):\n child.wait(timeout=2);sys.exit(0)\n'
                'signal.signal(signal.SIGTERM,stop)\n'
                f'pathlib.Path({str(pidfile)!r}).write_text(json.dumps([os.getpid(),child.pid]))\n'
                'time.sleep(20)')
            admitted, _ = start(c, code, deadline=25000)
            operation_id = admitted['operation']['operation_id']
            wait_until(pidfile.exists)
            pids = json.loads(pidfile.read_text())
            c.tool('operation_cancel', {'operation_id': operation_id})
            end = settled(c, operation_id)
            assert end['state'] == 'cancelled' and end['cancellation_requested'], end
            wait_until(lambda: all(not Path(f'/proc/{pid}').exists() for pid in pids))
            passed('explicit_cancellation_stops_root_and_inherited_child', observed_processes=2)

            admitted, _ = start(c, 'import time;time.sleep(20)', deadline=150)
            end = settled(c, admitted['operation']['operation_id'])
            assert end['state'] == 'timed_out', end
            passed('execution_deadline_is_enforced')

            admitted, _ = start(c, 'import os;os.write(1,b"x"*65536)')
            operation_id = admitted['operation']['operation_id']
            end = settled(c, operation_id)
            frames, gap, page = all_output(c, operation_id)
            retained = b''.join(base64.b64decode(f['bytes_base64']) for f in frames)
            assert end['state'] == 'succeeded' and gap and len(retained) <= 8192, (end, gap, len(retained))
            assert retained and all(byte == ord('x') for byte in retained)
            passed('bounded_retention_reports_output_gap_without_blocking', retained_raw_bytes=len(retained), limit_bytes=8192)

            marker = workspace / 'crash-once.txt'
            code = f'import pathlib,time\nf=pathlib.Path({str(marker)!r}).open("a");f.write("once\\n");f.close()\ntime.sleep(0.5)'
            admitted, request = start(c, code)
            operation_id = admitted['operation']['operation_id']
            wait_until(marker.exists)
            stop_broker(force=True)
            time.sleep(0.65)  # The owned fixture ends by itself; no recovered PID is signalled.
            launch_broker()
            recovered = c.tool('operation_get', {'operation_id': operation_id})
            assert recovered['state'] == 'unknown' and not recovered['output_complete'], recovered
            retry = c.tool('operation_start', request)
            assert retry['replayed'] and retry['operation']['operation_id'] == operation_id, retry
            assert retry['operation']['state'] == 'unknown' and marker.read_text() == 'once\n'
            passed('broker_crash_reports_unknown_without_reexecution')

            result = {'status': 'passed', 'product': 'Devagotchi', 'environment': sys.platform,
                'scope': 'Real Linux broker plus scripted MCP stdio client; not a named desktop-client certification',
                'elapsed_seconds': round(time.monotonic() - started, 3), 'checks': results}
        except Exception as error:
            result = {'status': 'failed', 'error': str(error), 'checks': results,
                'broker_stderr': log_path.read_text()[-6000:] if log_path.exists() else ''}
            raise
        finally:
            for c in clients:
                c.close()
            stop_broker()
            if 'result' in locals():
                if args.output:
                    args.output.parent.mkdir(parents=True, exist_ok=True)
                    args.output.write_text(json.dumps(result, indent=2) + '\n')
                print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
