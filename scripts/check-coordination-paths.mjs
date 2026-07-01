#!/usr/bin/env node
// Track 0013 DoD-5: §3.2 ↔ openapi.json consistency guard.
//
// Parses the coordination.md §3.2 endpoint table and asserts every listed
// /api/* path exists as a key in the engine's openapi.json. Runs as a
// pre-push hook (or standalone via `node scripts/check-coordination-paths.mjs`).
//
// Skips with a warning if either file is not found (solo-checkout CI).

import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";

const COORDINATION_MD = resolve(process.cwd(), "..", "coordinated", "coordination.md");
const OPENAPI_JSON = resolve(process.cwd(), "..", "ledgerful", "docs", "api", "openapi.json");

function fail(msg) {
  console.error(`ERROR (coordination guard): ${msg}`);
  process.exit(1);
}

function skip(msg) {
  console.warn(`WARN (coordination guard): ${msg} — skipping.`);
  process.exit(0);
}

if (!existsSync(COORDINATION_MD)) {
  skip(`coordination.md not found at ${COORDINATION_MD}`);
}
if (!existsSync(OPENAPI_JSON)) {
  skip(`openapi.json not found at ${OPENAPI_JSON}`);
}

const md = readFileSync(COORDINATION_MD, "utf-8");
const openapi = JSON.parse(readFileSync(OPENAPI_JSON, "utf-8"));

// Extract §3.2 endpoint table rows. Match lines like:
//   | `GET /api/snapshot` | ... |
const pathRegex = /\|\s*`(?:GET|POST|PUT|DELETE|PATCH)\s+(\/api\/[^`]+)`\s*\|/g;

const expectedPaths = new Set();
let match;
while ((match = pathRegex.exec(md)) !== null) {
  // Only collect paths from the §3.2 table (between "### 3.2" and "### 3.3")
  // Simple heuristic: the regex matches any table row with an /api/ path.
  // We rely on the strikethrough entries (~~...~~) being excluded because
  // they use ~~ delimiters not backticks.
  expectedPaths.add(match[1]);
}

// Filter to only paths from the §3.2 section (after the "### 3.2" heading,
// before the next ### or --- section break)
const section32Start = md.indexOf("### 3.2");
const section32End = md.indexOf("\n---\n", section32Start);
const section32 = md.slice(section32Start, section32End > 0 ? section32End : md.length);

expectedPaths.clear();
while ((match = pathRegex.exec(section32)) !== null) {
  expectedPaths.add(match[1]);
}

if (expectedPaths.size === 0) {
  skip("No /api/ paths found in §3.2 table (parse failure or empty section)");
}

const openapiPaths = new Set(Object.keys(openapi.paths || {}));
const missing = [...expectedPaths].filter((p) => !openapiPaths.has(p));

if (missing.length > 0) {
  fail(
    `§3.2 lists ${expectedPaths.size} /api/ paths; ${missing.length} missing from openapi.json:\n` +
      missing.map((p) => `  - ${p}`).join("\n") +
      `\n\nFix: add utoipa annotations for the missing routes, or update §3.2 in coordination.md.`
  );
}

console.log(`OK: all ${expectedPaths.size} §3.2 paths present in openapi.json`);