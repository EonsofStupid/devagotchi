#!/usr/bin/env python3
"""Probe the real stdio adapter with raw JSON-RPC, without executing broker jobs.

Wire metadata and expected errors follow rmcp 3.5.1's model/meta.rs,
model/serde_impl.rs, and tests/test_stateless_server_requests.rs. This is a
protocol probe, not certification of any named MCP client or a complete JSON
Schema conformance test. A nonexistent --socket is sufficient.
"""

import argparse
from collections import deque
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import queue
import subprocess
import sys
import threading
import time


REQUEST_TIMEOUT = 5.0
MAX_RESPONSE_BYTES = 256 * 1024
MODERN_VERSION = "2026-07-28"
INITIALIZE_VERSION = "2025-11-25"
UNSUPPORTED_VERSION = "2099-01-01"
CLIENT_INFO = {"name": "desktop-bridge-raw-protocol-probe", "version": "0.1.0"}
TOOL_SCHEMAS = {
    "operation_start": (
        {"idempotency_key", "executable", "args", "command", "cwd", "deadline_ms"},
        {"operation", "replayed"},
    ),
    "operation_get": ({"operation_id"}, {"operation_id", "state", "detail"}),
    "operation_read": (
        {"operation_id", "cursor", "max_bytes"},
        {"operation_id", "frames", "next_cursor", "gap", "has_more", "operation"},
    ),
    "operation_cancel": ({"operation_id"}, {"operation_id", "state", "detail"}),
    "operation_wait": ({"operation_id", "output_after", "timeout_ms"}, {"operation_id", "state", "detail"}),
    "operation_list": ({"offset", "limit"}, {"operations"}),
    "run": (
        {"command", "cwd", "wait_seconds", "timeout_seconds"},
        {"operation_id", "state", "exit_code", "output", "truncated"},
    ),
    "read_file": ({"path", "offset", "max_bytes", "start_line", "line_count"}, {"path", "size", "offset", "next_offset", "eof"}),
    "list_directory": ({"path", "depth", "max_entries"}, {"path", "entries", "truncated"}),
    "write_file": (
        {"path", "text", "bytes_base64", "mode", "expected_sha256", "create_dirs"},
        {"path", "size", "sha256", "created"},
    ),
    "edit_file": ({"path", "old_text", "new_text", "expected_replacements"}, {"path", "size", "sha256", "created"}),
    "read_multiple_files": ({"paths", "max_bytes_each"}, {"files"}),
    "get_file_info": ({"path"}, {"path", "kind", "size", "readonly"}),
    "create_directory": ({"path"}, {"path", "kind", "size", "readonly"}),
    "move_file": ({"source", "destination", "overwrite"}, {"path", "kind", "size", "readonly"}),
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def modern_meta(version=MODERN_VERSION):
    return {
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": CLIENT_INFO,
        "io.modelcontextprotocol/clientCapabilities": {},
    }


class StdioPeer:
    """One adapter process; bounded responses and shutdown, with stderr drained."""

    def __init__(self, adapter, socket):
        self.process = subprocess.Popen(
            [str(adapter), "mcp", "--socket", str(socket)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        self.messages = queue.Queue(maxsize=64)
        self.stderr_chunks = deque(maxlen=4)
        self.stderr_lock = threading.Lock()
        self.next_id = 0
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self):
        try:
            while True:
                line = self.process.stdout.readline(MAX_RESPONSE_BYTES + 1)
                if not line:
                    self.messages.put(("eof", None), timeout=REQUEST_TIMEOUT)
                    return
                if len(line) > MAX_RESPONSE_BYTES or not line.endswith(b"\n"):
                    raise RuntimeError("stdout response exceeds the bound or lacks newline framing")
                value = json.loads(line)
                require(isinstance(value, dict), "stdout message must be a JSON object")
                require(value.get("jsonrpc") == "2.0", "stdout contains non-JSON-RPC output")
                self.messages.put(("message", value), timeout=REQUEST_TIMEOUT)
        except Exception as error:
            try:
                self.messages.put(("error", str(error)), timeout=REQUEST_TIMEOUT)
            except queue.Full:
                pass

    def _read_stderr(self):
        try:
            while chunk := self.process.stderr.read(2048):
                with self.stderr_lock:
                    self.stderr_chunks.append(chunk)
        except (OSError, ValueError):
            pass

    def stderr_tail(self):
        with self.stderr_lock:
            return b"".join(self.stderr_chunks).decode("utf-8", errors="replace")[-4096:]

    def send(self, message):
        payload = json.dumps(message, separators=(",", ":"), allow_nan=False).encode() + b"\n"
        require(len(payload) < 4096, "probe request exceeds its fixed write bound")
        self.process.stdin.write(payload)
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        request_id = f"probe-{self.next_id}"
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        self.send(message)
        deadline = time.monotonic() + REQUEST_TIMEOUT
        while True:
            remaining = deadline - time.monotonic()
            require(remaining > 0, f"timeout waiting for {method}")
            try:
                kind, value = self.messages.get(timeout=remaining)
            except queue.Empty as error:
                raise RuntimeError(f"timeout waiting for {method}") from error
            require(kind != "eof", f"adapter stdout closed while waiting for {method}")
            require(kind != "error", f"invalid stdout: {value}")
            if "id" not in value and isinstance(value.get("method"), str):
                continue  # Protocol notifications do not satisfy an outstanding request.
            require(value.get("id") == request_id, f"unexpected response ID while waiting for {method}")
            require(("result" in value) != ("error" in value), "response must contain result or error")
            return value

    def notify_initialized(self):
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def close(self):
        forced = False
        try:
            self.process.stdin.close()
        except (OSError, ValueError):
            pass
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            forced = True
            self.process.terminate()
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=1)
        return {"exit_code": self.process.returncode, "forced_termination": forced}


def result_of(response):
    require("error" not in response, f"unexpected JSON-RPC error: {response.get('error')}")
    result = response["result"]
    require(isinstance(result, dict), "expected an object result")
    return result


def tools_only(capabilities):
    require(isinstance(capabilities, dict), "missing capabilities object")
    require(set(capabilities) == {"tools"}, f"unexpected capabilities: {sorted(capabilities)}")


def validate_catalog(response):
    result = result_of(response)
    tools = result.get("tools")
    require(isinstance(tools, list), "tools/list did not return a list")
    names = [tool.get("name") for tool in tools]
    require(len(names) == len(TOOL_SCHEMAS) and set(names) == set(TOOL_SCHEMAS), f"unexpected tools: {names}")
    require(not result.get("nextCursor"), "tool catalog unexpectedly requires another page")
    for tool in tools:
        name = tool["name"]
        input_schema, output_schema = tool.get("inputSchema"), tool.get("outputSchema")
        for label, schema in [("inputSchema", input_schema), ("outputSchema", output_schema)]:
            require(isinstance(schema, dict) and schema.get("type") == "object", f"{name}: missing object {label}")
        expected_input, expected_output = TOOL_SCHEMAS[name]
        require(set(input_schema.get("properties", {})) == expected_input, f"{name}: unexpected input properties")
        require(input_schema.get("additionalProperties") is False, f"{name}: input schema permits unknown fields")
        require(expected_output <= set(output_schema.get("properties", {})), f"{name}: missing output properties")
    return {"tools": sorted(names), "object_output_schemas": len(names), "input_schemas_reject_unknown_fields": True}


def error_code(response, expected):
    error = response.get("error")
    require(isinstance(error, dict), f"expected JSON-RPC error {expected}")
    require(error.get("code") == expected, f"expected error {expected}, received {error}")
    return {"error_code": expected}


def run_session(label, adapter, socket, checks):
    peer = None
    current = "launch"

    def check(name, operation):
        nonlocal current
        current = name
        detail = operation()
        checks.append({"name": f"{label}.{name}", "passed": True, **(detail or {})})

    try:
        peer = StdioPeer(adapter, socket)
        modern = label == "discover_2026_07_28"
        if modern:
            def discover():
                result = result_of(peer.request("server/discover", {"_meta": modern_meta()}))
                require(MODERN_VERSION in result.get("supportedVersions", []), "discovery omits 2026-07-28")
                require(result.get("resultType") == "complete", "discovery response is not complete")
                tools_only(result.get("capabilities"))
                return {"supported_versions": result["supportedVersions"]}

            check("discovery", discover)
            check("catalog", lambda: validate_catalog(peer.request("tools/list", {"_meta": modern_meta()})))
            check("unknown_version_rejected", lambda: error_code(
                peer.request("tools/list", {"_meta": modern_meta(UNSUPPORTED_VERSION)}), -32022
            ))
            check("missing_request_metadata_rejected", lambda: error_code(peer.request("tools/list", {}), -32602))
            check("valid_request_after_rejection", lambda: validate_catalog(peer.request("tools/list", {"_meta": modern_meta()})))
        else:
            def initialize():
                result = result_of(peer.request("initialize", {
                    "protocolVersion": INITIALIZE_VERSION,
                    "capabilities": {},
                    "clientInfo": CLIENT_INFO,
                }))
                require(result.get("protocolVersion") == INITIALIZE_VERSION, "initialize negotiated an unexpected revision")
                tools_only(result.get("capabilities"))
                peer.notify_initialized()
                return {"negotiated_version": result["protocolVersion"]}

            check("initialize", initialize)
            check("catalog", lambda: validate_catalog(peer.request("tools/list", {})))

        unknown_params = {"name": "probe_tool_that_does_not_exist", "arguments": {}}
        if modern:
            unknown_params["_meta"] = modern_meta()
        check("unknown_tool_rejected", lambda: error_code(peer.request("tools/call", unknown_params), -32602))
    except Exception as error:
        checks.append({
            "name": f"{label}.{current}", "passed": False, "error": str(error),
            "stderr_tail": peer.stderr_tail() if peer else "",
        })
    finally:
        if peer:
            try:
                outcome = peer.close()
                checks.append({
                    "name": f"{label}.clean_shutdown",
                    "passed": outcome["exit_code"] == 0 and not outcome["forced_termination"],
                    **outcome,
                })
            except Exception as error:
                checks.append({"name": f"{label}.clean_shutdown", "passed": False, "error": str(error)})


def absolute_path(value):
    path = Path(value)
    if not path.is_absolute():
        raise argparse.ArgumentTypeError("must be an absolute filesystem path")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--devagotchi", "--adapter", dest="adapter", type=absolute_path, required=True, help="the one binary; the adapter is `devagotchi mcp`")
    parser.add_argument("--socket", type=absolute_path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    summary = {
        "probe": "desktop-bridge-mcp-protocol",
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.system(),
        "adapter": str(args.adapter),
        "scope": "Raw stdio protocol and catalog only; no broker execution or named-client certification.",
        "checks": [],
    }
    try:
        with args.adapter.open("rb") as binary:
            digest = hashlib.sha256()
            for chunk in iter(lambda: binary.read(64 * 1024), b""):
                digest.update(chunk)
        summary["adapter_sha256"] = digest.hexdigest()
        for label in ("discover_2026_07_28", "initialize_2025_11_25"):
            run_session(label, args.adapter, args.socket, summary["checks"])
    except Exception as error:
        summary["checks"].append({"name": "probe_setup", "passed": False, "error": str(error)})
    summary["passed"] = bool(summary["checks"]) and all(check["passed"] for check in summary["checks"])
    if args.output:
        try:
            args.output.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        except OSError as error:
            summary["passed"] = False
            summary["checks"].append({"name": "save_summary", "passed": False, "error": str(error)})
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
