"use strict";

const { ensureBinary } = require("../lib/install");

if (process.env.LEDGERFUL_MCP_SKIP_DOWNLOAD === "1" || process.env.LEDGERFUL_MCP_BIN_OVERRIDE) {
  process.exit(0);
}

ensureBinary({ quiet: true }).catch((error) => {
  console.warn(`ledgerful-mcp: binary prefetch skipped: ${error.message}`);
  console.warn("ledgerful-mcp: the first run will retry the download unless LEDGERFUL_MCP_BIN_OVERRIDE is set.");
});
