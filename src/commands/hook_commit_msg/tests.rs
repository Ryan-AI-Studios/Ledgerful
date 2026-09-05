use super::*;

#[test]
fn skipped_coverage_summary_prefixed() {
    let s = skipped_coverage_summary("chore: fmt");
    assert!(s.starts_with(SKIPPED_SUMMARY_PREFIX));
    assert!(s.contains("chore: fmt"));
}

#[test]
fn skipped_coverage_risk_is_not_trivial() {
    // Promote maps TRIVIAL → verification_status None; SKIPPED must be Unverified.
    assert_ne!(SKIPPED_COVERAGE_RISK, "TRIVIAL");
}

#[test]
fn tui_skip_disposition_matches_s_key() {
    assert!(is_tui_skip_disposition("TRIVIAL", "Skipped intent entry"));
    assert!(!is_tui_skip_disposition("MEDIUM", "Skipped intent entry"));
    assert!(!is_tui_skip_disposition("TRIVIAL", "something else"));
}

// --- extract_ledger_tx_ref (0122) ---

const SAMPLE_UUID: &str = "d7f2e5e8-59b5-42fd-bcf3-d4ee99c507bf";

#[test]
fn extract_ledger_tx_ref_happy_ledger_line() {
    let msg = format!("[FEATURE] summary\n\nLedger: {SAMPLE_UUID}");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
}

#[test]
fn extract_ledger_tx_ref_case_insensitive() {
    let msg = format!("feat: x\n\nledger: {SAMPLE_UUID}");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
    let msg2 = format!("feat: x\n\nLEDGER: {SAMPLE_UUID}");
    assert_eq!(extract_ledger_tx_ref(&msg2).as_deref(), Some(SAMPLE_UUID));
}

#[test]
fn extract_ledger_tx_ref_ledger_tx_trailer() {
    let msg = format!("feat: x\n\nLedger-Tx: {SAMPLE_UUID}");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
}

#[test]
fn extract_ledger_tx_ref_prefers_ledger_over_ledger_tx() {
    let other = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let msg = format!("feat: x\n\nLedger: {SAMPLE_UUID}\nLedger-Tx: {other}");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
}

#[test]
fn extract_ledger_tx_ref_bare_uuid_alone_is_none() {
    assert_eq!(extract_ledger_tx_ref(SAMPLE_UUID), None);
    let prose = format!("Fixed bug {SAMPLE_UUID} in handler");
    assert_eq!(extract_ledger_tx_ref(&prose), None);
}

#[test]
fn extract_ledger_tx_ref_garbage_is_none() {
    assert_eq!(extract_ledger_tx_ref(""), None);
    assert_eq!(extract_ledger_tx_ref("feat: no ledger line"), None);
    assert_eq!(extract_ledger_tx_ref("Ledger: not-a-uuid"), None);
    assert_eq!(extract_ledger_tx_ref("Ledger: "), None);
}

#[test]
fn extract_ledger_tx_ref_rejects_reporting_status_string() {
    // Fixture: reporting.rs style compact status line must NOT match.
    let msg = "Ledger: 2 pending, 0 unaudited drift.";
    assert_eq!(extract_ledger_tx_ref(msg), None);
}

#[test]
fn extract_ledger_tx_ref_rejects_compact_status_with_work_root() {
    // 0200-A2: new compact form `Ledger [<workRoot>]: …` must not parse as a TX.
    // Raw string so `\d` is not a Rust escape.
    let msg = r"Ledger [C:\dev\ledgerful]: 2 pending, 0 unaudited drift.";
    assert_eq!(extract_ledger_tx_ref(msg), None);
}

#[test]
fn extract_ledger_tx_ref_rejects_verify_style_unaudited() {
    let msg = "Ledger: 3 unaudited…";
    assert_eq!(extract_ledger_tx_ref(msg), None);
    let msg2 = "Ledger: 1 unaudited drift item";
    assert_eq!(extract_ledger_tx_ref(msg2), None);
}

#[test]
fn extract_ledger_tx_ref_uuid_parse_str_rejects_non_uuid() {
    // Uuid::parse_str rejects non-UUID after Ledger: (end-of-line only path).
    assert!(uuid::Uuid::parse_str("2 pending, 0 unaudited drift.").is_err());
    assert_eq!(
        extract_ledger_tx_ref("Ledger: 2 pending, 0 unaudited drift."),
        None
    );
}

#[test]
fn extract_ledger_tx_ref_allows_surrounding_whitespace_on_line() {
    let msg = format!("  Ledger:   {SAMPLE_UUID}  ");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
}

#[test]
fn extract_ledger_tx_ref_non_ascii_lines_do_not_panic() {
    // `key.len()` for "Ledger:" is 7 bytes; multi-byte UTF-8 lines must not
    // panic when that index is not a char boundary (codex P1).
    assert_eq!(extract_ledger_tx_ref("é😊é"), None);
    assert_eq!(extract_ledger_tx_ref("日本語のコミット"), None);
    assert_eq!(extract_ledger_tx_ref("feat: café ☕\n\nbody"), None);
    // Valid Ledger: line still works when surrounded by non-ASCII.
    let msg = format!("feat: café\n\nLedger: {SAMPLE_UUID}\n\n日本語");
    assert_eq!(extract_ledger_tx_ref(&msg).as_deref(), Some(SAMPLE_UUID));
}

// --- classify_provenance_sot (0122) ---

#[test]
fn classify_provenance_sot_already_committed() {
    let class = classify_provenance_sot(Some(SAMPLE_UUID), &[], Some(LedgerRefStatus::Committed));
    assert_eq!(
        class,
        ProvenanceSotClass::AlreadyCommitted {
            tx_id: SAMPLE_UUID.to_string()
        }
    );
}

#[test]
fn classify_provenance_sot_link_pending_from_msg_ref() {
    let class = classify_provenance_sot(
        Some(SAMPLE_UUID),
        &[SAMPLE_UUID.to_string(), "other".into()],
        Some(LedgerRefStatus::Pending),
    );
    assert_eq!(
        class,
        ProvenanceSotClass::LinkPending {
            tx_id: SAMPLE_UUID.to_string()
        }
    );
}

#[test]
fn classify_provenance_sot_link_pending_single_global() {
    let class = classify_provenance_sot(None, &[SAMPLE_UUID.to_string()], None);
    assert_eq!(
        class,
        ProvenanceSotClass::LinkPending {
            tx_id: SAMPLE_UUID.to_string()
        }
    );
}

#[test]
fn classify_provenance_sot_ambiguous_multi_n2() {
    let pending = vec!["aaa".into(), "bbb".into()];
    let class = classify_provenance_sot(None, &pending, None);
    assert_eq!(class, ProvenanceSotClass::AmbiguousMulti);
}

#[test]
fn classify_provenance_sot_fallback_zero_pending() {
    let class = classify_provenance_sot(None, &[], None);
    assert_eq!(class, ProvenanceSotClass::Fallback);
}

#[test]
fn classify_provenance_sot_fallback_missing_ref() {
    let class = classify_provenance_sot(
        Some(SAMPLE_UUID),
        &[SAMPLE_UUID.to_string()],
        Some(LedgerRefStatus::Missing),
    );
    // Missing ref does not invent link even when a single pending exists.
    assert_eq!(class, ProvenanceSotClass::Fallback);
}
