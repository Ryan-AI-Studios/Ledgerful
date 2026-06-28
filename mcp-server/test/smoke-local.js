"use strict";

const path = require("node:path");
const { runSmoke } = require("./smoke-lib");

if (!process.env.LEDGERFUL_MCP_BIN_OVERRIDE) {
  const suffix = process.platform === "win32" ? ".exe" : "";
  process.env.LEDGERFUL_MCP_BIN_OVERRIDE = path.resolve(__dirname, "..", "..", "target", "debug", `ledgerful${suffix}`);
}

runSmoke().catch((error) => {
  console.error(error);
  process.exit(1);
});
