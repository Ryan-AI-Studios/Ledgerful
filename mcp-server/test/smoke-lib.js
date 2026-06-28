"use strict";

const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const path = require("node:path");
const { createFrameReader, encodeMessage } = require("./mcp-frame");

const EXPECTED_TOOLS = [
  "scan",
  "search",
  "ask",
  "ledger_status",
  "ledger_search",
  "hotspots",
  "endpoints_changed",
  "security_boundaries",
  "dead_code",
  "verify_plan"
];

function defaultCommand() {
  if (process.env.LEDGERFUL_MCP_COMMAND) {
    return process.env.LEDGERFUL_MCP_COMMAND;
  }
  return process.execPath;
}

function defaultArgs() {
  if (process.env.LEDGERFUL_MCP_COMMAND) {
    return [];
  }
  return [path.join(__dirname, "..", "bin", "ledgerful-mcp.js")];
}

async function runSmoke() {
  const command = defaultCommand();
  const child = spawn(command, defaultArgs(), {
    env: process.env,
    stdio: ["pipe", "pipe", "inherit"],
    shell: process.platform === "win32" && /\.(cmd|bat)$/i.test(command),
    windowsHide: true
  });
  const reader = createFrameReader(child.stdout);

  try {
    child.stdin.write(encodeMessage({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-11-25",
        capabilities: {},
        clientInfo: {
          name: "ledgerful-mcp-smoke",
          version: "1.0.0"
        }
      }
    }));
    const initialized = await reader.nextFrame();
    assert.equal(initialized.jsonrpc, "2.0");
    assert.equal(initialized.id, 1);
    assert.equal(initialized.result.serverInfo.name, "ledgerful");

    child.stdin.write(encodeMessage({
      jsonrpc: "2.0",
      id: 2,
      method: "tools/list",
      params: {}
    }));
    const toolsList = await reader.nextFrame();
    assert.equal(toolsList.jsonrpc, "2.0");
    assert.equal(toolsList.id, 2);
    const names = toolsList.result.tools.map((tool) => tool.name);
    assert.deepEqual(names, EXPECTED_TOOLS);
  } finally {
    child.kill();
  }
}

module.exports = { EXPECTED_TOOLS, runSmoke };
