"use strict";

const { spawn } = require("node:child_process");

function spawnFile(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: options.stdio || "pipe",
      windowsHide: true,
      ...options
    });
    let stderr = "";
    child.stderr?.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
        return;
      }
      reject(new Error(`${command} exited with ${code}${stderr ? `: ${stderr.trim()}` : ""}`));
    });
  });
}

module.exports = { spawnFile };
