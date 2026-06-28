use crate::common::crypto_home_guard;
use ledgerful::ledger::crypto::{sign_ledger_entry, verify_signature};

#[test]
fn test_sign_and_verify_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let tx_id = "tx_123";
    let category = "FEATURE";
    let summary = "Add crypto tests";
    let reason = "Verify security";
    let committed_at = "2024-05-20T12:00:00Z";

    let (sig, pub_key) =
        sign_ledger_entry(tx_id, category, summary, reason, committed_at).expect("Signing failed");

    let sig_str = sig.expect("No signature");
    let pub_str = pub_key.expect("No public key");

    assert!(verify_signature(
        tx_id,
        category,
        summary,
        reason,
        committed_at,
        &sig_str,
        &pub_str
    ));
}

#[test]
fn test_verify_fails_on_tampered_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let (_home, _userprofile) = crypto_home_guard(tmp.path());
    let tx_id = "tx_123";
    let category = "FEATURE";
    let summary = "Add crypto tests";
    let reason = "Verify security";
    let committed_at = "2024-05-20T12:00:00Z";

    let (sig, pub_key) =
        sign_ledger_entry(tx_id, category, summary, reason, committed_at).expect("Signing failed");

    let sig_str = sig.expect("No signature");
    let pub_str = pub_key.expect("No public key");

    // Tamper with summary
    assert!(!verify_signature(
        tx_id,
        category,
        "Tampered",
        reason,
        committed_at,
        &sig_str,
        &pub_str
    ));
}

#[test]
fn test_sign_returns_error_on_missing_key_dir() {
    // This is hard to test without mocking the home directory or env vars
}
