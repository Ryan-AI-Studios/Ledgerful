"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnFile } = require("./process");
const { resolveTarget } = require("./platform");

const OWNER = "Ryan-AI-Studios";
const REPO = "Ledgerful";

function packageVersion() {
  const packageJson = require("../package.json");
  return packageJson.version;
}

function engineReleaseTag() {
  const packageJson = require("../package.json");
  return packageJson.ledgerfulEngineTag || `v${packageVersion()}`;
}

function cacheRoot() {
  if (process.env.LEDGERFUL_MCP_CACHE_DIR) {
    return process.env.LEDGERFUL_MCP_CACHE_DIR;
  }
  if (process.platform === "win32" && process.env.LOCALAPPDATA) {
    return path.join(process.env.LOCALAPPDATA, "Ledgerful", "mcp-server");
  }
  if (process.platform === "darwin") {
    return path.join(os.homedir(), "Library", "Caches", "ledgerful-mcp-server");
  }
  return path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"), "ledgerful-mcp-server");
}

function releaseBaseUrl() {
  if (process.env.LEDGERFUL_MCP_RELEASE_BASE_URL) {
    return process.env.LEDGERFUL_MCP_RELEASE_BASE_URL.replace(/\/+$/, "");
  }
  if (process.env.LEDGERFUL_MCP_RELEASE_TAG) {
    return `https://github.com/${OWNER}/${REPO}/releases/download/${process.env.LEDGERFUL_MCP_RELEASE_TAG}`;
  }
  return `https://github.com/${OWNER}/${REPO}/releases/download/${engineReleaseTag()}`;
}

function parseChecksum(text) {
  const match = text.match(/\b([a-fA-F0-9]{64})\b/);
  if (!match) {
    throw new Error("checksum file did not contain a SHA-256 digest");
  }
  return match[1].toLowerCase();
}

function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

async function downloadToFile(url, destination) {
  const response = await fetch(url, {
    headers: {
      "user-agent": `ledgerful-mcp-server/${packageVersion()}`
    }
  });
  if (!response.ok) {
    throw new Error(`download failed for ${url}: HTTP ${response.status}`);
  }
  const buffer = Buffer.from(await response.arrayBuffer());
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.writeFileSync(destination, buffer);
}

function findBinary(root, binaryName) {
  const entries = fs.readdirSync(root, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(root, entry.name);
    if (entry.isDirectory()) {
      const nested = findBinary(fullPath, binaryName);
      if (nested) {
        return nested;
      }
    } else if (entry.name === binaryName) {
      return fullPath;
    }
  }
  return null;
}

async function extractArchive(archivePath, destination, target) {
  fs.rmSync(destination, { recursive: true, force: true });
  fs.mkdirSync(destination, { recursive: true });
  if (target.extension === ".zip") {
    await spawnFile("powershell", [
      "-NoLogo",
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-Command",
      `Expand-Archive -LiteralPath '${archivePath}' -DestinationPath '${destination}' -Force`
    ]);
  } else {
    await spawnFile("tar", ["-xzf", archivePath, "-C", destination]);
  }
}

async function installFromRelease(target, installDir, options = {}) {
  const baseUrl = releaseBaseUrl();
  const archiveUrl = `${baseUrl}/${target.archive}`;
  const checksumUrl = `${archiveUrl}.sha256`;
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "ledgerful-mcp-"));
  try {
    const archivePath = path.join(tempDir, target.archive);
    const checksumPath = `${archivePath}.sha256`;
    await downloadToFile(archiveUrl, archivePath);
    await downloadToFile(checksumUrl, checksumPath);

    const expected = parseChecksum(fs.readFileSync(checksumPath, "utf8"));
    const actual = sha256File(archivePath);
    if (actual !== expected) {
      throw new Error(`checksum mismatch for ${target.archive}: expected ${expected}, got ${actual}`);
    }

    const extractDir = path.join(tempDir, "extract");
    await extractArchive(archivePath, extractDir, target);
    const extractedBinary = findBinary(extractDir, target.binary);
    if (!extractedBinary) {
      throw new Error(`archive ${target.archive} did not contain ${target.binary}`);
    }

    fs.mkdirSync(installDir, { recursive: true });
    const installedBinary = path.join(installDir, target.binary);
    fs.copyFileSync(extractedBinary, installedBinary);
    if (process.platform !== "win32") {
      fs.chmodSync(installedBinary, 0o755);
    }
    return installedBinary;
  } catch (error) {
    if (!options.quiet) {
      console.error(`ledgerful-mcp: ${error.message}`);
    }
    throw error;
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

async function ensureBinary(options = {}) {
  if (process.env.LEDGERFUL_MCP_BIN_OVERRIDE) {
    const override = path.resolve(process.env.LEDGERFUL_MCP_BIN_OVERRIDE);
    if (!fs.existsSync(override)) {
      throw new Error(`LEDGERFUL_MCP_BIN_OVERRIDE does not exist: ${override}`);
    }
    return override;
  }

  const target = resolveTarget();
  const installDir = path.join(cacheRoot(), packageVersion(), target.asset);
  const binaryPath = path.join(installDir, target.binary);
  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }
  return installFromRelease(target, installDir, options);
}

module.exports = {
  cacheRoot,
  ensureBinary,
  installFromRelease,
  packageVersion,
  parseChecksum,
  releaseBaseUrl,
  sha256File
};
