use crate::common::{DirGuard, non_interactive, setup_git_repo};
use camino::Utf8Path;
use ed25519_dalek::{Signature, VerifyingKey};
use ledgerful::ledger::{compute_author_pseudonym as hmac_pseudonym, verify_manifest_signature};
use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const REDACTED_FIELDS: &[&str] = &[
    "\"id\":",
    "\"entry_type\":",
    "\"entity\":",
    "\"entity_normalized\":",
    "\"change_type\":",
    "\"is_breaking\":",
    "\"outcome_notes\":",
    "\"origin\":",
    "\"trace_id\":",
    "\"related_tickets\":",
    "\"prev_hash\":",
    "\"author\":",
];

fn run_ledgerful_binary(dir: &Path, args: &[&str]) {
    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(args)
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("ledgerful binary should be runnable");
    assert!(
        output.status.success(),
        "ledgerful command failed: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn export_public_in_repo(repo: &Path, output_dir: &str, sign: bool) {
    let mut args = vec!["ledger", "export-public", "--output", output_dir];
    if sign {
        args.push("--sign");
    }
    run_ledgerful_binary(repo, &args);
}

fn export_public_in_repo_with_key(repo: &Path, output_dir: &str, key_dir: &Path) {
    let key_arg = key_dir.to_string_lossy().to_string();
    let args = vec![
        "ledger",
        "export-public",
        "--output",
        output_dir,
        "--sign",
        "--key",
        &key_arg,
    ];
    run_ledgerful_binary(repo, &args);
}

fn read_bundle_file(repo: &Path, output_dir: &str, name: &str) -> Vec<u8> {
    let path = repo.join(output_dir).join(name);
    fs::read(&path).unwrap_or_else(|e| panic!("Failed to read {name}: {e}"))
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__two_runs_same_repo__entries_ndjson_byte_identical() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    export_public_in_repo(tmp.path(), "bundle1", false);
    export_public_in_repo(tmp.path(), "bundle2", false);

    let first_entries = read_bundle_file(tmp.path(), "bundle1", "entries.ndjson");
    let second_entries = read_bundle_file(tmp.path(), "bundle2", "entries.ndjson");
    assert_eq!(
        first_entries, second_entries,
        "entries.ndjson must be byte-identical across deterministic re-runs"
    );

    let first_manifest = read_bundle_file(tmp.path(), "bundle1", "manifest.json");
    let second_manifest = read_bundle_file(tmp.path(), "bundle2", "manifest.json");
    assert_eq!(
        first_manifest, second_manifest,
        "manifest.json must be byte-identical across deterministic re-runs (unsigned path)"
    );
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__signed_output__manifest_signature_verifies_with_bot_key() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    let bot_keys_dir = tmp.path().join("bot-keys");
    fs::create_dir_all(&bot_keys_dir).unwrap();

    export_public_in_repo_with_key(tmp.path(), "bundle-signed", &bot_keys_dir);

    let manifest_json = read_bundle_file(tmp.path(), "bundle-signed", "manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_json).expect("manifest.json must parse");
    let signature = manifest["signature"]
        .as_str()
        .expect("signature must be present");
    let public_key = manifest["publicKey"]
        .as_str()
        .expect("public key must be present");

    assert!(
        verify_manifest_signature(&manifest_json, signature, public_key),
        "manifest signature must verify against published bot public key"
    );

    // Bot key files must exist in the temp directory, never falling back to home.
    assert!(bot_keys_dir.join("ledgerful-ledger-bot.key").exists());
    assert!(bot_keys_dir.join("ledgerful-ledger-bot.pub").exists());
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__allowlist_enforcement__no_redacted_fields_in_entries_ndjson() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    export_public_in_repo(tmp.path(), "bundle-allowlist", false);

    let entries_text = String::from_utf8(read_bundle_file(
        tmp.path(),
        "bundle-allowlist",
        "entries.ndjson",
    ))
    .unwrap();

    for field in REDACTED_FIELDS {
        assert!(
            !entries_text.contains(field),
            "redacted field '{field}' leaked into entries.ndjson"
        );
    }
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__pseudonym_properties__keyed_hash_and_secret_not_in_bundle() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    let bot_keys_dir = tmp.path().join("bot-keys");
    fs::create_dir_all(&bot_keys_dir).unwrap();

    export_public_in_repo_with_key(tmp.path(), "bundle-pseudonym", &bot_keys_dir);

    // Read the generated HMAC secret and assert it never appears in bundle files.
    let secret = fs::read(bot_keys_dir.join("pseudonym-secret.key"))
        .expect("pseudonym secret should be created");
    let secret_hex = hex::encode(&secret);

    let manifest: serde_json::Value = serde_json::from_slice(&read_bundle_file(
        tmp.path(),
        "bundle-pseudonym",
        "manifest.json",
    ))
    .unwrap();
    let entry_count = manifest["entryCount"].as_u64().unwrap_or(0) as usize;
    assert!(
        entry_count > 0,
        "test needs at least one entry to inspect pseudonyms"
    );

    let entries_text = String::from_utf8(read_bundle_file(
        tmp.path(),
        "bundle-pseudonym",
        "entries.ndjson",
    ))
    .unwrap();
    let first_line = entries_text.lines().next().unwrap();
    let entry: serde_json::Value = serde_json::from_str(first_line).unwrap();
    let exported_pseudonym = entry["author_pseudonym"].as_str().unwrap();

    // Compute expected pseudonym from the actual secret and the raw author.
    // setup_git_repo configures git user.name = "Test User", which is what
    // the post-commit hook captures as the ledger entry author.
    let raw_author = "Test User";
    let expected = hmac_pseudonym(&secret, raw_author).expect("test HMAC should succeed");
    assert_eq!(
        exported_pseudonym, expected,
        "exported author_pseudonym must match HMAC-SHA256(secret, author)"
    );

    let bare_sha256 = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(raw_author.as_bytes());
        hex::encode(hasher.finalize())
    };
    assert_ne!(
        exported_pseudonym, bare_sha256,
        "author_pseudonym must not be a bare sha256(author)"
    );

    // Same secret + author yields the same pseudonym (correlatable).
    assert_eq!(
        hmac_pseudonym(&secret, raw_author).expect("test HMAC should succeed"),
        expected
    );

    // Different author with same secret differs.
    assert_ne!(
        hmac_pseudonym(&secret, "Other Author").expect("test HMAC should succeed"),
        expected
    );

    for name in [
        "manifest.json",
        "entries.ndjson",
        "index.html",
        "verifier.html",
        "README.md",
    ] {
        let contents = String::from_utf8(read_bundle_file(tmp.path(), "bundle-pseudonym", name))
            .unwrap_or_default();
        assert!(
            !contents.contains(&secret_hex),
            "pseudonym secret hex leaked into bundle file {name}"
        );
    }
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__empty_ledger__empty_bundle_with_zero_entries() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    // Clear the ledger_entries table so we can test the empty-ledger path.
    let db_path = tmp
        .path()
        .join(".ledgerful")
        .join("state")
        .join("ledger.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute("DELETE FROM ledger_entries", []).unwrap();
    conn.execute("DELETE FROM chain_head", []).unwrap();

    export_public_in_repo(tmp.path(), "bundle-empty", false);

    let entries = read_bundle_file(tmp.path(), "bundle-empty", "entries.ndjson");
    assert!(
        entries.is_empty(),
        "empty ledger must produce empty entries.ndjson"
    );

    let manifest: serde_json::Value = serde_json::from_slice(&read_bundle_file(
        tmp.path(),
        "bundle-empty",
        "manifest.json",
    ))
    .unwrap();
    assert_eq!(
        manifest["entryCount"].as_u64(),
        Some(0),
        "manifest must report entryCount 0"
    );
    assert!(
        manifest["chainHead"].is_null(),
        "empty ledger must have null chainHead"
    );
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__entries_sha256_matches_file_hash() {
    use sha2::{Digest, Sha256};

    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    export_public_in_repo(tmp.path(), "bundle-sha256", false);

    let entries_bytes = read_bundle_file(tmp.path(), "bundle-sha256", "entries.ndjson");
    let manifest: serde_json::Value = serde_json::from_slice(&read_bundle_file(
        tmp.path(),
        "bundle-sha256",
        "manifest.json",
    ))
    .unwrap();

    let manifest_hash = manifest["entriesSha256"]
        .as_str()
        .expect("manifest must include entriesSha256");

    let mut hasher = Sha256::new();
    hasher.update(&entries_bytes);
    let computed_hash = hex::encode(hasher.finalize());

    assert_eq!(
        manifest_hash, computed_hash,
        "manifest.entriesSha256 must equal SHA-256 of entries.ndjson bytes"
    );
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__with_chain_head__manifest_includes_chain_head() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    export_public_in_repo(tmp.path(), "bundle-chain", false);

    let manifest: serde_json::Value = serde_json::from_slice(&read_bundle_file(
        tmp.path(),
        "bundle-chain",
        "manifest.json",
    ))
    .unwrap();
    assert!(
        manifest.get("chainHead").is_some(),
        "manifest must include chainHead field"
    );
    assert!(
        !manifest["chainHead"].is_null(),
        "manifest chainHead must not be null when ledger has entries"
    );
    assert!(
        manifest["chainHead"]["head_signature"].is_string(),
        "chain head must carry a signature"
    );
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__does_not_mutate_ledger__verify_signatures_stable() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    let before = run_ledgerful_verify_signatures(tmp.path());
    export_public_in_repo(tmp.path(), "bundle-verify-check", false);
    let after = run_ledgerful_verify_signatures(tmp.path());

    assert_eq!(
        before, after,
        "ledgerful verify --signatures --chain output must be identical before and after export-public"
    );
}

fn run_ledgerful_verify_signatures(dir: &Path) -> Vec<u8> {
    let ledgerful_bin = std::env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(ledgerful_bin)
        .args(["verify", "--signatures", "--chain"])
        .current_dir(dir)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("verify command should run");
    assert!(
        output.status.success(),
        "verify --signatures --chain must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut captured = Vec::new();
    captured.extend_from_slice(&output.stdout);
    captured.extend_from_slice(&output.stderr);
    captured
}

fn verify_entry_signature(entry: &serde_json::Value) {
    let public_key_hex = entry["public_key"]
        .as_str()
        .expect("entry must have a public_key to verify");
    let signature_hex = entry["signature"]
        .as_str()
        .expect("entry must have a signature to verify");

    let pub_bytes = hex::decode(public_key_hex).expect("public_key must be valid hex");
    let pub_array: [u8; 32] = pub_bytes.try_into().expect("public_key must be 32 bytes");
    let verifying_key =
        VerifyingKey::from_bytes(&pub_array).expect("public_key must be valid Ed25519 key");

    let sig_bytes = hex::decode(signature_hex).expect("signature must be valid hex");
    let sig_array: [u8; 64] = sig_bytes.try_into().expect("signature must be 64 bytes");
    let signature = Signature::from_bytes(&sig_array);

    let payload = format!(
        "tx_id:{}\ncategory:{}\nsummary:{}\nreason:{}\ncommitted_at:{}",
        entry["tx_id"].as_str().unwrap_or(""),
        entry["category"].as_str().unwrap_or(""),
        entry["summary"].as_str().unwrap_or(""),
        entry["reason"].as_str().unwrap_or(""),
        entry["committed_at"].as_str().unwrap_or("")
    );

    verifying_key
        .verify_strict(payload.as_bytes(), &signature)
        .expect("entry signature must verify against reconstructed signing payload");
}

#[test]
#[serial(env, cwd)]
#[allow(non_snake_case)]
fn export_public__signed_output__entry_signature_verifies_with_public_key() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    run_ledgerful_binary(tmp.path(), &["init"]);

    fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    crate::common::git_add_and_commit_no_verify(tmp.path(), "initial commit");

    let bot_keys_dir = tmp.path().join("bot-keys");
    fs::create_dir_all(&bot_keys_dir).unwrap();

    export_public_in_repo_with_key(tmp.path(), "bundle-entry-sig", &bot_keys_dir);

    let entries_text = String::from_utf8(read_bundle_file(
        tmp.path(),
        "bundle-entry-sig",
        "entries.ndjson",
    ))
    .unwrap();

    let mut verified_count = 0;
    for line in entries_text.lines().filter(|l| !l.trim().is_empty()) {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        if entry["signature"].is_string() && entry["public_key"].is_string() {
            verify_entry_signature(&entry);
            verified_count += 1;
        }
    }

    assert!(
        verified_count >= 1,
        "at least one entry signature must verify; found {verified_count}"
    );
}
