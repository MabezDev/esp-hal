#!/usr/bin/env node
/**
 * Upload each { name, path } as its own Actions artifact.
 *
 * Env:
 *   ARTIFACTS_JSON       JSON array of { name, path }  (optional if matrix set)
 *   MATRIX_JSON          {"target":[{"soc":"..."},...]}  (optional if artifacts set)
 *   NAME_PREFIX          prefix for matrix mode (e.g. hil-tests)
 *   PATH_ROOT            root for matrix mode (default target/tests)
 *   CONTINUE_ON_MISSING  "true" (default) to skip missing paths
 *   REQUIRE_ANY          "true" to fail if nothing uploaded
 */

import { createRequire } from "node:module";
import { execSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const continueOnMissing = process.env.CONTINUE_ON_MISSING !== "false";
const requireAny = process.env.REQUIRE_ANY === "true";
const pathRoot = process.env.PATH_ROOT || "target/tests";

function itemsFromEnv() {
  const raw = process.env.ARTIFACTS_JSON;
  if (raw && raw.trim() !== "") {
    const items = JSON.parse(raw);
    if (!Array.isArray(items)) throw new Error("artifacts must be a JSON array");
    return items;
  }
  const matrixJson = process.env.MATRIX_JSON;
  const prefix = process.env.NAME_PREFIX;
  if (matrixJson && prefix) {
    const matrix = JSON.parse(matrixJson);
    const targets = matrix.target || [];
    if (!Array.isArray(targets)) throw new Error("matrix.target must be an array");
    return targets.map((t) => ({
      name: `${prefix}-${t.soc}`,
      path: path.join(pathRoot, t.soc),
    }));
  }
  throw new Error("provide artifacts JSON, or matrix + prefix");
}

function collectFiles(dir) {
  const out = [];
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) out.push(...collectFiles(p));
    else if (ent.isFile()) out.push(p);
  }
  return out;
}

function resolveFiles(p) {
  if (!fs.existsSync(p)) return null;
  const st = fs.statSync(p);
  if (st.isFile()) return { root: path.dirname(p), files: [p] };
  if (st.isDirectory()) {
    const files = collectFiles(p);
    return { root: p, files };
  }
  return null;
}

function loadClient() {
  const cache = path.join(
    process.env.RUNNER_TEMP || os.tmpdir(),
    "upload-named-artifacts-client",
  );
  if (!fs.existsSync(path.join(cache, "node_modules", "@actions", "artifact"))) {
    fs.mkdirSync(cache, { recursive: true });
    execSync(
      `npm install --silent --no-fund --no-audit --prefix "${cache}" @actions/artifact@2`,
      { stdio: "inherit" },
    );
  }
  return createRequire(path.join(cache, "package.json"))("@actions/artifact");
}

const items = itemsFromEnv();

if (!process.env.GITHUB_ACTIONS) {
  for (const item of items) {
    const resolved = resolveFiles(item.path);
    const n = resolved ? resolved.files.length : 0;
    console.log(`${n ? "would upload" : "would skip "} ${item.name} <- ${item.path}`);
  }
  process.exit(0);
}

const { DefaultArtifactClient } = loadClient();
const client = new DefaultArtifactClient();
let uploaded = 0;

for (const item of items) {
  if (!item.name || !item.path) {
    console.error(`invalid entry: ${JSON.stringify(item)}`);
    process.exit(1);
  }
  const resolved = resolveFiles(item.path);
  if (!resolved || resolved.files.length === 0) {
    const msg = `no files at ${item.path} for artifact ${item.name}`;
    if (continueOnMissing) {
      console.log(`skip: ${msg}`);
      continue;
    }
    console.error(`::error::${msg}`);
    process.exit(1);
  }
  console.log(`upload ${item.name} (${resolved.files.length} files from ${item.path})`);
  await client.uploadArtifact(item.name, resolved.files, resolved.root);
  uploaded += 1;
}

console.log(`uploaded ${uploaded}/${items.length} artifact(s)`);
if (requireAny && uploaded === 0) {
  console.error("::error::No artifacts were uploaded");
  process.exit(1);
}
