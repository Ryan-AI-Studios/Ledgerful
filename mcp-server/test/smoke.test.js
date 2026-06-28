"use strict";

const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const { runSmoke } = require("./smoke-lib");

test("loader performs initialize and tools/list round trip with local binary override", async (t) => {
  if (!process.env.LEDGERFUL_MCP_BIN_OVERRIDE) {
    const suffix = process.platform === "win32" ? ".exe" : "";
    const localBinary = path.resolve(__dirname, "..", "..", "target", "debug", `ledgerful${suffix}`);
    if (!fs.existsSync(localBinary)) {
      t.skip(`local binary not built: ${localBinary}`);
      return;
    }
    process.env.LEDGERFUL_MCP_BIN_OVERRIDE = localBinary;
  }
  await runSmoke();
});
