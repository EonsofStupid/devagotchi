#!/usr/bin/env python3
"""Black-box checks for the Linux desktop bridge proof.

Uses only Python's standard library, real broker binaries, and disposable fixtures.
These checks validate IPC and broker grant behavior; they do not establish an OS
sandbox or authentication of individual clients sharing one operating-system UID.

Usage: python3 tests/security_boundary.py [--broker target/debug/bridge-broker]
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


BROKER = None
CAPABILITIES = {}
MAX_FRAME = 64 * 1024
MAX_RESPONSE = 256 * 1024


def receive_exact(connection, count):
    chunks = bytearray()
    while len(chunks) < count:
        chunk = connection.recv(count - len(chunks))
        if not chunk:
            return None
        chunks.extend(chunk)
    return bytes(chunks)


def receive_response(connection):
    header = receive_exact(connection, 4)
    if header is None:
        return None
    size = struct.unpack('>I', header)[0]
    if size > MAX_RESPONSE:
        raise AssertionError(f'Broker emitted oversized response: {size} bytes')
    body = receive_exact(connection, size)
    if body is None:
        return None
    return json.loads(body)


def envelope(method, params, version=1):
    return {'api_version': version, 'request': {'method': method, 'params': params}}


def unwrap(response, expected_kind):
    if not response or response.get('status') != 'ok':
        raise AssertionError(f'Expected successful {expected_kind} response, got {response!r}')
    body = response['value']
    if body.get('kind') != expected_kind:
        raise AssertionError(f'Expected {expected_kind}, got {body!r}')
    return body['value']


def preflight():
    """Refuse misleading passes when the host itself prohibits local IPC."""
    result = {'unix_socket': False, 'different_uid': False, 'different_uid_reason': ''}
    with tempfile.TemporaryDirectory(prefix='desktop-bridge-test-preflight-') as temporary:
        root = Path(temporary)
        listener = socket.socket(socket.AF_UNIX)
        client = socket.socket(socket.AF_UNIX)
        try:
            listener.bind(str(root / 'preflight.sock'))
            listener.listen(1)
            client.connect(str(root / 'preflight.sock'))
            accepted, _ = listener.accept()
            accepted.close()
            result['unix_socket'] = True
        except OSError as error:
            result['error'] = f'Local Unix sockets are blocked by the execution environment: {error}'
            return result
        finally:
            client.close()
            listener.close()
        if os.geteuid() != 0:
            result['different_uid_reason'] = 'Launching a distinct UID requires a root test runner'
            return result
        probe = root / 'uid-probe'
        probe.write_bytes(b'')
        try:
            os.chown(probe, 65534, 65534)
            os.chown(probe, os.geteuid(), os.getegid())
            uid_run = subprocess.run([sys.executable, '-c',
                'import os; os.setgroups([]); os.setgid(65534); os.setuid(65534); print(os.geteuid())'],
                capture_output=True, text=True, timeout=3)
            if uid_run.returncode != 0 or uid_run.stdout.strip() != '65534':
                result['different_uid_reason'] = 'Host does not permit a distinct UID: ' + uid_run.stderr.strip()
            else:
                result['different_uid'] = True
        except OSError as error:
            result['different_uid_reason'] = f'Host UID namespace does not permit the fixture: {error}'
    return result


class EvidenceResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.evidence = []
        self.started = {}

    def startTest(self, test):
        self.started[test.id()] = time.monotonic()
        super().startTest(test)

    def record(self, test, status, detail=None):
        row = {'test': test.id(), 'status': status,
               'duration_seconds': round(time.monotonic() - self.started.get(test.id(), time.monotonic()), 4)}
        if detail:
            row['detail'] = detail
        self.evidence.append(row)

    def addSuccess(self, test):
        self.record(test, 'passed')
        super().addSuccess(test)

    def addFailure(self, test, error):
        self.record(test, 'failed', self._exc_info_to_string(error, test))
        super().addFailure(test, error)

    def addError(self, test, error):
        self.record(test, 'error', self._exc_info_to_string(error, test))
        super().addError(test, error)

    def addSkip(self, test, reason):
        self.record(test, 'skipped', reason)
        super().addSkip(test, reason)

    def addSubTest(self, test, subtest, error):
        if error is not None:
            self.record(subtest, 'failed', self._exc_info_to_string(error, test))
        super().addSubTest(test, subtest, error)


class Fixture:
    def __init__(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='desktop-bridge-boundary-')
        self.base = Path(self.temporary.name).resolve()
        self.base.chmod(0o700)
        self.state = self.base / 'state'
        self.workspace = self.base / 'workspace'
        self.workspace.mkdir(mode=0o700)
        self.executable = str(Path(sys.executable).resolve())
        self.config = self.base / 'config.json'
        self.config.write_text(json.dumps({
            'workspace': str(self.workspace),
            'allowed_executables': [self.executable],
            'output_limit_bytes': 262144,
            'max_operations': 64,
            'max_concurrent': 4,
        }))
        self.config.chmod(0o600)
        self.process = None
        self.log = None

    @property
    def socket_path(self):
        return self.state / 'broker.sock'

    def command(self):
        return [str(BROKER), '--state-dir', str(self.state), '--config', str(self.config)]

    def start(self):
        self.log = (self.base / 'broker.log').open('w+')
        self.process = subprocess.Popen(self.command(), stdout=subprocess.DEVNULL, stderr=self.log)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.log.flush()
                self.log.seek(0)
                raise AssertionError(f'Broker exited during startup: {self.log.read()[-4000:]}')
            if self.socket_path.exists():
                probe = socket.socket(socket.AF_UNIX)
                try:
                    probe.settimeout(0.2)
                    probe.connect(str(self.socket_path))
                    return self
                except (OSError, TimeoutError):
                    pass
                finally:
                    probe.close()
            time.sleep(0.025)
        raise AssertionError('Broker did not become ready within eight seconds')

    def failed_start(self):
        result = subprocess.run(self.command(), capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            raise AssertionError(f'Broker incorrectly accepted invalid startup fixture: {result.stderr}')
        return result

    def raw(self, payload, declared_size=None):
        connection = socket.socket(socket.AF_UNIX)
        try:
            connection.settimeout(3)
            connection.connect(str(self.socket_path))
            size = len(payload) if declared_size is None else declared_size
            connection.sendall(struct.pack('>I', size) + payload)
            connection.shutdown(socket.SHUT_WR)
            try:
                return receive_response(connection)
            except (BrokenPipeError, ConnectionResetError):
                return None
        finally:
            connection.close()

    def call(self, method, params, version=1):
        return self.raw(json.dumps(envelope(method, params, version)).encode())

    def start_input(self, code, **changes):
        request = {
            'idempotency_key': str(uuid.uuid4()),
            'executable': self.executable,
            'args': ['-c', code],
            'cwd': str(self.workspace),
            'deadline_ms': 5000,
        }
        request.update(changes)
        return request

    def operation(self, operation_id):
        return unwrap(self.call('operation_get', {'operation_id': operation_id}), 'operation')

    def close(self):
        if self.process is not None and self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        if self.log:
            self.log.close()
        self.temporary.cleanup()


class SecurityBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)

    def assert_error(self, response, may_close=False):
        if may_close and response is None:
            return
        self.assertIsInstance(response, dict, f'Expected explicit error response, got {response!r}')
        self.assertEqual(response.get('status'), 'error', response)

    def assert_responsive(self):
        self.assert_error(self.fixture.call('operation_get', {'operation_id': str(uuid.uuid4())}))
        self.assertIsNone(self.fixture.process.poll(), 'Malformed request terminated broker')

    def test_malformed_frames_and_request_fields_are_rejected(self):
        f = self.fixture.start()
        valid = envelope('operation_get', {'operation_id': str(uuid.uuid4())})
        cases = {
            'empty JSON': b'',
            'invalid JSON': b'{',
            'invalid UTF8': b'\xff',
            'trailing bytes in JSON frame': json.dumps(valid).encode() + b' junk',
            'unsupported API version': json.dumps({**valid, 'api_version': 999}).encode(),
            'unknown envelope field': json.dumps({**valid, 'approval': True}).encode(),
            'unknown method field': json.dumps({
                'api_version': 1, 'request': {**valid['request'], 'approval': True},
            }).encode(),
            'unknown params field': json.dumps(envelope('operation_get', {
                'operation_id': str(uuid.uuid4()), 'approval': True,
            })).encode(),
            'unknown method': json.dumps(envelope('set_allowed_executables', {})).encode(),
            'duplicate API version': ('{"api_version":1,"api_version":999,"request":' +
                                      json.dumps(valid['request']) + '}').encode(),
            'duplicate method': ('{"api_version":1,"request":{"method":"operation_get",'
                                 '"method":"operation_cancel","params":' +
                                 json.dumps(valid['request']['params']) + '}}').encode(),
            'duplicate parameter': ('{"api_version":1,"request":{"method":"operation_get",'
                                    '"params":{"operation_id":"first","operation_id":"second"}}}').encode(),
            'negative cursor': json.dumps(envelope('operation_read', {
                'operation_id': str(uuid.uuid4()), 'cursor': -1,
            })).encode(),
            'overflow cursor': json.dumps(envelope('operation_read', {
                'operation_id': str(uuid.uuid4()), 'cursor': 2 ** 64,
            })).encode(),
        }
        for label, body in cases.items():
            with self.subTest(label=label):
                self.assert_error(f.raw(body), may_close=True)
                self.assert_responsive()
        with self.subTest(label='oversized frame header without body'):
            self.assert_error(f.raw(b'', declared_size=MAX_FRAME + 1), may_close=True)
            self.assert_responsive()
        with self.subTest(label='truncated frame body'):
            self.assert_error(f.raw(b'{', declared_size=100), may_close=True)
            self.assert_responsive()

    def test_grant_and_schema_denials_have_no_process_side_effect(self):
        f = self.fixture.start()
        markers = []
        shell = shutil.which('sh')
        if shell is None:
            self.fail('This Linux proof test requires sh for the ungranted-executable fixture')
        outside = f.base / 'outside'
        outside.mkdir()
        case_sibling = f.base / 'WORKSPACE'
        case_sibling.mkdir()
        prefix_sibling = f.base / 'workspace-extra'
        prefix_sibling.mkdir()
        outside_link = f.workspace / 'outside-link'
        outside_link.symlink_to(outside, target_is_directory=True)
        variants = [
            ('relative executable', {'executable': Path(f.executable).name}),
            ('ungranted executable', {'executable': str(Path(shell).resolve())}),
            ('outside workspace cwd', {'cwd': str(outside)}),
            ('case-sensitive sibling cwd', {'cwd': str(case_sibling)}),
            ('partial-name sibling cwd', {'cwd': str(prefix_sibling)}),
            ('symlink to outside cwd', {'cwd': str(outside_link)}),
            ('relative cwd', {'cwd': '.'}),
            ('zero deadline', {'deadline_ms': 0}),
            ('oversized deadline', {'deadline_ms': 300001}),
            ('unknown permission claim', {'approved': True}),
        ]
        for label, changes in variants:
            with self.subTest(label=label):
                marker = f.base / f'marker-{len(markers)}'
                markers.append(marker)
                code = f'from pathlib import Path; Path({str(marker)!r}).write_text("executed")'
                params = f.start_input(code, **changes)
                if label == 'ungranted executable':
                    # The shell would create a marker if mistakenly launched.
                    import shlex
                    params['args'] = ['-c', f'printf executed > {shlex.quote(str(marker))}']
                self.assert_error(f.call('operation_start', params))
                self.assertFalse(marker.exists(), f'Denied request executed: {label}')
        # Duplicate recognized fields must not choose the first or last grant.
        marker = f.base / 'duplicate-field-marker'
        markers.append(marker)
        code = f'from pathlib import Path; Path({str(marker)!r}).write_text("executed")'
        params_json = json.dumps(f.start_input(code))
        duplicate = params_json[:-1] + ',"executable":' + json.dumps(f.executable) + '}'
        body = ('{"api_version":1,"request":{"method":"operation_start","params":' + duplicate + '}}').encode()
        self.assert_error(f.raw(body), may_close=True)
        time.sleep(0.1)
        for marker in markers:
            self.assertFalse(marker.exists(), f'Rejected request created {marker.name}')
        self.assert_responsive()

    def test_only_one_request_is_processed_per_connection(self):
        f = self.fixture.start()
        marker = f.base / 'second-request-marker'
        code = f'from pathlib import Path; Path({str(marker)!r}).write_text("executed")'
        first = json.dumps(envelope('operation_get', {'operation_id': str(uuid.uuid4())})).encode()
        second = json.dumps(envelope('operation_start', f.start_input(code))).encode()
        connection = socket.socket(socket.AF_UNIX)
        try:
            connection.settimeout(3)
            connection.connect(str(f.socket_path))
            connection.sendall(struct.pack('>I', len(first)) + first + struct.pack('>I', len(second)) + second)
            connection.shutdown(socket.SHUT_WR)
            self.assert_error(receive_response(connection))
            try:
                extra = receive_response(connection)
            except ConnectionResetError:
                extra = None
            self.assertIsNone(extra, 'A second request was processed on a one-request connection')
        finally:
            connection.close()
        time.sleep(0.1)
        self.assertFalse(marker.exists(), 'Trailing second frame caused execution')

    def test_incomplete_peers_are_timed_out_and_connection_capacity_recovers(self):
        f = self.fixture.start()
        peers = []
        try:
            # The broker admits at most 32 connection tasks. These peers send
            # only one header byte; excess peers may be closed immediately.
            for _ in range(40):
                connection = socket.socket(socket.AF_UNIX)
                peers.append(connection)
                connection.settimeout(7)
                connection.connect(str(f.socket_path))
                try:
                    connection.sendall(b'\x00')
                except (BrokenPipeError, ConnectionResetError):
                    pass
            for connection in peers:
                try:
                    response = receive_response(connection)
                except (BrokenPipeError, ConnectionResetError):
                    response = None
                self.assert_error(response, may_close=True)
        finally:
            for connection in peers:
                connection.close()
        self.assert_responsive()

    def test_owner_only_state_and_socket_permissions(self):
        f = self.fixture.start()
        state = f.state.stat()
        sock = f.socket_path.lstat()
        self.assertEqual(stat.S_IMODE(state.st_mode), 0o700)
        self.assertEqual(state.st_uid, os.geteuid())
        self.assertTrue(stat.S_ISSOCK(sock.st_mode))
        self.assertEqual(stat.S_IMODE(sock.st_mode), 0o600)
        self.assertEqual(sock.st_uid, os.geteuid())

    def test_insecure_existing_state_is_rejected(self):
        f = self.fixture
        f.state.mkdir(mode=0o755)
        f.state.chmod(0o755)
        f.failed_start()
        self.assertFalse(f.socket_path.exists())

    def test_symlink_state_and_ancestor_are_rejected(self):
        f = self.fixture
        target = f.base / 'real-state'
        target.mkdir(mode=0o700)
        f.state.symlink_to(target, target_is_directory=True)
        f.failed_start()
        self.assertFalse((target / 'broker.lock').exists())
        f.state.unlink()
        parent = f.base / 'real-parent'
        parent.mkdir(mode=0o700)
        alias = f.base / 'parent-alias'
        alias.symlink_to(parent, target_is_directory=True)
        f.state = alias / 'state'
        f.failed_start()
        self.assertFalse((parent / 'state').exists())

    def test_socket_and_lock_files_are_not_overwritten(self):
        f = self.fixture
        f.state.mkdir(mode=0o700)
        sentinel = f.base / 'sentinel'
        sentinel.write_text('preserve me')
        f.socket_path.write_text('regular socket collision')
        socket_inode = f.socket_path.lstat().st_ino
        f.failed_start()
        self.assertEqual(f.socket_path.read_text(), 'regular socket collision')
        self.assertEqual(f.socket_path.lstat().st_ino, socket_inode)
        f.socket_path.unlink()
        f.socket_path.symlink_to(sentinel)
        f.failed_start()
        self.assertTrue(f.socket_path.is_symlink())
        self.assertEqual(sentinel.read_text(), 'preserve me')
        f.socket_path.unlink()
        lock = f.state / 'broker.lock'
        if lock.exists():
            lock.unlink()
        lock.symlink_to(sentinel)
        f.failed_start()
        self.assertTrue(lock.is_symlink())
        self.assertEqual(sentinel.read_text(), 'preserve me')

    def test_hardlinked_journal_cannot_modify_an_unrelated_file(self):
        f = self.fixture
        f.state.mkdir(mode=0o700)
        sentinel = f.base / 'unrelated-empty-file'
        sentinel.write_bytes(b'')
        sentinel.chmod(0o600)
        os.link(sentinel, f.state / 'operations.sqlite3')
        try:
            f.failed_start()
        finally:
            self.assertEqual(sentinel.read_bytes(), b'', 'SQLite initialization modified a hardlinked unrelated file')

    def test_active_socket_is_preserved_even_without_a_broker_lock(self):
        f = self.fixture
        f.state.mkdir(mode=0o700)
        existing = socket.socket(socket.AF_UNIX)
        self.addCleanup(existing.close)
        existing.bind(str(f.socket_path))
        existing.listen(4)
        original_inode = f.socket_path.lstat().st_ino
        f.failed_start()
        self.assertEqual(f.socket_path.lstat().st_ino, original_inode)
        probe = socket.socket(socket.AF_UNIX)
        self.addCleanup(probe.close)
        probe.settimeout(1)
        probe.connect(str(f.socket_path))

    def test_owned_stale_socket_can_be_replaced(self):
        f = self.fixture
        f.state.mkdir(mode=0o700)
        stale = socket.socket(socket.AF_UNIX)
        stale.bind(str(f.socket_path))
        stale.close()
        f.start()
        self.assert_responsive()

    def test_second_broker_cannot_reconcile_the_active_brokers_job(self):
        f = self.fixture.start()
        start = unwrap(f.call('operation_start', f.start_input('import time; time.sleep(2)')), 'start')
        operation_id = start['operation']['operation_id']
        deadline = time.monotonic() + 1
        operation = f.operation(operation_id)
        while operation['state'] == 'accepted' and time.monotonic() < deadline:
            time.sleep(0.01)
            operation = f.operation(operation_id)
        self.assertEqual(operation['state'], 'running', operation)
        original_inode = f.socket_path.lstat().st_ino
        f.failed_start()
        self.assertEqual(f.socket_path.lstat().st_ino, original_inode)
        after = f.operation(operation_id)
        self.assertIn(after['state'], {'running', 'succeeded'}, after)

    def test_inherited_child_remains_cancellable_after_root_exits(self):
        f = self.fixture.start()
        ready = f.base / 'child-ready.json'
        marker = f.base / 'child-effect-marker'
        code = (
            'import os, time, json\nfrom pathlib import Path\n'
            'if os.fork():\n    os._exit(0)\n'
            f'Path({str(ready)!r}).write_text(json.dumps({{"pid":os.getpid(),"group":os.getpgrp()}}))\n'
            'time.sleep(3)\n'
            f'try:\n    Path({str(marker)!r}).write_text("executed")\n'
            'finally:\n    os._exit(0)\n'
        )
        start = unwrap(f.call('operation_start', f.start_input(code)), 'start')
        operation_id = start['operation']['operation_id']
        deadline = time.monotonic() + 1
        child = None
        while time.monotonic() < deadline:
            try:
                child = json.loads(ready.read_text())
                break
            except (FileNotFoundError, json.JSONDecodeError):
                time.sleep(0.01)
        self.assertIsNotNone(child, 'The inherited-child fixture did not start')
        # A root-only implementation previously became terminal after its one
        # second output-drain timeout, stranding this same-group descendant.
        time.sleep(1.2)
        before = f.operation(operation_id)
        self.assertEqual(before['state'], 'running', before)
        cancellation = unwrap(f.call('operation_cancel', {'operation_id': operation_id}), 'operation')
        self.assertTrue(cancellation['cancellation_requested'])
        deadline = time.monotonic() + 3
        after = f.operation(operation_id)
        while after['state'] in {'accepted', 'running'} and time.monotonic() < deadline:
            time.sleep(0.02)
            after = f.operation(operation_id)
        self.assertEqual(after['state'], 'cancelled', after)
        self.assertTrue(after['output_complete'], after)
        self.assertFalse(marker.exists(), 'An inherited-group descendant executed after cancellation')
        try:
            process_stat = Path(f'/proc/{child["pid"]}/stat').read_text()
            state = process_stat[process_stat.rfind(')') + 1:].split()[0]
            self.assertIn(state, {'Z', 'X'}, 'The cancelled inherited-group child is still executing')
        except FileNotFoundError:
            pass

    @unittest.skipUnless(os.geteuid() == 0, 'Foreign-UID fixtures require a root test runner')
    def test_foreign_owned_state_and_socket_are_preserved(self):
        if not CAPABILITIES.get('different_uid'):
            self.skipTest(CAPABILITIES.get('different_uid_reason', 'Distinct UID unavailable'))
        f = self.fixture
        f.state.mkdir(mode=0o700)
        os.chown(f.state, 65534, 65534)
        f.failed_start()
        self.assertFalse(f.socket_path.exists())
        os.chown(f.state, os.geteuid(), os.getegid())
        stale = socket.socket(socket.AF_UNIX)
        stale.bind(str(f.socket_path))
        stale.close()
        os.chown(f.socket_path, 65534, 65534)
        original_inode = f.socket_path.lstat().st_ino
        f.failed_start()
        self.assertEqual(f.socket_path.lstat().st_ino, original_inode)
        self.assertEqual(f.socket_path.lstat().st_uid, 65534)

    @unittest.skipUnless(os.geteuid() == 0, 'Peer-UID fixture requires privilege to launch a different UID')
    def test_peer_uid_is_checked_independently_of_socket_file_mode(self):
        if not CAPABILITIES.get('different_uid'):
            self.skipTest(CAPABILITIES.get('different_uid_reason', 'Distinct UID unavailable'))
        f = self.fixture.start()
        # Deliberately weaken only this fixture to reach the independent peer-UID
        # check. Ordinary configured state remains 0700 and socket remains 0600.
        f.base.chmod(0o711)
        f.state.chmod(0o711)
        f.socket_path.chmod(0o666)
        worker = r'''
import json, os, socket, struct, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(2)
result = {'uid': os.geteuid(), 'connected': False, 'response': None}
try:
    s.connect(sys.argv[1]); result['connected'] = True
    body = json.dumps({'api_version': 1, 'request': {'method': 'operation_get', 'params': {'operation_id': 'unavailable'}}}).encode()
    s.sendall(struct.pack('>I', len(body)) + body)
    head = s.recv(4)
    if len(head) == 4:
        size = struct.unpack('>I', head)[0]
        data = bytearray()
        while len(data) < size:
            part = s.recv(size-len(data))
            if not part: break
            data.extend(part)
        if len(data) == size: result['response'] = json.loads(data)
except (BrokenPipeError, ConnectionResetError):
    pass
finally:
    s.close()
print(json.dumps(result))
'''
        def drop_uid():
            os.setgroups([])
            os.setgid(65534)
            os.setuid(65534)
        try:
            result = subprocess.run([sys.executable, '-c', worker, str(f.socket_path)],
                                    capture_output=True, text=True, timeout=4, preexec_fn=drop_uid)
            self.assertEqual(result.returncode, 0, result.stderr)
            observation = json.loads(result.stdout)
            self.assertEqual(observation['uid'], 65534)
            self.assertTrue(observation['connected'], observation)
            response = observation['response']
            if response is not None:
                self.assert_error(response)
                code = response['value']['code'].lower()
                self.assertTrue(any(word in code for word in ('auth', 'denied', 'forbid', 'peer', 'uid')), response)
        finally:
            f.base.chmod(0o700)
            f.state.chmod(0o700)
            f.socket_path.chmod(0o600)
        self.assert_responsive()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--broker', default=str(Path(__file__).resolve().parents[1] / 'target/debug/bridge-broker'))
    parser.add_argument('--report', help='Optional path for machine-readable verification evidence')
    args, remaining = parser.parse_known_args()
    BROKER = Path(args.broker).resolve()
    if not BROKER.is_file():
        parser.error(f'Build the broker first; binary missing: {BROKER}')
    if sys.platform != 'linux':
        parser.error('This proof and its Unix peer-credential checks are Linux-specific')
    before_hash = hashlib.sha256(BROKER.read_bytes()).hexdigest()
    report = {
        'started_at_utc': datetime.now(timezone.utc).isoformat(),
        'broker_binary': str(BROKER),
        'broker_sha256': before_hash,
        'scope': 'Linux IPC, explicit process grants, state integrity, and owned inherited-group cancellation; not an OS sandbox',
    }
    CAPABILITIES = preflight()
    report['capabilities'] = CAPABILITIES
    if not CAPABILITIES['unix_socket']:
        report.update({'status': 'blocked', 'tests_run': 0})
        if args.report:
            Path(args.report).write_text(json.dumps(report, indent=2) + '\n')
        print(CAPABILITIES['error'], file=sys.stderr)
        sys.exit(2)
    runner = unittest.TextTestRunner(verbosity=2, resultclass=EvidenceResult)
    program = unittest.main(argv=[sys.argv[0], *remaining], exit=False, testRunner=runner)
    result = program.result
    changed = hashlib.sha256(BROKER.read_bytes()).hexdigest() != before_hash
    report.update({
        'finished_at_utc': datetime.now(timezone.utc).isoformat(),
        'status': 'passed' if result.wasSuccessful() and not changed else 'failed',
        'tests_run': result.testsRun,
        'failures': len(result.failures),
        'errors': len(result.errors),
        'skipped': len(result.skipped),
        'binary_changed_during_run': changed,
        'results': result.evidence,
    })
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=2) + '\n')
    sys.exit(0 if result.wasSuccessful() and not changed else 1)
