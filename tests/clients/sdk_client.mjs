// The official MCP TypeScript SDK as a client of a hub (tests/clients_oauth.py runs it).
//
// It connects the way an SDK-based app does (Claude Code, MCP Inspector, most editors):
// the 401 from /mcp, the protected-resource and authorization-server metadata, dynamic
// registration, the authorization code with PKCE and `resource`, the token; then
// tools/list, `whoami`, and a refresh the SDK does by itself when the hub refuses a spent
// access token. The person's approval happens in the harness's browser.
//
// Usage: node sdk_client.mjs <hub /mcp URL> <callback port>
// Writes one JSON object per line to stdout, and never a token or a code:
//   {"authorize": "<URL>"}   the person approves here; the harness opens it
//   {"ok": true, ...}        what the SDK saw
import http from "node:http";
import { randomUUID } from "node:crypto";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { UnauthorizedError } from "@modelcontextprotocol/sdk/client/auth.js";

const [mcpUrl, port] = process.argv.slice(2);
const redirect = `http://127.0.0.1:${port}/callback`;
const emit = (value) => process.stdout.write(JSON.stringify(value) + "\n");

class Provider {
  sentState = randomUUID();
  get redirectUrl() {
    return redirect;
  }
  get clientMetadata() {
    return {
      client_name: "MCP TypeScript SDK probe",
      redirect_uris: [redirect],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      token_endpoint_auth_method: "none",
    };
  }
  state() {
    return this.sentState;
  }
  clientInformation() {
    return this.info;
  }
  saveClientInformation(info) {
    this.info = info;
  }
  tokens() {
    return this.saved;
  }
  saveTokens(tokens) {
    this.saved = tokens;
    this.saves = (this.saves ?? 0) + 1;
  }
  redirectToAuthorization(url) {
    emit({ authorize: url.href });
  }
  saveCodeVerifier(verifier) {
    this.verifier = verifier;
  }
  codeVerifier() {
    return this.verifier;
  }
}

// The redirect lands here once, with the code (or the refusal).
function callback() {
  return new Promise((resolve, reject) => {
    const server = http.createServer((request, response) => {
      const url = new URL(request.url, redirect);
      if (url.pathname !== "/callback") {
        response.writeHead(404).end();
        return;
      }
      response.writeHead(200, { "content-type": "text/plain" }).end("You can close this tab.");
      server.close();
      resolve(url.searchParams);
    });
    server.on("error", reject);
    server.listen(Number(port), "127.0.0.1");
  });
}

async function connected(provider) {
  const client = new Client({ name: "devagotchi-sdk-probe", version: "1.0.0" });
  await client.connect(new StreamableHTTPClientTransport(new URL(mcpUrl), { authProvider: provider }));
  return client;
}

const answered = callback();
const provider = new Provider();
const transport = new StreamableHTTPClientTransport(new URL(mcpUrl), { authProvider: provider });
try {
  await new Client({ name: "devagotchi-sdk-probe", version: "1.0.0" }).connect(transport);
  throw new Error("the hub let the SDK in without signing in");
} catch (error) {
  if (!(error instanceof UnauthorizedError)) throw error;
}
const params = await answered;
if (params.get("error")) throw new Error(`the hub refused: ${params.get("error")}`);
if (params.get("state") !== provider.sentState) throw new Error("state did not come back");
const issuer = params.get("iss");
await transport.finishAuth(params.get("code"));

const client = await connected(provider);
const tools = (await client.listTools()).tools.map((tool) => tool.name);
const whoami = (await client.callTool({ name: "whoami", arguments: {} })).structuredContent;

// A spent access token: the SDK refreshes by itself on the hub's 401 and goes on.
const saves = provider.saves;
provider.saved = { ...provider.saved, access_token: "dgh_spent" };
const again = await connected(provider);
const machines = (await again.callTool({ name: "machines", arguments: {} })).structuredContent;
await again.close();
await client.close();

emit({
  ok: true,
  client_id_prefix: String(provider.info?.client_id ?? "").slice(0, 4),
  iss: issuer,
  scope: provider.saved.scope,
  refreshed: provider.saves > saves,
  tools,
  whoami: {
    owner: whoami?.owner,
    via: whoami?.via,
    subject: Boolean(whoami?.subject),
    // A hosted hub: the tenant the account was made in, and whether it is not secured yet.
    tenant: whoami?.tenant ?? null,
    unsecured: Boolean(whoami?.secure_url),
  },
  machines: Array.isArray(machines?.machines),
});
process.exit(0);
