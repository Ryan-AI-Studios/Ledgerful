#!/usr/bin/env node
"use strict";

const { spawn } = require("node:child_process");
const { ensureBinary } = require("../lib/install");

async function main() {
  const binary = await ensureBinary({ quiet: false });
  const child = spawn(binary, ["mcp", ...process.argv.slice(2)], {
    stdio: "inherit",
    windowsHide: true
  });

  child.on("error", (error) => {
    console.error(`ledgerful-mcp: failed to launch ${binary}: ${error.message}`);
    process.exitCode = 1;
  });

  child.on("exit", (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 1);
  });
}

main().catch((error) => {
  console.error(`ledgerful-mcp: ${error.message}`);
  process.exit(1);
});
