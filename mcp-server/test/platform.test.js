"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const { resolveTarget } = require("../lib/platform");
const { packageVersion, parseChecksum, releaseBaseUrl } = require("../lib/install");

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

test("defaults release downloads to the npm package version tag", () => {
  const previousTag = process.env.LEDGERFUL_MCP_RELEASE_TAG;
  const previousBase = process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  delete process.env.LEDGERFUL_MCP_RELEASE_TAG;
  delete process.env.LEDGERFUL_MCP_RELEASE_BASE_URL;
  try {
    assert.equal(
      releaseBaseUrl(),
      `https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v${packageVersion()}`
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
