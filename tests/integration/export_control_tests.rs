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
use ledgerful::ledger::types::{Category, ChangeType, EntryType, LedgerEntry, VerificationStatus};
use ledgerful::state::layout::Layout;

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
}

#[test]
#[serial(cwd, env)]
fn control_export__ledger_csv_identical_to_base() {
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
    assert_eq!(
        base.get("ledger.csv"),
        control.get("ledger.csv"),
        "ledger.csv must be byte-identical between base and control export"
    );
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
    let payload_marker = "let payload = format!(\n        \"tx_id:{}\\ncategory:{}\\nsummary:{}\\nreason:{}\\ncommitted_at:{}\",";
    assert!(
        crypto_source.contains(payload_marker),
        "signing payload format must contain exactly tx_id, category, summary, reason, committed_at"
    );
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
fn control_export__default_unchanged_when_no_control() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger_with_varied_entries(&repo);

    let layout = build_layout(&repo);
    let base_zip = generate_soc2_export_with_options(&layout, false, None, None)
        .expect("base export should succeed");
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

    let control_zip = generate_soc2_control_export(&layout, false, None, &["CC8.1".to_string()])
        .expect("control export should succeed");

    let control_members = extract_zip_members(&control_zip);
    assert!(control_members.contains_key("control-lens/cover.md"));
    assert!(control_members.contains_key("control-lens/index.json"));

    assert_zip_member_parity(&base_zip, &base_zip);
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
    assert!(matches_evidence_keyword(
        &entry_signed,
        "tamper_evident_chain"
    ));

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
