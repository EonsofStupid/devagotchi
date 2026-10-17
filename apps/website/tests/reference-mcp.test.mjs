import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

// Exercise the authored handler without starting a server or building/publishing
// the site. TypeScript is already a locked project development dependency.
const root = new URL("../", import.meta.url);
const schema = JSON.parse(readFileSync(new URL("public/reference/broker-contracts.schema.json", root), "utf8"));

function loadModule(path, imports) {
  const source = readFileSync(new URL(path, root), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      esModuleInterop: true,
    },
    fileName: path,
  });
  const exports = {};
  new Function("require", "exports", outputText)((name) => {
    assert.ok(Object.hasOwn(imports, name), `Unexpected import: ${name}`);
    return imports[name];
  }, exports);
  return exports;
}

const reference = loadModule("lib/devagotchi-reference.ts", {
  "../public/reference/broker-contracts.schema.json": schema,
});
const { handleReferenceMcp } = loadModule("lib/reference-mcp.ts", {
  "./devagotchi-reference": reference,
});
const referenceRoute = loadModule("app/api/v1/reference/route.ts", {
  "../../../../lib/devagotchi-reference": reference,
});
const capabilitiesRoute = loadModule("app/api/v1/capabilities/route.ts", {
  "../../../../lib/devagotchi-reference": reference,
});
const prefix = "io.modelcontextprotocol/";

function rpc(method = "server/discover", params = {}) {
  return {
    jsonrpc: "2.0", id: "check-1", method,
    params: {
      _meta: { [`${prefix}protocolVersion`]: "2026-07-28", [`${prefix}clientCapabilities`]: {} },
      ...params,
    },
  };
}

function request(body = rpc(), headers = {}) {
  const defaults = {
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
    "MCP-Protocol-Version": body.params?._meta?.[`${prefix}protocolVersion`] ?? "2026-07-28",
    "Mcp-Method": body.method ?? "server/discover",
  };
  if (body.method === "tools/call" && typeof body.params?.name === "string") defaults["Mcp-Name"] = body.params.name;
  const actual = new Headers(defaults);
  for (const [key, value] of Object.entries(headers)) {
    if (value === null) actual.delete(key);
    else actual.set(key, value);
  }
  return new Request("https://devagotchi.justsayit.chatgpt.site/mcp", {
    method: "POST", headers: actual, body: JSON.stringify(body),
  });
}

async function call(body, headers) {
  const response = await handleReferenceMcp(request(body, headers));
  return { response, body: await response.json() };
}

function protocolError(result, status, code) {
  assert.equal(result.response.status, status);
  assert.equal(result.body.error?.code, code);
  assert.equal(Object.hasOwn(result.body, "result"), false);
}

test("modern discovery works without optional clientInfo and identifies the read-only server", async () => {
  const result = await call(rpc());
  assert.equal(result.response.status, 200);
  assert.equal(result.body.id, "check-1");
  assert.equal(result.body.result.resultType, "complete");
  assert.deepEqual(result.body.result.supportedVersions, ["2026-07-28"]);
  assert.deepEqual(result.body.result.capabilities, { tools: {} });
  assert.equal(result.body.result._meta[`${prefix}serverInfo`].name, "devagotchi-reference");
});

test("the public catalog exposes only the two reference tools", async () => {
  const result = await call(rpc("tools/list"));
  assert.equal(result.response.status, 200);
  assert.deepEqual(result.body.result.tools.map(t => t.name), ["devagotchi_get_capabilities", "devagotchi_get_reference"]);
  assert.equal(result.body.result.tools.every(t => t.annotations.readOnlyHint && !t.annotations.destructiveHint), true);
  protocolError(await call(rpc("tools/call", { name: "operation_start", arguments: {} })), 400, -32602);
});

test("a Base64 Mcp-Name is decoded and compared; structured/text results agree", async () => {
  const name = "devagotchi_get_capabilities";
  const result = await call(rpc("tools/call", { name, arguments: {} }), {
    "mcp-name": `=?base64?${Buffer.from(name).toString("base64")}?=`,
  });
  assert.equal(result.response.status, 200);
  assert.equal(result.body.result.isError, false);
  assert.deepEqual(JSON.parse(result.body.result.content[0].text), result.body.result.structuredContent);
  assert.equal(result.body.result.structuredContent.workspace.remote_execution_available, false);
});

test("required, mismatched, malformed, and duplicate mirrored headers fail", async () => {
  const body = rpc("tools/call", { name: "devagotchi_get_capabilities", arguments: {} });
  for (const headers of [
    { "MCP-Protocol-Version": null },
    { "Mcp-Method": null },
    { "Mcp-Name": null },
    { "MCP-Protocol-Version": "2025-11-25" },
    { "Mcp-Method": "tools/list" },
    { "Mcp-Name": "different" },
    { "Mcp-Name": "=?base64?!!!?=" },
    { "Mcp-Name": "=?base64?/w==?=" },
  ]) protocolError(await call(body, headers), 400, -32020);
  const duplicated = request(body);
  duplicated.headers.append("Mcp-Name", body.params.name);
  const response = await handleReferenceMcp(duplicated);
  protocolError({ response, body: await response.json() }, 400, -32020);
});

test("metadata and unsupported-version errors retain their defined modern codes", async () => {
  const missing = rpc();
  delete missing.params._meta[`${prefix}clientCapabilities`];
  protocolError(await call(missing), 400, -32602);
  const future = rpc();
  future.params._meta[`${prefix}protocolVersion`] = "2099-01-01";
  const rejected = await call(future);
  protocolError(rejected, 400, -32022);
  assert.deepEqual(rejected.body.error.data, { supported: ["2026-07-28"], requested: "2099-01-01" });
  protocolError(await call(rpc("method/unknown")), 404, -32601);
});

test("Origin, unsupported HTTP methods, and media types are enforced", async () => {
  protocolError(await call(rpc(), { Origin: "https://attacker.invalid" }), 403, -32600);
  assert.equal((await call(rpc(), { Origin: reference.SITE_ORIGIN })).response.status, 200);
  for (const method of ["GET", "DELETE"]) {
    const response = await handleReferenceMcp(new Request(`${reference.SITE_ORIGIN}/mcp`, { method }));
    assert.equal(response.status, 405);
    assert.equal(response.headers.get("allow"), "POST");
  }
  protocolError(await call(rpc(), { "Content-Type": "text/plain" }), 415, -32600);
  protocolError(await call(rpc(), { Accept: "application/json" }), 406, -32600);
});

test("malformed UTF-8, malformed JSON, batch messages, and excessive byte counts are rejected", async () => {
  const headers = request().headers;
  for (const body of [new Uint8Array([0xff]), "{"]) {
    const response = await handleReferenceMcp(new Request(`${reference.SITE_ORIGIN}/mcp`, { method: "POST", headers, body }));
    protocolError({ response, body: await response.json() }, 400, -32700);
  }
  protocolError(await call([rpc()]), 400, -32600);
  const response = await handleReferenceMcp(new Request(`${reference.SITE_ORIGIN}/mcp`, {
    method: "POST", headers, body: " ".repeat(65537),
  }));
  protocolError({ response, body: await response.json() }, 413, -32600);
});

test("non-object CallToolRequest arguments are protocol errors, never successful empty arguments", async () => {
  for (const value of [null, [], "wrong", 1, false]) {
    protocolError(await call(rpc("tools/call", { name: "devagotchi_get_capabilities", arguments: value })), 400, -32602);
  }
});

test("valid argument objects with invalid tool inputs produce isError results", async () => {
  for (const body of [
    rpc("tools/call", { name: "devagotchi_get_capabilities", arguments: { unwanted: true } }),
    rpc("tools/call", { name: "devagotchi_get_reference", arguments: { topic: "not-a-topic" } }),
  ]) {
    const result = await call(body);
    assert.equal(result.response.status, 200);
    assert.equal(result.body.result.resultType, "complete");
    assert.equal(result.body.result.isError, true);
  }
});

test("an oversized-body rejection is not held hostage by asynchronous stream cancellation", async () => {
  const stream = new ReadableStream({
    start(controller) { controller.enqueue(new Uint8Array(65537)); },
    cancel() { return new Promise(() => {}); },
  });
  const response = await Promise.race([
    handleReferenceMcp(new Request(`${reference.SITE_ORIGIN}/mcp`, {
      method: "POST", headers: request().headers, body: stream, duplex: "half",
    })),
    new Promise(resolve => setTimeout(() => resolve("cleanup stalled"), 250)),
  ]);
  assert.notEqual(response, "cleanup stalled", "bounded body rejection awaited an unbounded cancel() promise");
  assert.equal(response.status, 413);
});

test("public reference GET routes expose static documents and reject ambiguous queries", async () => {
  const caps = await capabilitiesRoute.GET().json();
  assert.equal(caps.workspace.mesh_available, false);
  for (const suffix of ["?topic=mesh&topic=quickstart", "?extra=value", "?topic=__proto__"]) {
    const response = referenceRoute.GET(new Request(`${reference.SITE_ORIGIN}/api/v1/reference${suffix}`));
    assert.equal(response.status, 400);
  }
  const response = referenceRoute.GET(new Request(`${reference.SITE_ORIGIN}/api/v1/reference?topic=mesh`));
  assert.equal((await response.json()).reference.stage, "design-proposal");
  assert.equal(response.headers.get("access-control-allow-origin"), "*");
});

test("copyable broker command has no patch-marker arguments", () => {
  assert.doesNotMatch(reference.examples.startBroker, /\n\s*\+/);
  assert.match(reference.examples.startBroker, /--state-dir /);
  assert.match(reference.examples.startBroker, /--config /);
});
