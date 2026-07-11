//! SOC2 evidence export — assembles a tamper-evident `.zip` of ledger
//! provenance, verification history, and ADRs on the fly.
//!
//! The zip layout is:
//! - `manifest.json` — SHA-256 + size of every other file, plus
//!   `generatedAt` and `entryCount`. camelCase JSON.
//! - `manifest.sig` — Ed25519 signature over the `manifest.json` bytes (raw
//!   64-byte signature, written as raw bytes so the offline verifier's
//!   `Signature::from_bytes` path is direct).
//! - `manifest.pub` — Ed25519 verifying key (raw 32 bytes) for the signature.
//! - `ledger.csv` — all transactional provenance records (RFC 4180 CSV).
//! - `verification_history.csv` — CI gate pass/fail records.
//! - `adr/*.md` — generated MADR-format ADRs tied to the ledger.
//!
//! Tamper-evidence contract: re-hash each file's bytes and compare against
//! `manifest.json`'s `sha256`, then verify `manifest.sig` against
//! `manifest.pub` over the `manifest.json` bytes using the repo's Ed25519
//! keypair (`crate::ledger::crypto::get_or_create_keys`). This reuses the
//! same keypair `verify --signatures` uses, so the existing offline
//! verifier can validate the export.
//!
//! The module is gated behind the `web` feature because it depends on the
//! `zip` crate (listed in the `web` feature) and is only invoked from the
//! web dashboard's `/api/compliance/export` handler.

use crate::ledger::adr::{generate_madr_content, slugify_summary};
use crate::ledger::crypto::get_or_create_keys;
use crate::ledger::db::LedgerDb;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use ed25519_dalek::Signer;
use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;

/// One entry in `manifest.json`'s `files` array. `name` is the path inside
/// the zip (e.g. `"ledger.csv"`, `"adr/0001-use-uuid.md"`). `sha256` is the
/// hex-encoded SHA-256 of the file's bytes as written to the zip. `size` is
/// the byte length.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    name: String,
    sha256: String,
    size: u64,
}

/// The `manifest.json` payload. `files` is sorted by `name` ASC for
/// determinism. `generated_at` is `chrono::Utc::now().to_rfc3339()`.
/// `entry_count` is the number of ledger entries included in `ledger.csv`.
/// `manifest.json`, `manifest.sig`, and `manifest.pub` are NOT listed in
/// `files` — they ARE the manifest + signature.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    generated_at: String,
    files: Vec<ManifestFile>,
    entry_count: u64,
    gate_mode_disclosure: GateModeDisclosure,
}

/// Four-field mode disclosure required by track 0050.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GateModeDisclosure {
    reported_effective_mode: String,
    transition_history: String,
    chain_continuity_status: String,
    completeness_note: String,
}

/// A file to be written into the zip, with its manifest entry (name + hash +
/// size) precomputed from the bytes. Collecting these up front lets us sort
/// the manifest by name and write each file exactly once.
struct ZipEntry {
    name: String,
    bytes: Vec<u8>,
}

impl ZipEntry {
    fn new(name: impl Into<String>, bytes: Vec<u8>) -> (Self, ManifestFile) {
        let name = name.into();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sha256 = hex::encode(hasher.finalize());
        let size = bytes.len() as u64;
        let manifest_file = ManifestFile {
            name: name.clone(),
            sha256,
            size,
        };
        (ZipEntry { name, bytes }, manifest_file)
    }
}

/// Generate the SOC2 evidence export zip. All SQLite + zip + SHA-256 +
/// Ed25519 work happens synchronously here — callers MUST run this inside
/// `tokio::task::spawn_blocking` (the web handler does so).
///
/// No-DB / empty state still produces a valid zip: header-only CSVs, no
/// `adr/` files, a manifest with the files that exist, and a signature over
/// that manifest. Returns the raw zip bytes.
pub fn generate_soc2_export(layout: &Layout) -> Result<Vec<u8>> {
    // 1. Gather data. The ledger DB may not exist yet (fresh project / empty
    // state) — in that case we emit header-only CSVs and skip ADRs.
    let db_path = layout.state_subdir().join("ledger.db");
    let has_db = db_path.exists();

    let config = crate::config::load::load_config(layout).unwrap_or_default();

    let (ledger_entries, verification_rows, adr_entries) = if has_db {
        let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
        let conn = storage.get_connection();
        let db = LedgerDb::new(conn);

        let mut entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?;
        // `get_all_committed_ledger_entries` returns `committed_at ASC`
        // (`src/ledger/db/transactions.rs:349`); the CSV contract is
        // `committed_at ASC` so this is already correct. Sort defensively in
        // case the underlying ordering changes.
        entries.sort_by(|a, b| a.committed_at.cmp(&b.committed_at));

        let vrows = storage.get_verification_export_rows()?;

        let adrs = db
            .get_adr_entries(None)
            .map_err(|e| miette!("Failed to read ADR entries: {e}"))?;

        (entries, vrows, adrs)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    // 2. Build the file payloads (bytes + manifest entries).
    let mut zip_entries: Vec<ZipEntry> = Vec::new();
    let mut manifest_files: Vec<ManifestFile> = Vec::new();

    let ledger_csv = build_ledger_csv(&ledger_entries);
    let (entry, mf) = ZipEntry::new("ledger.csv", ledger_csv);
    zip_entries.push(entry);
    manifest_files.push(mf);

    let verify_csv = build_verification_csv(&verification_rows);
    let (entry, mf) = ZipEntry::new("verification_history.csv", verify_csv);
    zip_entries.push(entry);
    manifest_files.push(mf);

    // ADRs: one markdown file per ADR ledger entry, placed under `adr/`.
    // Filenames mirror `src/commands/ledger_adr.rs:107-110`.
    for adr in &adr_entries {
        // `slugify_summary` can return an empty string when the summary is
        // all non-alphanumeric characters (every char becomes `-` and is then
        // filtered out), which would yield a filename like `adr/0001-.md`.
        // Fall back to `untitled` so the filename is always well-formed; the
        // `{:04}` id prefix still keeps filenames unique across ADRs.
        let slug = slugify_summary(&adr.summary);
        let slug = if slug.is_empty() {
            "untitled"
        } else {
            slug.as_str()
        };
        let filename = format!("adr/{:04}-{}.md", adr.id, slug);
        let content = generate_madr_content(adr);
        let (entry, mf) = ZipEntry::new(filename, content.into_bytes());
        zip_entries.push(entry);
        manifest_files.push(mf);
    }

    // 3. Determinism: sort manifest files by name ASC. The zip itself is
    // written in the same order so the byte stream is also deterministic
    // (modulo zip metadata timestamps, which `SimpleFileOptions::default`
    // leaves at epoch zero).
    manifest_files.sort_by(|a, b| a.name.cmp(&b.name));
    zip_entries.sort_by(|a, b| a.name.cmp(&b.name));

    let mode_disclosure = build_mode_disclosure(&ledger_entries, &config.gate.mode);

    let manifest = Manifest {
        generated_at: chrono::Utc::now().to_rfc3339(),
        files: manifest_files,
        entry_count: ledger_entries.len() as u64,
        gate_mode_disclosure: mode_disclosure,
    };
    let manifest_json = serde_json::to_vec(&manifest)
        .map_err(|e| miette!("Failed to serialize manifest.json: {e}"))?;

    // 4. Sign the manifest JSON bytes with the repo's Ed25519 keypair.
    let (signing_key, verifying_key) = get_or_create_keys()?;
    let signature: [u8; 64] = signing_key.sign(&manifest_json).to_bytes();
    let pub_bytes: [u8; 32] = verifying_key.to_bytes();

    // 5. Assemble the zip. Mirror `src/sync/bundle.rs::Bundle::build` for the
    // `zip` 2.x API: `ZipWriter::new(Cursor::new(&mut buf))`,
    // `SimpleFileOptions::default().compression_method(Deflated)`.
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for entry in &zip_entries {
            zip.start_file(&entry.name, options)
                .map_err(|e| miette!("Failed to start {} in zip: {e}", entry.name))?;
            zip.write_all(&entry.bytes)
                .map_err(|e| miette!("Failed to write {} to zip: {e}", entry.name))?;
        }

        // manifest.json + manifest.sig + manifest.pub are written AFTER the
        // data files and are intentionally NOT listed in the manifest.
        zip.start_file("manifest.json", options)
            .map_err(|e| miette!("Failed to start manifest.json: {e}"))?;
        zip.write_all(&manifest_json)
            .map_err(|e| miette!("Failed to write manifest.json: {e}"))?;

        zip.start_file("manifest.sig", options)
            .map_err(|e| miette!("Failed to start manifest.sig: {e}"))?;
        zip.write_all(&signature)
            .map_err(|e| miette!("Failed to write manifest.sig: {e}"))?;

        zip.start_file("manifest.pub", options)
            .map_err(|e| miette!("Failed to start manifest.pub: {e}"))?;
        zip.write_all(&pub_bytes)
            .map_err(|e| miette!("Failed to write manifest.pub: {e}"))?;

        zip.finish()
            .map_err(|e| miette!("Failed to finish SOC2 export zip: {e}"))?;
    }

    Ok(buf)
}

// ---------------------------------------------------------------------------
// CSV helpers — hand-rolled RFC 4180 (no `csv` crate dependency).
// ---------------------------------------------------------------------------

/// Quote a CSV field per RFC 4180 with minimal quoting: a field is wrapped
/// in double quotes ONLY when it contains a comma, double quote, CR, or LF
/// (`needs_quoting`); fields without those characters are passed through
/// unquoted. When quoting is required, the field is wrapped in double quotes
/// and any inner double quotes are doubled (`"` → `""`), as RFC 4180
/// requires. This is the minimal RFC 4180 quoting form — fields are quoted
/// exactly when the spec mandates it and never otherwise.
fn build_mode_disclosure(
    entries: &[crate::ledger::types::LedgerEntry],
    current_mode: &str,
) -> GateModeDisclosure {
    let mut mode: Option<String> = None;
    let mut transition_points: Vec<String> = Vec::new();
    let mut observed_ranges: Vec<String> = Vec::new();
    let mut last_index: usize = 0;

    for (idx, entry) in entries.iter().enumerate() {
        if entry.entity == "ledgerful/gate-mode"
            && entry.entry_type == crate::ledger::types::EntryType::Maintenance
        {
            let new_mode = parse_mode_from_entry_text(&entry.summary)
                .or_else(|| parse_mode_from_entry_text(&entry.reason));
            if let Some(new_mode) = new_mode {
                if let Some(prev) = mode.as_ref() {
                    observed_ranges.push(format!(
                        "entries {}–{} under {}",
                        last_index + 1,
                        idx,
                        prev
                    ));
                }
                mode = Some(new_mode.clone());
                transition_points.push(format!(
                    "entry {}: {} (tx_id: {})",
                    idx + 1,
                    new_mode,
                    entry.tx_id
                ));
                last_index = idx + 1;
            }
        }
    }

    if let Some(prev) = mode.as_ref()
        && last_index < entries.len()
    {
        observed_ranges.push(format!(
            "entries {}–{} under {}",
            last_index + 1,
            entries.len(),
            prev
        ));
    }

    let transition_history = if transition_points.is_empty() {
        format!(
            "No mode-transition entries found; all {} entries predate mode tracking.",
            entries.len()
        )
    } else {
        format!(
            "{}; {}",
            transition_points.join("; "),
            observed_ranges.join("; ")
        )
    };

    GateModeDisclosure {
        reported_effective_mode: current_mode.to_string(),
        transition_history,
        chain_continuity_status: "not verified — chain feature not present".to_string(),
        completeness_note:
            "Completeness of the transition history is established only when chain continuity is verified."
                .to_string(),
    }
}

fn parse_mode_from_entry_text(text: &str) -> Option<String> {
    let normalized = text.to_lowercase();
    if normalized.contains("to enforce") || normalized.contains("initialized to enforce") {
        Some("enforce".to_string())
    } else if normalized.contains("to observe") || normalized.contains("initialized to observe") {
        Some("observe".to_string())
    } else {
        None
    }
}

fn csv_quote(s: &str) -> String {
    let needs_quoting = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if needs_quoting {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

/// Build `ledger.csv` from the committed ledger entries.
///
/// Columns: `tx_id,category,entity,change_type,summary,reason,committed_at,
/// signed,signature`. `signed` is `yes`/`no` from `signature.is_some()`;
/// `signature` is the hex signature or empty. Rows are sorted by
/// `committed_at` ASC (the caller sorts the entries before passing them;
/// this function preserves that order).
fn build_ledger_csv(entries: &[crate::ledger::types::LedgerEntry]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(
        "tx_id,category,entity,change_type,summary,reason,committed_at,signed,signature,observed\n",
    );
    for entry in entries {
        let signed = if entry.signature.is_some() {
            "yes"
        } else {
            "no"
        };
        let signature = entry.signature.clone().unwrap_or_default();
        let observed = match entry.observed {
            Some(true) => "yes",
            Some(false) => "no",
            None => "",
        };
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            csv_quote(&entry.tx_id),
            csv_quote(&entry.category.to_string()),
            csv_quote(&entry.entity),
            csv_quote(&format!("{:?}", entry.change_type)),
            csv_quote(&entry.summary),
            csv_quote(&entry.reason),
            csv_quote(&entry.committed_at),
            signed,
            csv_quote(&signature),
            observed,
        ));
    }
    out.into_bytes()
}

/// Build `verification_history.csv` from the joined
/// `verification_runs` × `verification_results` rows.
///
/// Columns: `run_timestamp,overall_pass,command,exit_code,duration_ms`.
/// `overall_pass` is `true`/`false`. Header-only when there are no rows.
fn build_verification_csv(rows: &[crate::state::storage::VerificationExportRow]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("run_timestamp,overall_pass,command,exit_code,duration_ms\n");
    for row in rows {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_quote(&row.run_timestamp),
            row.overall_pass,
            csv_quote(&row.command),
            row.exit_code,
            row.duration_ms,
        ));
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quote_passes_through_plain_field() {
        assert_eq!(csv_quote("plain"), "plain");
    }

    #[test]
    fn csv_quote_quotes_comma_field() {
        assert_eq!(csv_quote("a,b"), "\"a,b\"");
    }

    #[test]
    fn csv_quote_doubles_inner_quotes() {
        assert_eq!(csv_quote("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn csv_quote_quotes_newlines() {
        assert_eq!(csv_quote("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn ledger_csv_escapes_commas_in_summary() {
        use crate::ledger::types::*;
        let entry = LedgerEntry {
            id: 1,
            tx_id: "tx-1".to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "e".to_string(),
            entity_normalized: "e".to_string(),
            change_type: ChangeType::Modify,
            summary: "summary, with comma".to_string(),
            reason: "reason".to_string(),
            is_breaking: false,
            committed_at: "2026-06-20T10:00:00Z".to_string(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: Some("sig".to_string()),
            public_key: Some("pub".to_string()),
            risk: None,
            related_tickets: None,
            author: "Test".to_string(),
            observed: Some(true),
        };
        let csv = build_ledger_csv(&[entry]);
        let s = std::str::from_utf8(&csv).unwrap();
        assert!(
            s.contains("\"summary, with comma\""),
            "summary with comma must be quoted: {s}"
        );
        assert!(s.contains(",yes,"));
        assert!(
            s.ends_with(",yes\n"),
            "observed marker should be exported: {s}"
        );
    }

    #[test]
    fn verification_csv_is_header_only_when_empty() {
        let csv = build_verification_csv(&[]);
        let s = std::str::from_utf8(&csv).unwrap();
        assert_eq!(
            s,
            "run_timestamp,overall_pass,command,exit_code,duration_ms\n"
        );
    }
}
