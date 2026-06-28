"use strict";

const TARGETS = {
  "linux:x64": {
    asset: "ledgerful-x86_64-unknown-linux-gnu",
    extension: ".tar.gz",
    binary: "ledgerful"
  },
  "win32:x64": {
    asset: "ledgerful-x86_64-pc-windows-msvc",
    extension: ".zip",
    binary: "ledgerful.exe"
  },
  "darwin:x64": {
    asset: "ledgerful-x86_64-apple-darwin",
    extension: ".tar.gz",
    binary: "ledgerful"
  },
  "darwin:arm64": {
    asset: "ledgerful-aarch64-apple-darwin",
    extension: ".tar.gz",
    binary: "ledgerful"
  }
};

function resolveTarget(platform = process.platform, arch = process.arch) {
  const key = `${platform}:${arch}`;
  const target = TARGETS[key];
  if (!target) {
    const supported = Object.keys(TARGETS).join(", ");
    throw new Error(`unsupported platform ${key}; supported targets: ${supported}`);
  }
  return { ...target, key, archive: `${target.asset}${target.extension}` };
}

module.exports = { TARGETS, resolveTarget };
