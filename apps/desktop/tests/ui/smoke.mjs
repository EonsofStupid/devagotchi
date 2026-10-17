#!/usr/bin/env node
/** Browser UI checks with explicit injected fixtures; not a native-client test. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const require = createRequire(import.meta.url);
let chromium;
try { ({ chromium } = require("playwright")); }
catch (error) {
  const runtimeModules = process.env.CODEX_PRIMARY_RUNTIME_NODE_MODULES;
  if (!runtimeModules) throw new Error("Install Playwright and its Chromium browser for UI QA, or provide the existing runtime module path.", { cause: error });
  ({ chromium } = require(path.join(runtimeModules, "playwright")));
}
const desktop = fileURLToPath(new URL("../../", import.meta.url));
const output = path.join(desktop, "qa-output");
await mkdir(output, { recursive: true });
const server = spawn(process.execPath, [path.join(desktop, "node_modules/vite/bin/vite.js"), "--config", "vite.qa.config.ts"], { cwd: desktop, stdio: ["ignore", "pipe", "pipe"] });
let serverLog = "";
for (const stream of [server.stdout, server.stderr]) stream.on("data", (chunk) => { serverLog = (serverLog + chunk).slice(-6000); });
let browser;
const checks = [];
const failures = [];
const pageErrors = [];
const base = "http://127.0.0.1:1421";

async function ready() {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (server.exitCode !== null) throw new Error("QA server exited: " + serverLog);
    try { if ((await fetch(base, { signal: AbortSignal.timeout(700) })).ok) return; } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("QA server did not become ready: " + serverLog);
}

async function check(name, fn) {
  try { await fn(); checks.push({ name, passed: true }); }
  catch (error) { failures.push({ name, error: String(error) }); throw error; }
}

try {
  await ready();
  browser = await chromium.launch({ headless: true, args: ["--no-sandbox"] });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 1 });
  page.setDefaultTimeout(7000);
  await page.emulateMedia({ reducedMotion: "reduce" });
  page.on("pageerror", (error) => pageErrors.push(String(error)));
  const fixture = async (scenario) => {
    await page.goto(base + "/tests/ui/fixture.html?scenario=" + scenario);
    await page.getByText("TEST FIXTURE", { exact: true }).waitFor();
  };

  await check("product browser reports missing native host without invented activity", async () => {
    await page.goto(base);
    await page.locator(".connection-pill.offline").waitFor();
    assert.equal(await page.locator(".command-row").count(), 0);
    await page.getByRole("button", { name: "Connection", exact: true }).click();
    await page.getByRole("heading", { name: "Open the desktop app." }).waitFor();
    assert.match(await page.getByRole("alert").innerText(), /browser page has no native connection/);
    await page.screenshot({ path: path.join(output, "browser-offline.png") });
  });

  await check("fixture command details and process output remain literal text", async () => {
    await fixture("activity");
    await page.locator(".connection-pill.online").waitFor();
    await page.locator(".console-output").waitFor();
    assert.equal(await page.locator(".command-row").count(), 5);
    assert.match(await page.locator(".console-output").innerText(), /<img src=x onerror=/);
    assert.match(await page.locator(".console-output").innerText(), /␛\[32m/);
    assert.equal(await page.locator(".console img, .console script, .console a").count(), 0);
    assert.equal(await page.evaluate(() => window.fixtureExecuted), undefined);
    assert.match(await page.locator(".invocation-panel").innerText(), /\/usr\/bin\/python3/);
    await page.screenshot({ path: path.join(output, "fixture-activity.png") });
  });

  await check("workspace and connection navigation show data-backed context", async () => {
    await page.getByRole("button", { name: "Workspaces", exact: true }).click();
    await page.locator(".workspace-card").first().waitFor();
    assert.equal(await page.locator(".workspace-card").count(), 2);
    assert.match(await page.locator(".workspaces-view").innerText(), /observations, not a list of granted access/);
    await page.locator(".workspace-card .text-button").first().click();
    await page.locator(".detail-panel .operation-id-row").waitFor();
    await page.getByRole("button", { name: "Connection", exact: true }).click();
    assert.match(await page.locator(".connection-details").innerText(), /\/tmp\/devagotchi-fixture\/broker.sock/);
  });

  await check("connected empty history is represented as empty", async () => {
    await fixture("empty");
    await page.locator(".connection-pill.online").waitFor();
    await page.getByRole("heading", { name: "Ready when you are." }).waitFor();
    assert.equal(await page.locator(".command-row").count(), 0);
  });

  await check("permission failure surfaces the exact broker error", async () => {
    await fixture("error");
    await page.locator(".connection-pill.offline").waitFor();
    await page.getByRole("button", { name: "Connection", exact: true }).click();
    await page.getByRole("heading", { name: "Unable to connect." }).waitFor();
    assert.match(await page.getByRole("alert").innerText(), /socket permissions/i);
    assert.match(await page.getByRole("alert").innerText(), /not private to the local operator/);
  });

  await check("unknown outcome with incomplete output does not promise future output", async () => {
    await fixture("unknown");
    await page.getByText("Output incomplete", { exact: true }).waitFor();
    await page.getByText("No retained text output; completeness cannot be confirmed.", { exact: true }).waitFor();
    assert.equal(await page.getByRole("button", { name: "Cancel command", exact: true }).count(), 0);
  });

  await check("pending cancellation stays correlated with its selected command", async () => {
    await fixture("cancel");
    await page.getByRole("button", { name: "Cancel command", exact: true }).click();
    await page.getByRole("button", { name: "Requesting…", exact: true }).waitFor();
    await page.locator('[data-operation-id="fixture-23c871ca-schema"]').click();
    await page.getByRole("button", { name: "Cancel command", exact: true }).waitFor();
    assert.equal(await page.getByRole("button", { name: "Requesting…", exact: true }).count(), 0);
    await page.getByRole("button", { name: "Cancel command", exact: true }).click();
    await page.locator(".detail-title-row .state-cancelled").waitFor();
  });

  await check("frontend retention remains bounded while draining long output", async () => {
    await fixture("overflow");
    await page.getByText("Output complete", { exact: true }).waitFor();
    assert.match(await page.locator(".output-notice").innerText(), /most recent retained text/);
    assert.ok(await page.locator(".console-output > span").count() <= 1200);
    assert.ok((await page.locator(".console-output").textContent()).length <= 512 * 1024);
  });

  assert.deepEqual(pageErrors, []);
} catch (error) {
  if (!failures.length) failures.push({ name: "harness", error: String(error) });
} finally {
  if (browser) await browser.close();
  server.kill("SIGTERM");
  await Promise.race([new Promise((resolve) => server.once("exit", resolve)), new Promise((resolve) => setTimeout(resolve, 2000))]);
  if (server.exitCode === null) server.kill("SIGKILL");
  const summary = { scope: "Browser rendering with explicit synthetic-response harness; product no-native state also checked. Not native IPC or named-client certification.", passed: failures.length === 0, checks, failures, page_errors: pageErrors, screenshots: ["fixture-activity.png", "browser-offline.png"] };
  await writeFile(path.join(output, "ui-results.json"), JSON.stringify(summary, null, 2) + "\n");
  process.stdout.write(JSON.stringify(summary, null, 2) + "\n");
  if (failures.length) process.exitCode = 1;
}
