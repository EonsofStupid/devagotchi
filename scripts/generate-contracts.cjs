#!/usr/bin/env node
"use strict";

// Rust DTOs -> one Schemars root/$defs graph -> generated TypeScript.
// --check regenerates in memory and fails drift without rewriting artifacts.
const fs = require("node:fs/promises");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const root = path.resolve(__dirname, "..");
const schemaFile = path.join(root, "schemas", "broker-contracts.schema.json");
const typesFile = path.join(root, "apps", "desktop", "src", "generated", "contracts.ts");
const expectedTypes = [
  "BridgeError", "GetInput", "ListInput", "OperationState", "OperationSummary",
  "OperationsPage", "OperationView", "OutputFrame", "OutputPage", "ReadInput",
  "StartInput", "StartOutput",
];

function exportFreshSchema() {
  const cargo = process.env.CARGO || "cargo";
  const result = spawnSync(cargo, ["run", "--quiet", "--locked", "--offline", "--bin", "export-contracts"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 120_000,
    shell: false,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`Rust contract export failed. Ensure Cargo and the locked dependencies are installed.\n${result.error?.message || result.stderr || `exit ${result.status}`}`);
  }
  const schema = JSON.parse(result.stdout);
  if (schema.title !== "BrokerContracts" || !schema.$defs) {
    throw new Error("Exporter did not return the expected BrokerContracts root and shared $defs.");
  }
  for (const name of expectedTypes) {
    if (!Object.hasOwn(schema.$defs, name)) {
      throw new Error(`Canonical schema is missing ${name}; repair the Rust export root.`);
    }
  }
  return { text: result.stdout, schema };
}

async function compare(file, expected) {
  let actual;
  try {
    actual = await fs.readFile(file, "utf8");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return `${path.relative(root, file)} is missing`;
  }
  return actual === expected ? null : `${path.relative(root, file)} differs from fresh canonical generation`;
}

async function main() {
  const cliArgs = process.argv.slice(2);
  if (cliArgs.length === 1 && ["--help", "-h"].includes(cliArgs[0])) {
    process.stdout.write("Usage: node scripts/generate-contracts.cjs [--check]\nRegenerates the JSON schema and desktop TypeScript from Rust; --check detects drift without writing either artifact.\n");
    return;
  }
  if (cliArgs.length > 1 || (cliArgs.length === 1 && cliArgs[0] !== "--check")) {
    throw new Error("Usage: node scripts/generate-contracts.cjs [--check]");
  }
  const checking = cliArgs[0] === "--check";
  let compiler;
  try {
    compiler = require(require.resolve("json-schema-to-typescript", { paths: [path.join(root, "apps", "desktop")] }));
  } catch (error) {
    throw new Error(`Cannot load json-schema-to-typescript. Install the desktop's locked dev dependencies first.\n${error.message}`);
  }
  const exported = exportFreshSchema();
  const types = await compiler.compile(exported.schema, "BrokerContracts", {
    cwd: path.join(root, "schemas"),
    $refOptions: { resolve: { external: false } },
    bannerComment: "/**\n * Generated from canonical Rust DTOs in src/contracts.rs. DO NOT EDIT.\n * Generator: node scripts/generate-contracts.cjs\n * Drift check: node scripts/generate-contracts.cjs --check\n * JSON Schema: schemas/broker-contracts.schema.json\n * Broker validation enforces domain bounds beyond these structural types.\n */",
    declareExternallyReferenced: true,
    inferStringEnumKeysFromValues: false,
    enableConstEnums: false,
    unknownAny: true,
    strictIndexSignatures: true,
    additionalProperties: false,
    format: true,
    style: { endOfLine: "lf", semi: true, singleQuote: false, tabWidth: 2, trailingComma: "none", printWidth: 100, useTabs: false },
  });
  for (const name of expectedTypes) {
    const declarations = types.match(new RegExp(`export (?:interface|type) ${name}\\b`, "g")) || [];
    if (declarations.length !== 1) {
      throw new Error(`Expected one exported ${name} declaration; generator produced ${declarations.length}.`);
    }
  }
  if (!/export type OperationState\s*=/.test(types)) {
    throw new Error("OperationState must remain a generated string union, not a runtime enum.");
  }
  if (checking) {
    const differences = (await Promise.all([compare(schemaFile, exported.text), compare(typesFile, types)])).filter(Boolean);
    if (differences.length) {
      throw new Error(`Contract drift detected:\n${differences.map((difference) => `- ${difference}`).join("\n")}\nRun node scripts/generate-contracts.cjs and review the canonical change.`);
    }
    process.stdout.write("Canonical Rust schema and generated desktop contracts are current.\n");
    return;
  }
  await fs.mkdir(path.dirname(schemaFile), { recursive: true });
  await fs.mkdir(path.dirname(typesFile), { recursive: true });
  await fs.writeFile(schemaFile, exported.text);
  await fs.writeFile(typesFile, types);
  process.stdout.write("Generated schemas/broker-contracts.schema.json and apps/desktop/src/generated/contracts.ts.\n");
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
