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

use crate::export::control_mapping::{
    ControlMapping, ControlSelector, generate_control_lens_files,
};
use crate::ledger::adr::{generate_madr_content, slugify_summary};
use crate::ledger::crypto::get_or_create_keys;
use crate::ledger::db::LedgerDb;
use crate::ledger::types::{AdrStatus, ChainHead};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use ed25519_dalek::Signer;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

/// One entry in `manifest.json`'s `files` array. `name` is the path inside
/// the zip (e.g. `"ledger.csv"`, `"adr/0001-use-uuid.md"`). `sha256` is the
/// hex-encoded SHA-256 of the file's bytes as written to the zip. `size` is
/// the byte length.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    name: String,
    sha256: String,
    size: u64,
}

fn is_false(v: &bool) -> bool {
    !v
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
    #[serde(skip_serializing_if = "is_false")]
    demo: bool,
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
    generate_soc2_export_with_options(layout, false, None, None)
}

/// Generate a SOC2 evidence export with an optional demo marker and
/// custom keys directory.
///
/// `demo` adds `demo: true` to `manifest.json`, includes an `index.md`
/// explaining the export was generated from a synthetic repository, and uses
/// the demo signing path so the evidence is self-identifying.
///
/// `keys_dir` overrides the default home-scoped key directory. When `Some`,
/// keys are loaded from the given path instead of `~/.ledgerful/keys/`.
/// This is used by the demo command to sign with a repo-local ephemeral
/// keypair instead of the user's production keys.
pub fn generate_soc2_export_with_options(
    layout: &Layout,
    demo: bool,
    keys_dir: Option<&Path>,
    controls: Option<&ControlSelector>,
) -> Result<Vec<u8>> {
    // 1. Gather data. The ledger DB may not exist yet (fresh project / empty
    // state) — in that case we emit header-only CSVs and skip ADRs.
    let db_path = layout.state_subdir().join("ledger.db");
    let has_db = db_path.exists();

    let config = crate::config::load::load_config(layout).unwrap_or_default();

    let (ledger_entries, verification_rows, adr_entries, stored_head, chain_head) = if has_db {
        let storage = StorageManager::open_read_only_sqlite_only(layout)?;
        let conn = storage.get_connection();
        let db = LedgerDb::new(conn);

        let mut entries = db
            .get_all_committed_ledger_entries()
            .map_err(|e| miette!("Failed to read ledger entries: {e}"))?;
        // `get_all_committed_ledger_entries` returns `committed_at ASC, tx_id ASC`
        // (`src/ledger/db/transactions.rs:349`); the CSV contract is
        // `committed_at ASC, tx_id ASC` so this is already correct. Sort defensively in
        // case the underlying ordering changes.
        entries.sort_by(|a, b| {
            a.committed_at
                .cmp(&b.committed_at)
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });

        let vrows = storage.get_verification_export_rows()?;

        let adrs = db
            .get_adr_entries(None)
            .map_err(|e| miette!("Failed to read ADR entries: {e}"))?;
        let mut paired = Vec::with_capacity(adrs.len());
        for adr in adrs {
            let lifecycle = match db.get_adr_metadata(&adr.tx_id) {
                Ok(Some(metadata)) => metadata.status,
                Ok(None) => AdrStatus::Proposed,
                Err(e) => {
                    return Err(miette!(
                        "Failed to read ADR metadata for {}: {e}",
                        adr.tx_id
                    ));
                }
            };
            paired.push((adr, lifecycle));
        }

        let head = match conn
            .query_row(
                "SELECT latest_entry_hash, genesis, length, head_signature, head_public_key, updated_at FROM chain_head WHERE id = 1",
                [],
                |row| {
                    Ok(ChainHead {
                        latest_entry_hash: row.get(0)?,
                        genesis: row.get(1)?,
                        length: row.get(2)?,
                        head_signature: row.get(3)?,
                        head_public_key: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
        {
            Ok(head) => head,
            Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => None,
            Err(e) => return Err(miette!("Failed to read chain head: {e}")),
        };

        // Zip still synthesizes chain_head.json for legacy/empty-head
        // exports (0050). Disclosure classifies from the stored row only.
        let stored_head = head;
        let zip_head = stored_head
            .clone()
            .or_else(|| synthesize_chain_head(&entries));
        (entries, vrows, paired, stored_head, zip_head)
    } else {
        (Vec::new(), Vec::new(), Vec::new(), None, None)
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
    for (adr, lifecycle) in &adr_entries {
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
        let content = generate_madr_content(adr, *lifecycle);
        let (entry, mf) = ZipEntry::new(filename, content.into_bytes());
        zip_entries.push(entry);
        manifest_files.push(mf);
    }

    // Demo self-identification: index.md explains the synthetic origin.
    if demo {
        let index_md = DEMO_INDEX_MD.as_bytes().to_vec();
        let (entry, mf) = ZipEntry::new("index.md", index_md);
        zip_entries.push(entry);
        manifest_files.push(mf);
    }

    let mode_disclosure =
        build_mode_disclosure(&ledger_entries, &config.gate.mode, stored_head.as_ref());

    // Include chain_head.json in the export before sorting so it participates
    // in the deterministic alphabetical order.
    if let Some(ref head) = chain_head {
        let chain_head_json = serde_json::to_vec(head)
            .map_err(|e| miette!("Failed to serialize chain_head.json: {e}"))?;
        let (entry, mf) = ZipEntry::new("chain_head.json", chain_head_json);
        zip_entries.push(entry);
        manifest_files.push(mf);
    }

    // Additive control lens: requested controls produce cover.md + index.json
    // under control-lens/. These files are added to the signed bundle; they do
    // not alter, remove, or truncate any existing file.
    if let Some(selector) = controls {
        if selector.requested().is_empty() {
            return Err(miette!("at least one --control value is required"));
        }
        let mapping = ControlMapping::load_static()?;
        let selected = selector.select(&mapping)?;
        let requested = selector.canonical_requested();
        let lens_files = generate_control_lens_files(
            &mapping,
            &selected,
            &ledger_entries,
            &requested,
            chain_head.as_ref(),
        )?;
        for (name, bytes) in lens_files {
            let (entry, mf) = ZipEntry::new(name, bytes);
            zip_entries.push(entry);
            manifest_files.push(mf);
        }
    }

    // 3. Determinism: sort manifest files by name ASC. The zip itself is
    // written in the same order so the byte stream is also deterministic
    // (modulo zip metadata timestamps, which `SimpleFileOptions::default`
    // leaves at epoch zero).
    manifest_files.sort_by(|a, b| a.name.cmp(&b.name));
    zip_entries.sort_by(|a, b| a.name.cmp(&b.name));

    let manifest = Manifest {
        generated_at: chrono::Utc::now().to_rfc3339(),
        files: manifest_files,
        entry_count: ledger_entries.len() as u64,
        gate_mode_disclosure: mode_disclosure,
        demo,
    };
    let manifest_json = serde_json::to_vec(&manifest)
        .map_err(|e| miette!("Failed to serialize manifest.json: {e}"))?;

    // 4. Sign the manifest JSON bytes with the repo's Ed25519 keypair.
    // When keys_dir is provided (demo repos), use that path instead of the
    // home-scoped default to ensure demo exports are signed with the
    // repo-local ephemeral keypair, never the user's production keys.
    let (signing_key, verifying_key) = if let Some(kd) = keys_dir {
        crate::ledger::crypto::get_or_create_keys_in(kd)?
    } else {
        get_or_create_keys()?
    };
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

const DEMO_INDEX_MD: &str = "# Ledgerful DEMO Evidence Export

This evidence export was generated by the `ledgerful demo` command on a synthetic
repository. It demonstrates Ledgerful's signed ledger, verification history,
and tamper-evident export format.

**This is not real development work.** The repository, commits, keys, and
entries are disposable artifacts created solely to show the Ledgerful workflow.
Do not use this export as actual compliance evidence.

To inspect the demo repo, re-run:

```bash
ledgerful demo --keep
```

The export can be verified offline with the public key included as
`manifest.pub` and the signature in `manifest.sig`.
";

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
const NOTE_VERIFIED: &str = "Walked prev_hash continuity of the presented chain matches the stored signed head; this does not assert per-entry signature validity. Detection of rollback to an earlier valid state requires an independently retained chain head.";
const NOTE_SIGNED_HEAD_ONLY: &str = "Chain walk was not full-chain verified; authenticity is based on the stored head signature only; detection of rollback to an earlier valid state requires an independently retained chain head.";
const NOTE_INVALID: &str = "Chain continuity or stored head signature is invalid; this field does not assert per-entry signature validity.";
const NOTE_NOT_VERIFIED: &str = "Chain continuity is not verified; the chain feature is not started, the stored head is unsigned, or the head was synthesized.";
const EN_DASH: char = '\u{2013}';

fn push_mode_range(ranges: &mut Vec<String>, start: usize, end: usize, mode: &str) {
    if start > end {
        return;
    }
    ranges.push(format!("entries {start}{EN_DASH}{end} under {mode}"));
}

fn head_sig_fields_present(head: &ChainHead) -> bool {
    let sig = head.head_signature.as_deref().unwrap_or("");
    let pubk = head.head_public_key.as_deref().unwrap_or("");
    !sig.is_empty() && !pubk.is_empty()
}

fn first_walk_failure(
    entries: &[crate::ledger::types::LedgerEntry],
    head: &ChainHead,
) -> Option<String> {
    let walk = crate::ledger::chain_iter::iter_local_chain_with_head(entries, Some(head));
    // Extra-genesis / orphans / link-breaks apply only after a prev_hash
    // chain has started. Spec §1b writes
    // `should_walk_chain = stored_head.is_some() || has_any_prev_link`,
    // but this function runs only with a stored head, so that formula
    // would always be true and would mislabel pre-chain + real head as
    // extra-genesis `signed-head-only:` (DoD-3). Gate on linkage.
    let has_any_prev_link = walk
        .ordered
        .iter()
        .any(|e| e.prev_hash.as_deref().is_some_and(|p| !p.is_empty()))
        || entries
            .iter()
            .any(|e| e.origin == "LOCAL" && e.prev_hash.as_deref().is_some_and(|p| !p.is_empty()));

    if !walk.forks.is_empty() {
        return Some(format!(
            "signed-head-only: detected {} fork(s) in local chain (first parent hash {})",
            walk.forks.len(),
            walk.forks[0].0
        ));
    }
    if has_any_prev_link && !walk.extra_genesis.is_empty() {
        return Some(format!(
            "signed-head-only: {} additional genesis entry/entries with null prev_hash after chain started",
            walk.extra_genesis.len()
        ));
    }
    if has_any_prev_link && !walk.orphans.is_empty() {
        return Some(format!(
            "signed-head-only: {} orphan local entry/entries not linked by prev_hash",
            walk.orphans.len()
        ));
    }
    if has_any_prev_link
        && let Some(msg) = crate::ledger::chain_iter::check_chain_links(&walk.ordered)
    {
        let detail = msg
            .strip_prefix("Chain break at TX ")
            .unwrap_or(msg.as_str());
        return Some(format!("signed-head-only: chain break at TX {detail}"));
    }

    let computed_hash = walk.tail_hash().unwrap_or_default();
    if computed_hash != head.latest_entry_hash {
        return Some(format!(
            "signed-head-only: latest entry hash mismatch (computed {computed_hash}, head claims {})",
            head.latest_entry_hash
        ));
    }
    let computed_len = walk.length();
    if computed_len != head.length {
        return Some(format!(
            "signed-head-only: chain length mismatch (computed {computed_len}, head claims {})",
            head.length
        ));
    }
    None
}

fn classify_continuity(
    entries: &[crate::ledger::types::LedgerEntry],
    stored_head: Option<&ChainHead>,
) -> (String, String) {
    if entries.is_empty() {
        return (
            "not verified — chain feature not present or not yet started".to_string(),
            NOTE_NOT_VERIFIED.to_string(),
        );
    }
    match stored_head {
        Some(head) => {
            if !head_sig_fields_present(head) {
                return (
                    "not verified — unsigned chain head".to_string(),
                    NOTE_NOT_VERIFIED.to_string(),
                );
            }
            let valid = crate::ledger::crypto::verify_chain_head(
                &head.latest_entry_hash,
                &head.genesis,
                head.length,
                head.head_signature.as_deref().unwrap_or(""),
                head.head_public_key.as_deref().unwrap_or(""),
            );
            if !valid {
                return (
                    format!(
                        "INVALID — stored chain head signature does not verify (head {}, genesis {}, length {})",
                        head.latest_entry_hash, head.genesis, head.length
                    ),
                    NOTE_INVALID.to_string(),
                );
            }
            if let Some(reason) = first_walk_failure(entries, head) {
                return (reason, NOTE_SIGNED_HEAD_ONLY.to_string());
            }
            (
                format!(
                    "verified: {} linked entries from genesis {} to head {}",
                    head.length, head.genesis, head.latest_entry_hash
                ),
                NOTE_VERIFIED.to_string(),
            )
        }
        None => {
            let any_prev = entries
                .iter()
                .any(|e| e.prev_hash.as_deref().is_some_and(|p| !p.is_empty()));
            if any_prev {
                (
                    "INVALID — entries have prev_hash values but no chain head is present (downgrade detected)".to_string(),
                    NOTE_INVALID.to_string(),
                )
            } else {
                (
                    "not verified — unsigned synthesized head".to_string(),
                    NOTE_NOT_VERIFIED.to_string(),
                )
            }
        }
    }
}

fn build_mode_disclosure(
    entries: &[crate::ledger::types::LedgerEntry],
    current_mode: &str,
    stored_head: Option<&ChainHead>,
) -> GateModeDisclosure {
    let mut mode: Option<String> = None;
    let mut transition_points: Vec<String> = Vec::new();
    let mut observed_ranges: Vec<String> = Vec::new();
    let mut last_transition_1based: usize = 0;

    for (idx, entry) in entries.iter().enumerate() {
        if entry.entity == "ledgerful/gate-mode"
            && entry.entry_type == crate::ledger::types::EntryType::Maintenance
        {
            let new_mode = parse_mode_from_entry_text(&entry.summary)
                .or_else(|| parse_mode_from_entry_text(&entry.reason));
            if let Some(new_mode) = new_mode {
                if let Some(prev) = mode.as_ref() {
                    let current_1based = idx + 1;
                    push_mode_range(
                        &mut observed_ranges,
                        last_transition_1based + 1,
                        current_1based - 1,
                        prev,
                    );
                }
                mode = Some(new_mode.clone());
                transition_points.push(format!(
                    "entry {}: {} (tx_id: {})",
                    idx + 1,
                    new_mode,
                    entry.tx_id
                ));
                last_transition_1based = idx + 1;
            }
        }
    }

    if let Some(prev) = mode.as_ref() {
        push_mode_range(
            &mut observed_ranges,
            last_transition_1based + 1,
            entries.len(),
            prev,
        );
    }

    let transition_history = if transition_points.is_empty() {
        format!(
            "No mode-transition entries found; all {} entries predate mode tracking.",
            entries.len()
        )
    } else if observed_ranges.is_empty() {
        transition_points.join("; ")
    } else {
        format!(
            "{}; {}",
            transition_points.join("; "),
            observed_ranges.join("; ")
        )
    };

    let (chain_continuity_status, completeness_note) = classify_continuity(entries, stored_head);

    GateModeDisclosure {
        reported_effective_mode: current_mode.to_string(),
        transition_history,
        chain_continuity_status,
        completeness_note,
    }
}

/// Synthesize a `ChainHead` from ledger entries when the singleton
/// `chain_head` row is absent. This ensures every non-empty export carries a
/// `chain_head.json` for rollback detection, even for legacy ledgers or
/// rows inserted outside the normal append path.
///
/// Ordering is shared with against-export via
/// [`crate::ledger::chain_checkpoint::ordered_local_for_head`]: post-chain uses
/// the LOCAL `prev_hash` walk; pre-chain uses full LOCAL `(committed_at, tx_id)`
/// sort so multi-entry pre-chain length/hash stay consistent. Signature fields
/// are `None` because no signed singleton exists.
pub fn synthesize_chain_head(entries: &[crate::ledger::types::LedgerEntry]) -> Option<ChainHead> {
    if entries.is_empty() {
        return None;
    }
    let ordered = crate::ledger::chain_checkpoint::ordered_local_for_head(entries);
    if ordered.is_empty() {
        return None;
    }
    let first = ordered.first()?;
    let last = ordered.last()?;
    // genesis is the ISO-8601 timestamp of the first in-chain entry, matching
    // the normal append path and verify_chain_integrity.
    let genesis = first.committed_at.clone();
    // Fail closed on encode errors (never drop the head silently).
    let latest = match crate::ledger::crypto::compute_entry_hash_for_entry(last) {
        Ok(h) => h,
        Err(err) => {
            tracing::error!(
                tx_id = %last.tx_id,
                error = %err,
                "synthesize_chain_head: entry hash encode failed"
            );
            return None;
        }
    };
    Some(ChainHead {
        latest_entry_hash: latest,
        genesis,
        length: ordered.len() as i64,
        head_signature: None,
        head_public_key: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
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
        "tx_id,category,entity,change_type,summary,reason,committed_at,signed,signature,observed,prev_hash\n",
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
        let prev_hash = entry.prev_hash.as_deref().unwrap_or("");
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
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
            csv_quote(prev_hash),
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
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use crate::ledger::crypto::{compute_entry_hash_for_entry, sign_chain_head};
    use crate::ledger::types::{
        Category, ChangeType, EntryType, LedgerEntry, VerificationBasis, VerificationStatus,
    };

    fn sample_entry(tx: &str, prev: Option<&str>, committed_at: &str) -> LedgerEntry {
        LedgerEntry {
            id: 1,
            tx_id: tx.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "e".to_string(),
            entity_normalized: "e".to_string(),
            change_type: ChangeType::Modify,
            summary: "s".to_string(),
            reason: "r".to_string(),
            is_breaking: false,
            committed_at: committed_at.to_string(),
            verification_status: None::<VerificationStatus>,
            verification_basis: None::<VerificationBasis>,
            outcome_notes: None,
            origin: "LOCAL".to_string(),
            trace_id: None,
            signature: Some("sig".to_string()),
            public_key: Some("pub".to_string()),
            risk: None,
            related_tickets: None,
            author: "Test".to_string(),
            observed: None,
            prev_hash: prev.map(str::to_string),
            sig_version: 1,
        }
    }

    fn gate_mode_entry(tx: &str, mode: &str, committed_at: &str) -> LedgerEntry {
        let mut entry = sample_entry(tx, None, committed_at);
        entry.entity = "ledgerful/gate-mode".to_string();
        entry.entity_normalized = "ledgerful/gate-mode".to_string();
        entry.entry_type = EntryType::Maintenance;
        entry.summary = format!("initialized to {mode}");
        entry
    }

    fn sign_head(latest: &str, genesis: &str, length: i64) -> ChainHead {
        let dir = tempfile::tempdir().expect("temp keys dir");
        let (sig, pubk) =
            sign_chain_head(dir.path(), latest, genesis, length).expect("sign_chain_head");
        ChainHead {
            latest_entry_hash: latest.to_string(),
            genesis: genesis.to_string(),
            length,
            head_signature: sig,
            head_public_key: pubk,
            updated_at: "2026-01-02T00:00:00Z".to_string(),
        }
    }

    fn linked_two() -> (Vec<LedgerEntry>, ChainHead) {
        let a = sample_entry("tx-a", None, "2026-01-01T00:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash a");
        let b = sample_entry("tx-b", Some(&a_hash), "2026-01-02T00:00:00Z");
        let b_hash = compute_entry_hash_for_entry(&b).expect("hash b");
        let head = sign_head(&b_hash, &a.committed_at, 2);
        (vec![a, b], head)
    }

    #[test]
    fn build_mode_disclosure__empty_entries__not_verified_chain_feature() {
        let d = build_mode_disclosure(&[], "observe", None);
        assert_eq!(
            d.chain_continuity_status,
            "not verified — chain feature not present or not yet started"
        );
        assert_eq!(d.completeness_note, NOTE_NOT_VERIFIED);
    }

    #[test]
    fn build_mode_disclosure__signed_head_clean_walk__verified() {
        let (entries, head) = linked_two();
        let d = build_mode_disclosure(&entries, "observe", Some(&head));
        assert!(
            d.chain_continuity_status.starts_with("verified:"),
            "{}",
            d.chain_continuity_status
        );
        assert_eq!(d.completeness_note, NOTE_VERIFIED);
    }

    #[test]
    fn build_mode_disclosure__extra_genesis_after_chain_started__signed_head_only() {
        let (mut entries, head) = linked_two();
        entries.push(sample_entry("tx-extra", None, "2026-01-03T00:00:00Z"));
        let d = build_mode_disclosure(&entries, "observe", Some(&head));
        assert!(
            d.chain_continuity_status.starts_with("signed-head-only:"),
            "{}",
            d.chain_continuity_status
        );
        assert!(
            d.chain_continuity_status.contains("additional genesis"),
            "{}",
            d.chain_continuity_status
        );
        assert!(!d.chain_continuity_status.starts_with("verified:"));
        assert_eq!(d.completeness_note, NOTE_SIGNED_HEAD_ONLY);
    }

    #[test]
    fn build_mode_disclosure__orphan__signed_head_only() {
        let (mut entries, head) = linked_two();
        entries.push(sample_entry(
            "tx-orphan",
            Some("deadbeef"),
            "2026-01-03T00:00:00Z",
        ));
        let d = build_mode_disclosure(&entries, "observe", Some(&head));
        assert!(
            d.chain_continuity_status.contains("orphan"),
            "{}",
            d.chain_continuity_status
        );
        assert_eq!(d.completeness_note, NOTE_SIGNED_HEAD_ONLY);
    }

    #[test]
    fn build_mode_disclosure__fork__signed_head_only() {
        let a = sample_entry("tx-a", None, "2026-01-01T00:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash a");
        let b = sample_entry("tx-b", Some(&a_hash), "2026-01-02T00:00:00Z");
        let c = sample_entry("tx-c", Some(&a_hash), "2026-01-03T00:00:00Z");
        let b_hash = compute_entry_hash_for_entry(&b).expect("hash b");
        let head = sign_head(&b_hash, &a.committed_at, 2);
        let d = build_mode_disclosure(&[a, b, c], "observe", Some(&head));
        assert!(
            d.chain_continuity_status.starts_with("signed-head-only:"),
            "{}",
            d.chain_continuity_status
        );
        assert!(
            d.chain_continuity_status.contains("fork"),
            "{}",
            d.chain_continuity_status
        );
        assert_eq!(d.completeness_note, NOTE_SIGNED_HEAD_ONLY);
    }

    #[test]
    fn build_mode_disclosure__length_mismatch__signed_head_only() {
        let (entries, mut head) = linked_two();
        head.length = 99;
        let dir = tempfile::tempdir().expect("temp keys dir");
        let (sig, pubk) = sign_chain_head(
            dir.path(),
            &head.latest_entry_hash,
            &head.genesis,
            head.length,
        )
        .expect("re-sign");
        head.head_signature = sig;
        head.head_public_key = pubk;
        let d = build_mode_disclosure(&entries, "observe", Some(&head));
        assert!(
            d.chain_continuity_status.starts_with("signed-head-only:"),
            "{}",
            d.chain_continuity_status
        );
        assert!(
            d.chain_continuity_status.contains("chain length mismatch"),
            "{}",
            d.chain_continuity_status
        );
        assert_eq!(d.completeness_note, NOTE_SIGNED_HEAD_ONLY);
    }

    #[test]
    fn build_mode_disclosure__no_stored_head_no_prev__unsigned_synthesized() {
        let entries = vec![sample_entry("tx-a", None, "2026-01-01T00:00:00Z")];
        let d = build_mode_disclosure(&entries, "observe", None);
        assert_eq!(
            d.chain_continuity_status,
            "not verified — unsigned synthesized head"
        );
        assert_eq!(d.completeness_note, NOTE_NOT_VERIFIED);
    }

    #[test]
    fn build_mode_disclosure__stored_unsigned_head__not_synthesized_token() {
        let entries = vec![sample_entry("tx-a", None, "2026-01-01T00:00:00Z")];
        let head = ChainHead {
            latest_entry_hash: "abc".to_string(),
            genesis: "2026-01-01T00:00:00Z".to_string(),
            length: 1,
            head_signature: None,
            head_public_key: None,
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let d = build_mode_disclosure(&entries, "observe", Some(&head));
        assert_eq!(
            d.chain_continuity_status,
            "not verified — unsigned chain head"
        );
        assert_eq!(d.completeness_note, NOTE_NOT_VERIFIED);
    }

    #[test]
    fn build_mode_disclosure__no_stored_head_with_prev__invalid_downgrade() {
        let entries = vec![sample_entry("tx-a", Some("prev"), "2026-01-01T00:00:00Z")];
        let d = build_mode_disclosure(&entries, "observe", None);
        assert_eq!(
            d.chain_continuity_status,
            "INVALID — entries have prev_hash values but no chain head is present (downgrade detected)"
        );
        assert_eq!(d.completeness_note, NOTE_INVALID);
    }

    #[test]
    fn build_mode_disclosure__prechain_real_head__not_extra_genesis() {
        let a = sample_entry("tx-a", None, "2026-01-01T00:00:00Z");
        let b = sample_entry("tx-b", None, "2026-01-02T00:00:00Z");
        let a_hash = compute_entry_hash_for_entry(&a).expect("hash a");
        let head = sign_head(&a_hash, &a.committed_at, 1);
        let d = build_mode_disclosure(&[a, b], "observe", Some(&head));
        assert!(
            !d.chain_continuity_status.contains("additional genesis"),
            "{}",
            d.chain_continuity_status
        );
        assert!(
            d.chain_continuity_status.starts_with("verified:"),
            "{}",
            d.chain_continuity_status
        );
    }

    #[test]
    fn build_mode_disclosure__consecutive_transitions__omit_inverted_range() {
        let entries = vec![
            gate_mode_entry("tx-g1", "enforce", "2026-01-01T00:00:00Z"),
            gate_mode_entry("tx-g2", "observe", "2026-01-02T00:00:00Z"),
            sample_entry("tx-c", None, "2026-01-03T00:00:00Z"),
        ];
        let d = build_mode_disclosure(&entries, "observe", None);
        assert!(
            !d.transition_history.contains("entries 2–1"),
            "{}",
            d.transition_history
        );
        assert!(
            !d.transition_history.split("entries ").skip(1).any(|rest| {
                let Some((span, _)) = rest.split_once(" under ") else {
                    return false;
                };
                let Some((start, end)) = span.split_once(EN_DASH) else {
                    return false;
                };
                start
                    .parse::<usize>()
                    .ok()
                    .zip(end.parse::<usize>().ok())
                    .is_some_and(|(s, e)| s > e)
            }),
            "{}",
            d.transition_history
        );
    }

    #[test]
    fn build_mode_disclosure__single_entry_span__en_dash_nn() {
        let entries = vec![
            gate_mode_entry("tx-g1", "enforce", "2026-01-01T00:00:00Z"),
            sample_entry("tx-mid", None, "2026-01-02T00:00:00Z"),
            gate_mode_entry("tx-g2", "observe", "2026-01-03T00:00:00Z"),
        ];
        let d = build_mode_disclosure(&entries, "observe", None);
        assert!(
            d.transition_history.contains("entries 2–2 under enforce"),
            "{}",
            d.transition_history
        );
        assert!(d.transition_history.contains(EN_DASH));
    }

    #[test]
    fn build_mode_disclosure__trailing_range_omitted_when_last_is_transition() {
        let entries = vec![
            sample_entry("tx-a", None, "2026-01-01T00:00:00Z"),
            gate_mode_entry("tx-g1", "enforce", "2026-01-02T00:00:00Z"),
        ];
        let d = build_mode_disclosure(&entries, "enforce", None);
        assert!(
            !d.transition_history.contains("under enforce")
                || !d.transition_history.contains("entries 3–"),
            "{}",
            d.transition_history
        );
        assert!(
            !d.transition_history.contains("entries 3"),
            "{}",
            d.transition_history
        );
    }

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
            prev_hash: None,
            sig_version: 1,
        };
        let csv = build_ledger_csv(&[entry]);
        let s = std::str::from_utf8(&csv).unwrap();
        assert!(
            s.contains("\"summary, with comma\""),
            "summary with comma must be quoted: {s}"
        );
        assert!(s.contains(",yes,"));
        assert!(
            s.ends_with(",yes,\n"),
            "observed and empty prev_hash should be exported: {s}"
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
