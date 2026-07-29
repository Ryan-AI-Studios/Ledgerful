"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const { resolveTarget } = require("../lib/platform");
const { packageVersion, parseChecksum, releaseBaseUrl } = require("../lib/install");
const pkg = require("../package.json");

test("maps supported platforms to release assets", () => {
  assert.equal(resolveTarget("linux", "x64").archive, "ledgerful-x86_64-unknown-linux-gnu.tar.gz");
  assert.equal(resolveTarget("win32", "x64").archive, "ledgerful-x86_64-pc-windows-msvc.zip");
  assert.equal(resolveTarget("darwin", "x64").archive, "ledgerful-x86_64-apple-darwin.tar.gz");
  assert.equal(resolveTarget("darwin", "arm64").archive, "ledgerful-aarch64-apple-darwin.tar.gz");
});

test("rejects unsupported platforms clearly", () => {
  assert.throws(() => resolveTarget("linux", "arm64"), /unsupported platform linux:arm64/);
});

test("parses sha256 checksum files", () => {
  const digest = "a".repeat(64);
  assert.equal(parseChecksum(`${digest}  ledgerful-x86_64-unknown-linux-gnu.tar.gz\n`), digest);
});

// Literal expected tag — must not be derived from pkg.ledgerfulEngineTag (that was a
// tautology that passed for any pin value, including the stale v0.1.9 for three releases).
// Update this when cutting a release (Gate A also checks the pin against the tag).
const EXPECTED_ENGINE_TAG = "v0.2.3";

test("defaults release downloads to the engine release tag", () => {
  const previousTag = process.env.LEDGERFUL_MCP_RELEASE_TAG;
  const previousBase = process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  delete process.env.LEDGERFUL_MCP_RELEASE_TAG;
  delete process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  try {
    assert.equal(
      releaseBaseUrl(),
      `https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/${EXPECTED_ENGINE_TAG}`
    );
  } finally {
    if (previousTag === undefined) {
      delete process.env.LEDGERFUL_MCP_RELEASE_TAG;
    } else {
      process.env.LEDGERFUL_MCP_RELEASE_TAG = previousTag;
    }
    if (previousBase === undefined) {
      delete process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
    } else {
      process.env.LEDGERFUL_MCP_RELEASE_BASE_URL = previousBase;
    }
  }
});

test("ledgerfulEngineTag equals literal expected engine tag", () => {
  assert.equal(
    pkg.ledgerfulEngineTag,
    EXPECTED_ENGINE_TAG,
    `ledgerfulEngineTag must be ${EXPECTED_ENGINE_TAG} (got ${pkg.ledgerfulEngineTag})`
  );
});

test("ledgerfulEngineTag matches Cargo.toml package version", () => {
  const cargoToml = fs.readFileSync(path.join(__dirname, "..", "..", "Cargo.toml"), "utf8");
  const match = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
  assert.ok(match, "Cargo.toml must have a package version line");
  const expected = `v${match[1]}`;
  assert.equal(
    pkg.ledgerfulEngineTag,
    expected,
    `ledgerfulEngineTag (${pkg.ledgerfulEngineTag}) must equal v + Cargo.toml version (${expected})`
  );
});
