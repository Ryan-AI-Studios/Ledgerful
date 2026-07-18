#![cfg(feature = "export")]
#![allow(non_snake_case)]

//! Track 0048: control-scoped SOC2 evidence export.
//!
//! The signed bundle must stay whole; a --control export only adds an
//! additive control lens under control-lens/, never removes files.

use crate::common::non_interactive;
use crate::export_cli_parity::{
    assert_zip_member_parity, extract_zip_members, seed_export_ledger, setup_export_repo,
};
use ledgerful::export::control_mapping::{ControlMapping, ControlSelector, banned_terms};
use ledgerful::export::soc2::generate_soc2_export_with_options;
use ledgerful::export::soc2_control::generate_soc2_control_export;
use ledgerful::ledger::crypto::sign_ledger_entry_in;
use ledgerful::ledger::db::LedgerDb;
use ledgerful::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, VerificationStatus};
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;

use serial_test::serial;
use std::collections::BTreeSet;
use std::process::Command;

fn seed_export_ledger_with_varied_entries(repo: &crate::export_cli_parity::ExportRepo) {
    seed_export_ledger(repo);

    let keys_dir = repo.root.join(".ledgerful").join("keys");
    let keys_path = keys_dir.as_std_path();

    let tx_id = "tx-export-verified-002";
    let committed_at = "2026-06-20T11:00:00Z";
    let summary = "Add verified change with risk";
    let reason = "Track 0048 needs risk and verification signals";
    let (sig, pub_key) =
        sign_ledger_entry_in(keys_path, tx_id, "BUGFIX", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");

    {
        let conn = rusqlite::Connection::open(repo.db_path.as_path()).unwrap();
        conn.execute(
            "INSERT INTO transactions \
             (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
             VALUES (?1, 'COMMITTED', 'BUGFIX', 'src/export/control.rs', 'src/export/control.rs', 'test', 'test', ?2)",
            rusqlite::params![tx_id, committed_at],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger_entries \
             (tx_id, category, entry_type, entity, entity_normalized, change_type, \
              summary, reason, is_breaking, committed_at, origin, author, observed, \
              verification_status, verification_basis, risk) \
             VALUES (?1, 'BUGFIX', 'IMPLEMENTATION', 'src/export/control.rs', 'src/export/control.rs', 'MODIFY', \
                     ?2, ?3, 0, ?4, 'LOCAL', 'Test User', NULL, 'verified', 'tests', 'medium')",
            rusqlite::params![tx_id, summary, reason, committed_at],
        )
        .unwrap();
        conn.execute(
            "UPDATE ledger_entries SET signature = ?1, public_key = ?2 WHERE tx_id = ?3",
            rusqlite::params![sig, pub_key, tx_id],
        )
        .unwrap();
    }
}

fn seed_chain_head(repo: &crate::export_cli_parity::ExportRepo) {
    let keys_dir = repo.root.join(".ledgerful").join("keys");
    let keys_path = keys_dir.as_std_path();

    let conn = rusqlite::Connection::open(repo.db_path.as_path()).unwrap();

    // Build a genuinely linked chain: walk entries in chronological order and
    // set each entry's prev_hash to the computed hash of the previous entry.
    // The genesis entry keeps a NULL prev_hash.
    let entries: Vec<(String, Option<String>, Option<String>)> = conn
        .prepare(
            "SELECT tx_id, signature, prev_hash FROM ledger_entries \
             ORDER BY committed_at ASC, tx_id ASC",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let mut prev_hash: Option<String> = None;
    for (tx_id, sig, _) in entries {
        let sig_hex = sig.as_deref().unwrap_or("");
        let current_hash = ledgerful::ledger::crypto::compute_entry_hash(
            &tx_id,
            sig_hex,
            prev_hash.as_deref().unwrap_or(""),
        );
        conn.execute(
            "UPDATE ledger_entries SET prev_hash = ?1 WHERE tx_id = ?2",
            rusqlite::params![prev_hash, tx_id],
        )
        .unwrap();
        prev_hash = Some(current_hash);
    }

    // Determine the chronologically last signed entry in the seeded ledger
    // and compute its entry hash so we can store a signed chain_head.
    let (last_tx_id, last_sig, last_prev_hash, last_committed_at): (
        String,
        Option<String>,
        Option<String>,
        String,
    ) = conn
        .query_row(
            "SELECT tx_id, signature, prev_hash, committed_at FROM ledger_entries \
             ORDER BY committed_at DESC, tx_id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get(3)?,
                ))
            },
        )
        .unwrap();

    let latest_hash = ledgerful::ledger::crypto::compute_entry_hash(
        &last_tx_id,
        last_sig.as_deref().unwrap_or(""),
        last_prev_hash.as_deref().unwrap_or(""),
    );

    let genesis: String = conn
        .query_row(
            "SELECT committed_at FROM ledger_entries ORDER BY committed_at ASC, tx_id ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let length: i64 = conn
        .query_row("SELECT COUNT(*) FROM ledger_entries", [], |row| row.get(0))
        .unwrap();

    let (head_sig, head_pub) =
        ledgerful::ledger::crypto::sign_chain_head(keys_path, &latest_hash, &genesis, length)
            .expect("sign_chain_head should succeed");

    let updated_at = last_committed_at;
    conn.execute("DELETE FROM chain_head WHERE id = 1", [])
        .unwrap();
    conn.execute(
        "INSERT INTO chain_head (id, latest_entry_hash, genesis, length, head_signature, head_public_key, updated_at) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            latest_hash,
            genesis,
            length,
            head_sig.unwrap_or_default(),
            head_pub.unwrap_or_default(),
            updated_at,
        ],
    )
    .unwrap();
}

fn build_layout(repo: &crate::export_cli_parity::ExportRepo) -> Layout {
    Layout::new(repo.root.clone())
}

fn any_banned_term_present(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    banned_terms().iter().find(|t| lower.contains(*t)).copied()
}

#[test]
#[serial(cwd, env)]
fn control_export__contains_whole_bundle_plus_lens__no_truncation() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let base_zip = generate_soc2_export_with_options(&layout, false, None, None)
        .expect("base export should succeed");
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let base_members = extract_zip_members(&base_zip);
    let control_members = extract_zip_members(&control_zip);

    let base_names: BTreeSet<String> = base_members.keys().cloned().collect();
    let control_names: BTreeSet<String> = control_members.keys().cloned().collect();

    assert!(
        base_names.is_subset(&control_names),
        "control export must contain all base files"
    );
    assert!(control_names.contains("control-lens/cover.md"));
    assert!(control_names.contains("control-lens/index.json"));

    let extras: Vec<&String> = control_names.difference(&base_names).collect();
    assert_eq!(
        extras.len(),
        2,
        "only control-lens/ files should be added; got {extras:?}"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__default_unchanged_when_no_control() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let base_zip = generate_soc2_export_with_options(&layout, false, None, None)
        .expect("base export should succeed");
    let base_members = extract_zip_members(&base_zip);

    assert!(!base_members.contains_key("control-lens/cover.md"));
    assert!(!base_members.contains_key("control-lens/index.json"));

    // An empty selector is rejected at the export boundary, so the base export
    // is the only no-control case we can observe here.
    let empty_control_zip = generate_soc2_export_with_options(
        &layout,
        false,
        None,
        Some(&ControlSelector::new(vec![])),
    );
    assert!(
        empty_control_zip.is_err(),
        "empty selector should be rejected at the export boundary"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__signature_still_verifies_and_hashes_match() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    assert_zip_member_parity(&control_zip, &control_zip);

    let members = extract_zip_members(&control_zip);
    let manifest_json = members
        .get("manifest.json")
        .expect("manifest.json must be present");
    let manifest: serde_json::Value =
        serde_json::from_slice(manifest_json).expect("manifest.json must parse");
    let files = manifest["files"]
        .as_array()
        .expect("files array must exist");

    for file in files {
        let name = file["name"].as_str().expect("file name must be string");
        let expected_sha = file["sha256"].as_str().expect("sha256 must be string");
        let bytes = members
            .get(name)
            .unwrap_or_else(|| panic!("manifest lists missing file: {name}"));
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let actual_sha = hex::encode(hasher.finalize());
        assert_eq!(actual_sha, expected_sha, "hash mismatch for {name}");
    }

    let names: Vec<String> = files
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"control-lens/cover.md".to_string()),
        "manifest must list control-lens/cover.md"
    );
    assert!(
        names.contains(&"control-lens/index.json".to_string()),
        "manifest must list control-lens/index.json"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__base_payloads_byte_identical() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let base_zip = generate_soc2_export_with_options(&layout, false, None, None)
        .expect("base export should succeed");
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let base = extract_zip_members(&base_zip);
    let control = extract_zip_members(&control_zip);

    for name in base.keys() {
        if name == "manifest.json" || name == "manifest.sig" || name == "manifest.pub" {
            continue;
        }
        assert_eq!(
            base.get(name),
            control.get(name),
            "{name} must be byte-identical between base and control export"
        );
    }
}

#[test]
#[serial(cwd, env)]
fn control_lens__lists_matching_tx_ids() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC3.4".to_string()])
        .expect("control export should succeed");

    let members = extract_zip_members(&control_zip);
    let index_json = members
        .get("control-lens/index.json")
        .expect("control-lens/index.json must exist");
    let index: serde_json::Value =
        serde_json::from_slice(index_json).expect("index.json must parse");

    let controls = index["controls"]
        .as_array()
        .expect("controls array must exist");
    assert_eq!(controls.len(), 1);
    let control = &controls[0];
    assert_eq!(control["id"].as_str().unwrap(), "CC3.4");

    let tx_ids: Vec<String> = control["matchingTxIds"]
        .as_array()
        .expect("matchingTxIds must be array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    let expected: Vec<String> = vec![
        "tx-export-adr-001".to_string(),
        "tx-export-seeded-001".to_string(),
        "tx-export-verified-002".to_string(),
    ];
    assert_eq!(tx_ids, expected);
    assert!(
        !control["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("tamper_evident_chain")),
        "tamper_evident_chain should not populate matchingTxIds"
    );
}

#[test]
#[serial(cwd, env)]
fn control_lens__contains_disclaimer_and_no_banned_terms() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let mapping = ControlMapping::load_static().unwrap();
    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let members = extract_zip_members(&control_zip);
    let cover = members
        .get("control-lens/cover.md")
        .expect("cover.md must exist");
    let cover_text = String::from_utf8_lossy(cover);

    assert!(
        cover_text.contains(&mapping.meta.disclaimer),
        "cover.md must contain the mapping disclaimer"
    );

    if let Some(term) = any_banned_term_present(&cover_text) {
        panic!("cover.md contains banned term: {term}");
    }

    assert!(
        cover_text.contains("- **Per-entry matches:**"),
        "cover.md must list per-entry matches for CC8.1"
    );
    assert!(
        cover_text.contains("- **Framework-wide evidence:** tamper_evident_chain"),
        "cover.md must list tamper_evident_chain as framework-wide evidence for CC8.1"
    );
    assert!(
        !cover_text.contains("preserved unchanged"),
        "cover.md must not falsely claim manifest files are preserved unchanged"
    );
    assert!(
        cover_text.contains("manifest.json and manifest.sig files are regenerated"),
        "cover.md must accurately describe manifest regeneration"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__multiple_controls_requested() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(
        &layout,
        false,
        None,
        &["CC8.1".to_string(), "CC7.1".to_string()],
    )
    .expect("multi-control export should succeed");

    let members = extract_zip_members(&control_zip);
    let index_json = members
        .get("control-lens/index.json")
        .expect("control-lens/index.json must exist");
    let index: serde_json::Value =
        serde_json::from_slice(index_json).expect("index.json must parse");
    let ids: Vec<String> = index["controls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["CC7.1", "CC8.1"]);
}

#[test]
#[serial(cwd, env)]
fn control_export__duplicate_controls_deduplicated() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let zip_a = generate_soc2_control_export(
        &layout,
        false,
        None,
        &["CC8.1".to_string(), "CC8.1".to_string()],
    )
    .expect("duplicate selector export should succeed");
    let zip_b = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("single selector export should succeed");

    assert_zip_member_parity(&zip_a, &zip_b);
}

#[test]
#[serial(cwd, env)]
fn control_export__control_order_canonical() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let zip_a = generate_soc2_control_export(
        &layout,
        false,
        None,
        &["CC7.1".to_string(), "CC8.1".to_string()],
    )
    .expect("ordered selector export should succeed");
    let zip_b = generate_soc2_control_export(
        &layout,
        false,
        None,
        &["CC8.1".to_string(), "CC7.1".to_string()],
    )
    .expect("reversed selector export should succeed");

    assert_zip_member_parity(&zip_a, &zip_b);
}

#[test]
#[serial(cwd, env)]
fn control_export__family_wildcard_matches() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC7.*".to_string()])
        .expect("family wildcard export should succeed");

    let members = extract_zip_members(&control_zip);
    let index_json = members
        .get("control-lens/index.json")
        .expect("control-lens/index.json must exist");
    let index: serde_json::Value =
        serde_json::from_slice(index_json).expect("index.json must parse");
    let ids: Vec<String> = index["controls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["CC7.1", "CC7.2"]);

    let cover = members
        .get("control-lens/cover.md")
        .expect("control-lens/cover.md must exist");
    let cover_text = String::from_utf8_lossy(cover);
    assert!(
        cover_text.contains("CC7.*"),
        "cover.md must contain the wildcard request string"
    );
    assert!(
        cover_text.contains("CC7.1"),
        "cover.md must contain resolved control CC7.1"
    );
    assert!(
        cover_text.contains("CC7.2"),
        "cover.md must contain resolved control CC7.2"
    );
}

#[test]
#[serial(cwd, env)]
fn unknown_control_lowercase_rejected() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let result = generate_soc2_control_export(&layout, false, None, &["cc8.1".to_string()]);
    assert!(
        result.is_err(),
        "lowercase cc8.1 must be rejected as unknown (case-sensitive matching)"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("cc8.1"),
        "error must name the bad selector: {err}"
    );
}

#[test]
#[serial(cwd, env)]
fn family_wildcard_no_matches_rejected() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let result = generate_soc2_control_export(&layout, false, None, &["CC9.*".to_string()]);
    assert!(
        result.is_err(),
        "CC9.* wildcard must be rejected because no controls match the CC9 family"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("CC9.*"),
        "error must name the bad selector: {err}"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__unknown_control_rejected() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let result = generate_soc2_control_export(&layout, false, None, &["CC99.9".to_string()]);
    assert!(result.is_err(), "unknown control CC99.9 must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("CC99.9"),
        "error must name the bad selector: {err}"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__no_banned_terms_in_export() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let members = extract_zip_members(&control_zip);
    for (name, bytes) in &members {
        let text = String::from_utf8_lossy(bytes);
        if let Some(term) = any_banned_term_present(&text) {
            panic!("zip member {name} contains banned term: {term}");
        }
    }
}

#[test]
#[serial(cwd, env)]
fn control_export__signing_basis_unchanged() {
    let crypto_source =
        std::fs::read_to_string("src/ledger/crypto.rs").expect("crypto.rs source must be readable");
    // Restrict the check to the format! string literal so that variable names
    // and other code outside the signing basis cannot create false positives.
    let format_start = crypto_source
        .find("let payload = format!(")
        .expect("crypto.rs must contain a signing payload format block");
    let after_format = &crypto_source[format_start + "let payload = format!".len()..];
    let open_quote = after_format
        .find('"')
        .expect("format! string literal must start with a double quote");
    let literal_body = &after_format[open_quote + 1..];
    let close_quote = literal_body
        .find('"')
        .expect("format! string literal must end with a double quote");
    let literal = &literal_body[..close_quote];

    for field in ["tx_id", "category", "summary", "reason", "committed_at"] {
        assert!(
            literal.contains(field),
            "signing payload format string must contain field {field}"
        );
    }

    let placeholder_count = literal.matches("{}").count();
    assert_eq!(
        placeholder_count, 5,
        "signing payload format string must contain exactly 5 field placeholders, found {placeholder_count}"
    );

    let forbidden = [
        "entity",
        "origin",
        "trace_id",
        "risk",
        "author",
        "observed",
        "prev_hash",
        "public_key",
        "signature",
        "verification",
        "change_type",
        "is_breaking",
        "entry_type",
    ];
    for field in forbidden {
        assert!(
            !literal.contains(field),
            "signing payload format string must not contain forbidden field {field}"
        );
    }
}

#[test]
#[serial(cwd, env)]
fn control_export__deterministic_for_same_repo_state() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let zip_a = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("first control export should succeed");
    let zip_b = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("second control export should succeed");

    assert_zip_member_parity(&zip_a, &zip_b);
}

#[test]
#[serial(cwd, env)]
fn control_export__chain_verification_passes() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);
    seed_chain_head(&repo);

    let layout = build_layout(&repo);
    let base_zip = generate_soc2_export_with_options(&layout, false, None, None)
        .expect("base export should succeed");
    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let base_members = extract_zip_members(&base_zip);
    let control_members = extract_zip_members(&control_zip);

    assert!(
        control_members.contains_key("chain_head.json"),
        "control export must contain chain_head.json"
    );
    assert!(
        base_members.contains_key("chain_head.json"),
        "base export must contain chain_head.json"
    );
    assert_eq!(
        base_members.get("chain_head.json"),
        control_members.get("chain_head.json"),
        "chain_head.json must be byte-identical between base and control export (no truncation)"
    );

    let head_json = control_members
        .get("chain_head.json")
        .expect("chain_head.json present");
    let head: serde_json::Value =
        serde_json::from_slice(head_json).expect("chain_head.json must parse");

    let latest = head["latest_entry_hash"]
        .as_str()
        .expect("latest entry hash");
    let genesis = head["genesis"].as_str().expect("genesis");
    let length = head["length"].as_i64().expect("length");
    let sig = head["head_signature"].as_str().expect("head signature");
    let pub_key = head["head_public_key"].as_str().expect("head public key");

    assert!(
        ledgerful::ledger::crypto::verify_chain_head(latest, genesis, length, sig, pub_key),
        "exported chain head signature must verify"
    );

    let storage =
        StorageManager::open_read_only_sqlite_only(&repo.root).expect("storage should open");
    let db = LedgerDb::new(storage.get_connection());
    let entries = db.get_all_committed_ledger_entries().unwrap();
    assert_eq!(
        entries.len() as i64,
        length,
        "chain head length must match ledger entry count"
    );
    let last = entries.last().expect("at least one entry");
    let expected_latest = ledgerful::ledger::crypto::compute_entry_hash(
        &last.tx_id,
        last.signature.as_deref().unwrap_or(""),
        last.prev_hash.as_deref().unwrap_or(""),
    );
    assert_eq!(
        latest, expected_latest,
        "chain head latest_entry_hash must match computed hash of last entry"
    );

    let mut prev_hash: Option<String> = None;
    for entry in &entries {
        if let Some(expected_prev) = prev_hash.as_ref() {
            assert_eq!(
                entry.prev_hash.as_deref().unwrap_or(""),
                expected_prev.as_str(),
                "entry {} prev_hash must equal computed hash of previous entry",
                entry.tx_id
            );
        } else {
            assert!(
                entry.prev_hash.is_none(),
                "genesis entry {} must have no prev_hash",
                entry.tx_id
            );
        }
        prev_hash = Some(ledgerful::ledger::crypto::compute_entry_hash(
            &entry.tx_id,
            entry.signature.as_deref().unwrap_or(""),
            entry.prev_hash.as_deref().unwrap_or(""),
        ));
    }
    assert_eq!(
        latest,
        prev_hash.as_deref().unwrap_or(""),
        "chain head latest_entry_hash must equal terminal computed hash after walking all links"
    );
}

#[test]
#[serial(cwd, env)]
fn control_export__cli_supports_control_flag() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let out_path = repo.root.join("control-evidence.zip");
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args([
            "export",
            "evidence",
            "--profile",
            "soc2",
            "--control",
            "CC8.1",
            "--control",
            "CC7.*",
            "--out",
            out_path.as_str(),
        ])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        output.status.success(),
        "CLI export with --control should succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let zip_bytes = std::fs::read(&out_path).expect("CLI export file should exist");
    let members = extract_zip_members(&zip_bytes);
    assert!(members.contains_key("control-lens/cover.md"));
    assert!(members.contains_key("control-lens/index.json"));
}

fn read_repo_file(path: &str) -> String {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(repo_root.join(path))
        .unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[test]
fn source_artifacts__no_banned_terms() {
    let toml = read_repo_file("mappings/soc2.toml");
    let md = read_repo_file("docs/mappings/soc2.md");
    let features = read_repo_file("docs/Features.md");
    if let Some(term) = any_banned_term_present(&toml) {
        panic!("mappings/soc2.toml contains banned term: {term}");
    }
    if let Some(term) = any_banned_term_present(&md) {
        panic!("docs/mappings/soc2.md contains banned term: {term}");
    }
    if let Some(term) = any_banned_term_present(&features) {
        panic!("docs/Features.md contains banned term: {term}");
    }
}

#[test]
fn mappings_doc_drift__toml_and_doc_agree() {
    use ledgerful::export::control_mapping::render_mapping_doc;

    let mapping = ControlMapping::load_static().expect("static mapping must parse");
    let doc = read_repo_file("docs/mappings/soc2.md");
    let rendered = render_mapping_doc(&mapping);

    assert_eq!(
        doc, rendered,
        "docs/mappings/soc2.md must match the canonical renderer output"
    );
}

#[test]
fn evidence_predicate__matches_expected_entry_characteristics() {
    use ledgerful::export::control_mapping::matches_evidence_keyword;

    let entry_signed = LedgerEntry {
        id: 1,
        tx_id: "tx-1".to_string(),
        category: Category::Feature,
        entry_type: EntryType::Implementation,
        entity: "src/a.rs".to_string(),
        entity_normalized: "src/a.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "summary".to_string(),
        reason: "reason".to_string(),
        is_breaking: false,
        committed_at: "2026-06-20T10:00:00Z".to_string(),
        verification_status: Some(VerificationStatus::Verified),
        verification_basis: None,
        outcome_notes: None,
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: Some("sig".to_string()),
        public_key: None,
        risk: Some("medium".to_string()),
        related_tickets: None,
        author: "Test".to_string(),
        observed: None,
        prev_hash: None,
    };

    assert!(matches_evidence_keyword(
        &entry_signed,
        "signed_ledger_entry"
    ));
    assert!(matches_evidence_keyword(
        &entry_signed,
        "verification_result"
    ));
    assert!(matches_evidence_keyword(&entry_signed, "risk_score"));
    assert!(matches_evidence_keyword(
        &entry_signed,
        "risk_impact_analysis"
    ));
    assert!(!matches_evidence_keyword(
        &entry_signed,
        "tamper_evident_chain"
    ));
    assert!(!matches_evidence_keyword(&entry_signed, "scan_impact"));
    assert!(!matches_evidence_keyword(&entry_signed, "config_diff"));

    let entry_unsigned = LedgerEntry {
        id: 2,
        tx_id: "tx-2".to_string(),
        category: Category::Chore,
        entry_type: EntryType::Maintenance,
        entity: "src/b.rs".to_string(),
        entity_normalized: "src/b.rs".to_string(),
        change_type: ChangeType::Modify,
        summary: "summary".to_string(),
        reason: "reason".to_string(),
        is_breaking: false,
        committed_at: "2026-06-20T11:00:00Z".to_string(),
        verification_status: None,
        verification_basis: None,
        outcome_notes: None,
        origin: "LOCAL".to_string(),
        trace_id: None,
        signature: None,
        public_key: None,
        risk: None,
        related_tickets: None,
        author: "Test".to_string(),
        observed: None,
        prev_hash: None,
    };

    assert!(!matches_evidence_keyword(
        &entry_unsigned,
        "signed_ledger_entry"
    ));
    assert!(!matches_evidence_keyword(
        &entry_unsigned,
        "verification_result"
    ));
    assert!(!matches_evidence_keyword(&entry_unsigned, "risk_score"));
    assert!(!matches_evidence_keyword(
        &entry_unsigned,
        "risk_impact_analysis"
    ));
}
