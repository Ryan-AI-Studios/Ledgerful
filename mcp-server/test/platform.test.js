"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const { resolveTarget } = require("../lib/platform");
const { parseChecksum, releaseBaseUrl } = require("../lib/install");
const pkg = require("../package.json");

/**
 * External source of truth for the engine release tag: Cargo.toml package version.
 *
 * Do **not** hardcode a second `EXPECTED_ENGINE_TAG = "vX.Y.Z"` here. That third pin
 * was not updated by `prepare-release-cut.sh` (four-file invariant) and failed the
 * v0.2.4 release after Gate A + full multi-OS builds (run 30622446190).
 *
 * Anti-tautology: expected comes from Cargo, not from `pkg.ledgerfulEngineTag` alone
 * (reading the pin and asserting pin === pin passed for stale values historically).
 */
function cargoEngineTag() {
  const cargoToml = fs.readFileSync(path.join(__dirname, "..", "..", "Cargo.toml"), "utf8");
  const match = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
  assert.ok(match, "Cargo.toml must have a package version line");
  return `v${match[1]}`;
}

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

test("ledgerfulEngineTag matches Cargo.toml package version", () => {
  const expected = cargoEngineTag();
  assert.equal(
    pkg.ledgerfulEngineTag,
    expected,
    `ledgerfulEngineTag (${pkg.ledgerfulEngineTag}) must equal v + Cargo.toml version (${expected})`
  );
});

test("defaults release downloads to the Cargo-aligned engine tag", () => {
  const previousTag = process.env.LEDGERFUL_MCP_RELEASE_TAG;
  const previousBase = process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  delete process.env.LEDGERFUL_MCP_RELEASE_TAG;
  delete process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  try {
    const expectedTag = cargoEngineTag();
    // install.js must honor ledgerfulEngineTag; pin must match Cargo (test above).
    // Asserting the URL against Cargo (not a hand-maintained third pin) keeps cuts green.
    assert.equal(
      releaseBaseUrl(),
      `https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/${expectedTag}`
    );
    assert.equal(
      releaseBaseUrl(),
      `https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/${pkg.ledgerfulEngineTag}`,
      "releaseBaseUrl must use package.json ledgerfulEngineTag when env overrides are unset"
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
