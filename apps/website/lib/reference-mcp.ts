import { capabilities, documentation, documentationTopics, MCP_VERSION, REFERENCE_VERSION, referenceTools, SITE_ORIGIN, type DocumentationTopic } from "./devagotchi-reference";

type RecordValue = Record<string, unknown>;
type RequestId = string | number;
const MAX_REQUEST_BYTES = 64 * 1024;
const META_PREFIX = "io.modelcontextprotocol/";
const origins = new Set([SITE_ORIGIN, "https://devagotchi.net"]);
const serverInfo = { name: "devagotchi-reference", version: REFERENCE_VERSION };

function record(value: unknown): value is RecordValue {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function response(value: unknown, status = 200) {
  return Response.json(value, { status, headers: { "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" } });
}

function error(status: number, code: number, message: string, id?: RequestId, data?: unknown) {
  return response({ jsonrpc: "2.0", ...(id === undefined ? {} : { id }), error: { code, message, ...(data === undefined ? {} : { data }) } }, status);
}

function complete(id: RequestId, result: RecordValue) {
  return response({ jsonrpc: "2.0", id, result: { resultType: "complete", ...result, _meta: { [`${META_PREFIX}serverInfo`]: serverInfo } } });
}

function decodeHeader(value: string | null): string | null {
  if (value === null) return null;
  if (value.startsWith("=?base64?") && value.endsWith("?=")) {
    const encoded = value.slice(9, -2);
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) throw new Error("Invalid Base64 header");
    return new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(encoded), c => c.charCodeAt(0)));
  }
  if (!/^[\x20-\x7E\t]*$/.test(value) || value.trim() !== value) throw new Error("Invalid header characters");
  return value;
}

async function readBody(request: Request): Promise<string> {
  const length = request.headers.get("content-length");
  if (length && (!/^\d+$/.test(length) || Number(length) > MAX_REQUEST_BYTES)) throw new RangeError("Request body exceeds 64 KiB");
  if (!request.body) return "";
  const reader = request.body.getReader();
  let total = 0;
  const chunks: Uint8Array[] = [];
  let timeout: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => { timeout = setTimeout(() => reject(new Error("Request body timed out")), 5000); });
  try {
    await Promise.race([deadline, (async () => {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        total += value.byteLength;
        if (total > MAX_REQUEST_BYTES) throw new RangeError("Request body exceeds 64 KiB");
        chunks.push(value);
      }
    })()]);
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } finally {
    clearTimeout(timeout);
    void reader.cancel().catch(() => {});
    try { reader.releaseLock(); } catch { /* A pending read settles when cancellation closes the stream. */ }
  }
}

export async function handleReferenceMcp(request: Request): Promise<Response> {
  const origin = request.headers.get("origin");
  const requestUrl = new URL(request.url);
  const localPreview = ["localhost", "127.0.0.1", "terminal.local"].includes(requestUrl.hostname);
  if (origin && !origins.has(origin) && !(localPreview && origin === requestUrl.origin)) return error(403, -32600, "Origin is not allowed.");
  if (request.method !== "POST") return new Response("This endpoint accepts MCP 2026-07-28 POST requests. See /connect.", { status: 405, headers: { Allow: "POST", "Cache-Control": "no-store" } });
  if (request.headers.get("content-type")?.split(";")[0].trim().toLowerCase() !== "application/json") return error(415, -32600, "Content-Type must be application/json.");
  const accepts = (request.headers.get("accept") ?? "").split(",").map(v => v.split(";")[0].trim().toLowerCase());
  if (!accepts.includes("application/json") || !accepts.includes("text/event-stream")) return error(406, -32600, "Accept must include application/json and text/event-stream.");

  let body: unknown;
  try { body = JSON.parse(await readBody(request)); }
  catch (cause) {
    if (cause instanceof RangeError) return error(413, -32600, "Request body exceeds 64 KiB.");
    if (cause instanceof Error && cause.message === "Request body timed out") return error(408, -32600, "Request body timed out.");
    return error(400, -32700, "Body must be valid UTF-8 JSON.");
  }
  if (!record(body) || body.jsonrpc !== "2.0" || typeof body.method !== "string" || body.method.length > 128 || Object.hasOwn(body, "result") || Object.hasOwn(body, "error")) return error(400, -32600, "Send one JSON-RPC 2.0 request; batch messages are not supported.");
  const id = typeof body.id === "string" && body.id.length <= 256 ? body.id : typeof body.id === "number" && Number.isSafeInteger(body.id) ? body.id : undefined;
  if (id === undefined) return error(400, -32600, "Use a string or integer request ID. This reference server does not accept notifications.");
  if (!record(body.params) || !record(body.params._meta)) return error(400, -32602, "MCP 2026-07-28 requires per-request metadata; initialize is not supported.", id);
  const params = body.params;
  const meta = params._meta as RecordValue;
  const version = meta[`${META_PREFIX}protocolVersion`];
  if (typeof version !== "string" || !record(meta[`${META_PREFIX}clientCapabilities`])) return error(400, -32602, "Include protocolVersion and clientCapabilities in params._meta.", id);
  const clientInfo = meta[`${META_PREFIX}clientInfo`];
  if (clientInfo !== undefined && (!record(clientInfo) || typeof clientInfo.name !== "string" || typeof clientInfo.version !== "string")) return error(400, -32602, "clientInfo must contain a name and version when supplied.", id);
  if (request.headers.get("MCP-Protocol-Version") !== version || request.headers.get("Mcp-Method") !== body.method) return error(400, -32020, "Required version or method headers do not match the request body.", id);
  if (version !== MCP_VERSION) return error(400, -32022, "Unsupported protocol version.", id, { supported: [MCP_VERSION], requested: version });

  if (body.method === "server/discover") return complete(id, { supportedVersions: [MCP_VERSION], capabilities: { tools: {} }, instructions: "Public Devagotchi reference only. These tools cannot run commands, access files, enroll devices, or operate a workspace." });
  if (body.method === "ping") return complete(id, {});
  if (body.method === "tools/list") {
    if (params.cursor !== undefined) return error(400, -32602, "This fixed reference catalog is returned in one page; omit cursor.", id);
    return complete(id, { tools: referenceTools });
  }
  if (body.method !== "tools/call") return error(404, -32601, `Method not supported. Use MCP ${MCP_VERSION} server/discover, tools/list, tools/call, or ping.`, id);
  if (typeof params.name !== "string") return error(400, -32602, "Tool name is required.", id);
  try {
    if (decodeHeader(request.headers.get("Mcp-Name")) !== params.name) return error(400, -32020, "Mcp-Name must match params.name.", id);
  } catch { return error(400, -32020, "Malformed Mcp-Name header.", id); }
  if (!referenceTools.some(tool => tool.name === params.name)) return error(400, -32602, "Unknown reference tool. Call tools/list for the current catalog.", id);
  const args = params.arguments === undefined ? {} : params.arguments;
  if (!record(args)) return error(400, -32602, "Tool arguments must be an object when supplied.", id);
  let result: unknown;
  let failure: string | undefined;
  if (params.name === "devagotchi_get_capabilities") {
    if (Object.keys(args).length) failure = "This tool takes no arguments.";
    else result = capabilities();
  } else if (Object.keys(args).some(key => key !== "topic") || typeof args.topic !== "string" || !documentationTopics.includes(args.topic as DocumentationTopic)) failure = `Choose one topic: ${documentationTopics.join(", ")}.`;
  else result = documentation[args.topic as DocumentationTopic];
  if (failure) return complete(id, { isError: true, content: [{ type: "text", text: failure }] });
  return complete(id, { isError: false, content: [{ type: "text", text: JSON.stringify(result, null, 2) }], structuredContent: result });
}
