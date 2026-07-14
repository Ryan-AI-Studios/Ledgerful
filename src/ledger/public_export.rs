//! Public ledger bundle export.
//!
//! Exports the engine's own signed ledger entries as a static, redaction-controlled,
//! cryptographically verifiable bundle suitable for publication on the web.
//!
//! The bundle is written to the caller-supplied output directory and contains:
//! - `manifest.json` — publisher identity, entry count, time range, signature metadata,
//!   and (when present) the signed chain head.
//! - `entries.ndjson` — one JSON object per line, allowlist-applied fields only.
//! - `index.html` — static, no-JS browse page.
//! - `verifier.html` — standalone offline WebCrypto verifier.
//! - `README.md` — explains the bundle, verification, allowlist, and honest ceiling.
//!
//! The manifest is signed by a dedicated bot keypair (`ledgerful-ledger-bot`),
//! separate from the engine's main signing key. Entry signatures are read-only and
//! are not modified by this export.

use crate::commands::helpers::get_layout;
use crate::ledger::crypto::compute_entry_hash;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{ChainHead, LedgerEntry, VerificationStatus};
use crate::state::storage::StorageManager;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use hmac::{Hmac, KeyInit, Mac};
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::OwoColorize;
use rand::rngs::OsRng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const PUBLISHER: &str = "ledgerful-ledger-bot";
const SIGNATURE_ALGORITHM: &str = "Ed25519";
const ALLOWLIST_VERSION: u32 = 1;
const BOT_KEY_SEED_FILE: &str = "ledgerful-ledger-bot.key";
const BOT_KEY_PUB_FILE: &str = "ledgerful-ledger-bot.pub";
const PSEUDONYM_SECRET_FILE: &str = "pseudonym-secret.key";

/// Bot-specific errors surfaced to users.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
enum PublicExportError {
    #[error("failed to write {file}: {detail}")]
    WriteBundleFile { file: String, detail: String },
    #[error("failed to serialize {file}: {detail}")]
    Serialize { file: String, detail: String },
    #[error("bot key operation failed: {detail}")]
    BotKey { detail: String },
}

impl PublicExportError {
    fn write(file: impl Into<String>, detail: impl std::fmt::Display) -> Self {
        Self::WriteBundleFile {
            file: file.into(),
            detail: detail.to_string(),
        }
    }

    fn serialize(file: impl Into<String>, detail: impl std::fmt::Display) -> Self {
        Self::Serialize {
            file: file.into(),
            detail: detail.to_string(),
        }
    }
}

/// Manifest JSON payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    publisher: String,
    generated_at: String,
    entry_count: u64,
    time_range: Option<TimeRange>,
    signature_algorithm: String,
    signature: Option<String>,
    public_key: Option<String>,
    public_key_fingerprint: Option<String>,
    chain_head: Option<ChainHead>,
    allowlist_version: u32,
    honest_ceiling: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TimeRange {
    earliest: String,
    latest: String,
}

/// Public bundle export options.
pub struct ExportOptions<'a> {
    pub output: &'a Path,
    pub sign: bool,
    pub key: Option<&'a Path>,
}

/// Generate the public ledger bundle.
pub fn export_public_bundle(options: ExportOptions<'_>) -> Result<()> {
    let layout = get_layout()?;

    let output = options.output;
    if !output.exists() {
        fs::create_dir_all(output)
            .map_err(|e| miette!("Failed to create output directory: {e}"))?;
    }

    // Read-only access to ledger state. This export never writes ledger entries.
    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let db = LedgerDb::new(storage.get_connection());

    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette!("Failed to read ledger entries: {e}"))?;
    let chain_head = db
        .get_chain_head()
        .map_err(|e| miette!("Failed to read chain head: {e}"))?;

    let keys_dir = options
        .key
        .map(PathBuf::from)
        .or_else(get_bot_keys_dir)
        .ok_or_else(|| miette!("Failed to determine keys directory"))?;
    // The pseudonym secret is always needed (pseudonyms are published in
    // entries.ndjson regardless of --sign), so we create/load it unconditionally.
    let pseudonym_secret = get_or_create_pseudonym_secret(&keys_dir)?;

    let public_entries: Vec<PublicEntry> = entries
        .iter()
        .map(|entry| PublicEntry::from_ledger_entry(entry, &pseudonym_secret))
        .collect::<Result<Vec<_>>>()?;

    let entries_ndjson = build_entries_ndjson(&public_entries)?;
    write_bundle_file(output, "entries.ndjson", &entries_ndjson)?;

    let time_range = compute_time_range(&public_entries);

    let generated_at = entries
        .last()
        .map(|e| e.committed_at.clone())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    let (_signature, _public_key, _public_key_fingerprint) = if options.sign {
        let (signing_key, verifying_key) = get_or_create_bot_keys_in(&keys_dir).map_err(|e| {
            miette!(
                "{}",
                PublicExportError::BotKey {
                    detail: e.to_string(),
                }
            )
        })?;

        let manifest_for_signing = Manifest {
            publisher: PUBLISHER.to_string(),
            generated_at: generated_at.clone(),
            entry_count: public_entries.len() as u64,
            time_range: time_range.clone(),
            signature_algorithm: SIGNATURE_ALGORITHM.to_string(),
            signature: None,
            public_key: None,
            public_key_fingerprint: None,
            chain_head: chain_head.clone(),
            allowlist_version: ALLOWLIST_VERSION,
            honest_ceiling: honest_ceiling_text(),
        };

        let manifest_json = serialize_manifest(&manifest_for_signing)?;
        let sig = signing_key.sign(&manifest_json);
        let pub_bytes = verifying_key.to_bytes();
        let fingerprint = sha256_fingerprint(&pub_bytes);

        let signature = Some(hex::encode(sig.to_bytes()));
        let public_key = Some(hex::encode(pub_bytes));
        let public_key_fingerprint = Some(format!("sha256:{fingerprint}"));

        write_bundle_file(output, "manifest.sig", sig.to_bytes().as_slice())?;
        write_bundle_file(output, "manifest.pub", pub_bytes.as_slice())?;

        let manifest = Manifest {
            signature: signature.clone(),
            public_key: public_key.clone(),
            public_key_fingerprint: public_key_fingerprint.clone(),
            ..manifest_for_signing
        };
        let manifest_json = serialize_manifest(&manifest)?;
        write_bundle_file(output, "manifest.json", &manifest_json)?;

        (signature, public_key, public_key_fingerprint)
    } else {
        let manifest = Manifest {
            publisher: PUBLISHER.to_string(),
            generated_at,
            entry_count: public_entries.len() as u64,
            time_range,
            signature_algorithm: SIGNATURE_ALGORITHM.to_string(),
            signature: None,
            public_key: None,
            public_key_fingerprint: None,
            chain_head,
            allowlist_version: ALLOWLIST_VERSION,
            honest_ceiling: honest_ceiling_text(),
        };
        let manifest_json = serialize_manifest(&manifest)?;
        write_bundle_file(output, "manifest.json", &manifest_json)?;
        (None, None, None)
    };

    let index_html = build_index_html(&public_entries);
    write_bundle_file(output, "index.html", index_html.as_bytes())?;

    let verifier_html = build_verifier_html();
    write_bundle_file(output, "verifier.html", verifier_html.as_bytes())?;

    let readme = build_readme(options.sign);
    write_bundle_file(output, "README.md", readme.as_bytes())?;

    println!(
        "{} Public ledger bundle exported to {}",
        "SUCCESS:".green().bold(),
        output.display()
    );
    println!(
        "  entries={}, signed={}",
        public_entries.len(),
        options.sign
    );

    Ok(())
}

fn serialize_manifest(manifest: &Manifest) -> Result<Vec<u8>> {
    serde_json::to_vec(manifest)
        .map_err(|e| miette!("{}", PublicExportError::serialize("manifest.json", e)))
}

fn write_bundle_file(output: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = output.join(name);
    fs::write(&path, bytes).map_err(|e| miette!("{}", PublicExportError::write(name, e)))
}

fn compute_time_range(entries: &[PublicEntry]) -> Option<TimeRange> {
    if entries.is_empty() {
        return None;
    }
    let earliest = entries.first()?.committed_at.clone();
    let latest = entries.last()?.committed_at.clone();
    Some(TimeRange { earliest, latest })
}

fn sha256_fingerprint(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn get_bot_keys_dir() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(PathBuf::from(home).join(".ledgerful").join("keys"))
}

fn get_or_create_bot_keys_in(keys_dir: &Path) -> Result<(SigningKey, VerifyingKey)> {
    if !keys_dir.exists() {
        fs::create_dir_all(keys_dir).into_diagnostic()?;
    }

    let priv_path = keys_dir.join(BOT_KEY_SEED_FILE);
    let pub_path = keys_dir.join(BOT_KEY_PUB_FILE);

    if priv_path.exists() {
        let priv_str = fs::read_to_string(&priv_path).into_diagnostic()?;
        let priv_bytes = hex::decode(priv_str.trim()).into_diagnostic()?;
        let priv_array: [u8; 32] = priv_bytes
            .try_into()
            .map_err(|_| miette!("Invalid bot private key size"))?;
        let signing_key = SigningKey::from_bytes(&priv_array);
        let verifying_key = if pub_path.exists() {
            let pub_str = fs::read_to_string(&pub_path).into_diagnostic()?;
            let pub_bytes = hex::decode(pub_str.trim()).into_diagnostic()?;
            let pub_array: [u8; 32] = pub_bytes
                .try_into()
                .map_err(|_| miette!("Invalid bot public key size"))?;
            VerifyingKey::from_bytes(&pub_array).into_diagnostic()?
        } else {
            let vk = signing_key.verifying_key();
            fs::write(&pub_path, hex::encode(vk.to_bytes())).into_diagnostic()?;
            vk
        };

        if signing_key.verifying_key().to_bytes() != verifying_key.to_bytes() {
            return Err(miette!(
                "Bot keypair mismatch: public key does not match private seed"
            ));
        }

        Ok((signing_key, verifying_key))
    } else {
        let mut csprng = OsRng;
        let mut seed = [0u8; 32];
        use rand::RngCore;
        csprng.fill_bytes(&mut seed);
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();

        fs::write(&priv_path, hex::encode(signing_key.to_bytes())).into_diagnostic()?;
        fs::write(&pub_path, hex::encode(verifying_key.to_bytes())).into_diagnostic()?;

        Ok((signing_key, verifying_key))
    }
}

fn get_or_create_pseudonym_secret(keys_dir: &Path) -> Result<Vec<u8>> {
    if !keys_dir.exists() {
        fs::create_dir_all(keys_dir).into_diagnostic()?;
    }

    let secret_path = keys_dir.join(PSEUDONYM_SECRET_FILE);
    if secret_path.exists() {
        fs::read(&secret_path).into_diagnostic()
    } else {
        let mut csprng = OsRng;
        let mut secret = [0u8; 32];
        use rand::RngCore;
        csprng.fill_bytes(&mut secret);
        fs::write(&secret_path, secret.as_slice()).into_diagnostic()?;
        Ok(secret.to_vec())
    }
}

/// One published entry in `entries.ndjson`.
///
/// Fields are limited to the allowlist and serialized in alphabetical order for
/// determinism.
#[derive(Debug, Serialize)]
struct PublicEntry {
    author_pseudonym: String,
    category: String,
    committed_at: String,
    entry_hash: String,
    public_key: Option<String>,
    reason: String,
    risk_level: Option<String>,
    signature: Option<String>,
    summary: String,
    tx_id: String,
    verification_result: Option<String>,
}

impl PublicEntry {
    fn from_ledger_entry(entry: &LedgerEntry, pseudonym_secret: &[u8]) -> Result<Self> {
        let author_pseudonym = compute_author_pseudonym(pseudonym_secret, &entry.author)?;
        let verification_result = entry.verification_status.map(|status| match status {
            VerificationStatus::Verified => "PASS".to_string(),
            VerificationStatus::PartiallyVerified => "PARTIAL".to_string(),
            VerificationStatus::Failed => "FAIL".to_string(),
            VerificationStatus::Unverified => "UNVERIFIED".to_string(),
        });

        Ok(Self {
            author_pseudonym,
            category: entry.category.to_string(),
            committed_at: entry.committed_at.clone(),
            entry_hash: compute_entry_hash(
                &entry.tx_id,
                entry.signature.as_deref().unwrap_or(""),
                entry.prev_hash.as_deref().unwrap_or(""),
            ),
            public_key: entry.public_key.clone(),
            reason: entry.reason.clone(),
            risk_level: entry.risk.clone(),
            signature: entry.signature.clone(),
            summary: entry.summary.clone(),
            tx_id: entry.tx_id.clone(),
            verification_result,
        })
    }

    /// Serialize the public entry as an alphabetically-keyed JSON object.
    ///
    /// Using a `BTreeMap` guarantees stable key ordering, which keeps
    /// `entries.ndjson` byte-identical for the same ledger state.
    fn to_json_value(&self) -> BTreeMap<String, serde_json::Value> {
        let mut map = BTreeMap::new();
        map.insert(
            "author_pseudonym".to_string(),
            self.author_pseudonym.clone().into(),
        );
        map.insert("category".to_string(), self.category.clone().into());
        map.insert("committed_at".to_string(), self.committed_at.clone().into());
        map.insert("entry_hash".to_string(), self.entry_hash.clone().into());
        map.insert("public_key".to_string(), self.public_key.clone().into());
        map.insert("reason".to_string(), self.reason.clone().into());
        map.insert("risk_level".to_string(), self.risk_level.clone().into());
        map.insert("signature".to_string(), self.signature.clone().into());
        map.insert("summary".to_string(), self.summary.clone().into());
        map.insert("tx_id".to_string(), self.tx_id.clone().into());
        map.insert(
            "verification_result".to_string(),
            self.verification_result.clone().into(),
        );
        map
    }
}

pub fn compute_author_pseudonym(secret: &[u8], author: &str) -> Result<String> {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| miette!("invalid HMAC key length"))?;
    mac.update(author.as_bytes());
    let result = mac.finalize();
    Ok(hex::encode(result.into_bytes()))
}

fn build_entries_ndjson(entries: &[PublicEntry]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    for entry in entries {
        let value = entry.to_json_value();
        let line = serde_json::to_vec(&value)
            .map_err(|e| miette!("{}", PublicExportError::serialize("entries.ndjson", e)))?;
        buf.extend_from_slice(&line);
        buf.push(b'\n');
    }
    Ok(buf)
}

fn honest_ceiling_text() -> String {
    "This bundle proves each entry's Ed25519 signature and the manifest signature. It does not prove the order or set of entries (that's the chain head) or the identity behind the key (out-of-band fingerprint comparison).".to_string()
}

fn build_index_html(entries: &[PublicEntry]) -> String {
    let rows: String = entries
        .iter()
        .map(|entry| {
            format!(
                "<tr>\n<td>{tx_id}</td>\n<td>{category}</td>\n<td>{summary}</td>\n<td>{committed_at}</td>\n<td>{author}</td>\n<td>{risk}</td>\n</tr>\n",
                tx_id = html_escape(&entry.tx_id),
                category = html_escape(&entry.category),
                summary = html_escape(&entry.summary),
                committed_at = html_escape(&entry.committed_at),
                author = html_escape(&entry.author_pseudonym),
                risk = html_escape(entry.risk_level.as_deref().unwrap_or("—")),
            )
        })
        .collect();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Public Ledger</title>
<style>
body {{ font-family: system-ui, -apple-system, sans-serif; margin: 2rem; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ border: 1px solid #ccc; padding: 0.5rem; text-align: left; }}
th {{ background: #f5f5f5; }}
tr:nth-child(even) {{ background: #fafafa; }}
</style>
</head>
<body>
<h1>Public Ledger</h1>
<p><a href="verifier.html">Offline verifier</a></p>
<table>
<thead>
<tr><th>tx_id</th><th>category</th><th>summary</th><th>committed_at</th><th>author_pseudonym</th><th>risk_level</th></tr>
</thead>
<tbody>
{rows}
</tbody>
</table>
</body>
</html>"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn build_verifier_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Ledgerful Public Ledger Verifier</title>
<style>
body { font-family: system-ui, -apple-system, sans-serif; margin: 2rem; }
#status { font-weight: bold; }
.valid { color: green; }
.invalid { color: red; }
.unsigned { color: gray; }
pre { background: #f5f5f5; padding: 1rem; overflow-x: auto; }
table { border-collapse: collapse; width: 100%; }
th, td { border: 1px solid #ccc; padding: 0.5rem; text-align: left; }
</style>
</head>
<body>
<h1>Ledgerful Public Ledger Verifier</h1>
<p>This page verifies the bundle's manifest signature and each entry's Ed25519 signature using WebCrypto. It works offline; no external resources are loaded.</p>
<p id="status">Loading bundle files...</p>
<div id="results"></div>
<script>
(async function() {
  const statusEl = document.getElementById('status');
  const resultsEl = document.getElementById('results');

  if (!window.crypto || !window.crypto.subtle) {
    statusEl.textContent = 'WebCrypto is unavailable in this browser. Use the CLI verifier instead.';
    return;
  }

  async function loadText(name) {
    const res = await fetch(name);
    if (!res.ok) throw new Error('Failed to load ' + name + ': ' + res.status);
    return res.text();
  }
  async function loadBytes(name) {
    const res = await fetch(name);
    if (!res.ok) throw new Error('Failed to load ' + name + ': ' + res.status);
    const buf = await res.arrayBuffer();
    return new Uint8Array(buf);
  }
  function hexToBytes(hex) {
    const bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < hex.length; i += 2) {
      bytes[i / 2] = parseInt(hex.substr(i, 2), 16);
    }
    return bytes;
  }
  async function importEd25519PublicKey(rawBytes) {
    // Modern browsers support 'Ed25519'; Chromium historically used 'NODE-ED25519'.
    for (const alg of ['Ed25519', 'NODE-ED25519']) {
      try {
        return await window.crypto.subtle.importKey(
          'raw',
          rawBytes,
          { name: alg, namedCurve: alg },
          false,
          ['verify']
        );
      } catch (e) {
        // Try the next algorithm name.
      }
    }
    throw new Error('Ed25519 is not supported by this browser');
  }
  async function verifyEd25519(keyBytes, sigBytes, payloadBytes) {
    const key = await importEd25519PublicKey(keyBytes);
    return await window.crypto.subtle.verify(key.algorithm.name, key, sigBytes, payloadBytes);
  }

  try {
    const manifestText = await loadText('manifest.json');
    const manifest = JSON.parse(manifestText);
    const sigHex = manifest.signature;
    const pubHex = manifest.publicKey;

    let manifestValid = false;
    if (sigHex && pubHex) {
      const sigBytes = hexToBytes(sigHex);
      const pubBytes = hexToBytes(pubHex);
      // The signature covers the manifest with signature fields null.
      const canonical = JSON.parse(JSON.stringify(manifest));
      canonical.signature = null;
      canonical.publicKey = null;
      canonical.publicKeyFingerprint = null;
      const canonicalText = JSON.stringify(canonical);
      manifestValid = await verifyEd25519(pubBytes, sigBytes, new TextEncoder().encode(canonicalText));
    }

    const manifestStatus = document.createElement('p');
    manifestStatus.innerHTML = '<strong>Manifest:</strong> ' + (manifestValid ? '<span class="valid">VALID</span>' : sigHex ? '<span class="invalid">INVALID</span>' : '<span class="unsigned">UNSIGNED</span>');
    resultsEl.appendChild(manifestStatus);

    const entriesText = await loadText('entries.ndjson');
    const lines = entriesText.split('\n').filter(line => line.trim());

    const table = document.createElement('table');
    const thead = document.createElement('thead');
    thead.innerHTML = '<tr><th>tx_id</th><th>category</th><th>summary</th><th>status</th></tr>';
    table.appendChild(thead);
    const tbody = document.createElement('tbody');

    for (const line of lines) {
      const entry = JSON.parse(line);
      const key = entry.public_key ? hexToBytes(entry.public_key) : null;
      const sig = entry.signature ? hexToBytes(entry.signature) : null;
      const basis = `tx_id:${entry.tx_id}\ncategory:${entry.category}\nsummary:${entry.summary}\nreason:${entry.reason}\ncommitted_at:${entry.committed_at}`;
      const payload = new TextEncoder().encode(basis);
      let entryValid = false;
      let label = 'UNSIGNED';
      if (key && sig) {
        try {
          entryValid = await verifyEd25519(key, sig, payload);
          label = entryValid ? 'VALID' : 'INVALID';
        } catch (e) {
          label = 'INVALID';
        }
      }
      const cls = label === 'VALID' ? 'valid' : label === 'INVALID' ? 'invalid' : 'unsigned';
      const tr = document.createElement('tr');
      tr.innerHTML = '<td>' + entry.tx_id + '</td><td>' + entry.category + '</td><td>' + (entry.summary || '') + '</td><td class="' + cls + '">' + label + '</td>';
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    resultsEl.appendChild(table);

    statusEl.textContent = 'Verification complete.';
  } catch (err) {
    statusEl.textContent = 'Verification failed: ' + err.message;
    statusEl.className = 'invalid';
  }
})();
</script>
</body>
</html>"#
    .to_string()
}

fn build_readme(signed: bool) -> String {
    format!(
        r#"# Ledgerful Public Ledger Bundle

This bundle is a redacted, cryptographically verifiable export of the Ledgerful
engine's own signed ledger entries.

## Files

- `manifest.json` — publisher identity, entry count, time range, signature
  metadata, and (if present) the signed chain head.
- `entries.ndjson` — one JSON object per line containing only the allowlisted
  fields.
- `index.html` — static browse page (no JavaScript).
- `verifier.html` — standalone offline WebCrypto verifier. Open this file in a
  modern browser to verify the manifest signature and every entry's Ed25519
  signature without network access.
- `README.md` — this file.

## Allowlist

Each published entry includes only:

- `tx_id`
- `category`
- `summary`
- `reason`
- `committed_at`
- `author_pseudonym` (HMAC-SHA256 keyed hash of the author)
- `verification_result`
- `risk_level`
- `entry_hash`
- `signature`
- `public_key`

The following are intentionally redacted: internal IDs, entry type, entity
path, normalized entity, change type, breaking flag, outcome notes, origin,
trace ID, related tickets, raw author, observed flag, and previous chain hash.

## Verification

1. Open `verifier.html` in a browser, or
2. Use the CLI: `ledgerful ledger export-public --output <dir> --sign`

If signed, the bundle also contains:

- `manifest.sig` — raw Ed25519 signature over `manifest.json`
- `manifest.pub` — raw Ed25519 verifying key

## Honest ceiling

{ceiling}

## Bot key

The manifest is signed by a dedicated `ledgerful-ledger-bot` keypair that is
separate from the engine's main signing key. This keeps bundle-key rotation
independent of the engine's signing identity.

## Signed

{signed}
"#,
        ceiling = honest_ceiling_text(),
        signed = if signed {
            "Yes — `manifest.sig` and `manifest.pub` are present."
        } else {
            "No — this bundle was exported without `--sign`."
        }
    )
}

/// Verify a public bundle's manifest signature using the provided bot public key.
///
/// The signature is computed over the manifest JSON with `signature`,
/// `publicKey`, and `publicKeyFingerprint` set to null, so this helper
/// reconstructs that canonical form before verifying.
pub fn verify_manifest_signature(
    manifest_json: &[u8],
    signature_hex: &str,
    public_key_hex: &str,
) -> bool {
    let canonical_json = match canonical_manifest_for_verify(manifest_json) {
        Some(j) => j,
        None => return false,
    };

    let Ok(pub_bytes) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(pub_array) = pub_bytes.try_into() else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pub_array) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(sig_array) = sig_bytes.try_into() else {
        return false;
    };
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
    verifying_key
        .verify_strict(&canonical_json, &signature)
        .is_ok()
}

/// Strip the mutable signature fields from a published manifest so it matches
/// the bytes that were actually signed.
fn canonical_manifest_for_verify(manifest_json: &[u8]) -> Option<Vec<u8>> {
    let mut manifest: serde_json::Value = serde_json::from_slice(manifest_json).ok()?;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("signature".to_string(), serde_json::Value::Null);
        obj.insert("publicKey".to_string(), serde_json::Value::Null);
        obj.insert("publicKeyFingerprint".to_string(), serde_json::Value::Null);
    }
    serde_json::to_vec(&manifest).ok()
}

/// Compute an author pseudonym deterministically from a secret and author string.
#[cfg(test)]
pub fn test_compute_author_pseudonym(secret: &[u8], author: &str) -> String {
    compute_author_pseudonym(secret, author).expect("test secret is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_entry_to_json_sorted_keys() {
        let entry = PublicEntry {
            author_pseudonym: "p".to_string(),
            category: "FEATURE".to_string(),
            committed_at: "2026-07-14T12:00:00Z".to_string(),
            entry_hash: "h".to_string(),
            public_key: None,
            reason: "r".to_string(),
            risk_level: None,
            signature: None,
            summary: "s".to_string(),
            tx_id: "tx".to_string(),
            verification_result: None,
        };
        let value = entry.to_json_value();
        let keys: Vec<&String> = value.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn html_escape_escapes_special_chars() {
        assert_eq!(
            html_escape("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn compute_author_pseudonym_deterministic() {
        let secret = b"secret-key-32-bytes-long0000000";
        let a = compute_author_pseudonym(secret, "alice@example.com").unwrap();
        let b = compute_author_pseudonym(secret, "alice@example.com").unwrap();
        assert_eq!(a, b);
        let c = compute_author_pseudonym(secret, "bob@example.com").unwrap();
        assert_ne!(a, c);
    }
}
