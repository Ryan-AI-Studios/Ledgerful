#![allow(non_snake_case)]

//! Phase 1 of Track 0039: parity + path-safety tests for
//! `ledgerful export evidence --profile soc2`.

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};
use camino::Utf8Path;
use ledgerful::commands::init::execute_init;
use ledgerful::export::soc2::generate_soc2_export;
use ledgerful::ledger::crypto::sign_ledger_entry_in;
use ledgerful::state::layout::Layout;
use ledgerful::state::storage::StorageManager;
use serial_test::serial;
use std::collections::BTreeMap;
use std::process::Command;
use tempfile::tempdir;

/// Owns the temp directory + environment guards so the real home directory is
/// never touched and cwd is restored on drop.
pub(crate) struct ExportRepo {
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    pub(crate) root: camino::Utf8PathBuf,
    pub(crate) db_path: std::path::PathBuf,
    #[allow(dead_code)]
    _cwd_guard: DirGuard,
    #[allow(dead_code)]
    _home_guard: TempEnv,
    #[allow(dead_code)]
    _profile_guard: TempEnv,
}

pub(crate) fn setup_export_repo() -> ExportRepo {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root_utf8 = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();
    let cwd_guard = DirGuard::from_utf8(&root_utf8);

    // Keep keys/state inside the temp dir so tests never touch the real home.
    let home_guard = TempEnv::set("HOME", dir.path().to_str().unwrap());
    let profile_guard = TempEnv::set("USERPROFILE", dir.path().to_str().unwrap());

    execute_init(false, false).unwrap();

    let db_path = root_utf8
        .join(".ledgerful")
        .join("state")
        .join("ledger.db")
        .into_std_path_buf();

    ExportRepo {
        dir,
        root: root_utf8,
        db_path,
        _cwd_guard: cwd_guard,
        _home_guard: home_guard,
        _profile_guard: profile_guard,
    }
}

pub(crate) fn seed_export_ledger(repo: &ExportRepo) {
    let keys_dir = repo.root.join(".ledgerful").join("keys");
    let keys_path = keys_dir.as_std_path();

    let tx_id = "tx-export-seeded-001";
    let committed_at = "2026-06-20T10:00:00Z";
    let summary = "Add SOC2 CLI export";
    let reason = "Track 0039 requires a CLI evidence export";
    let (sig, pub_key) =
        sign_ledger_entry_in(keys_path, tx_id, "FEATURE", summary, reason, committed_at)
            .expect("sign_ledger_entry_in should succeed");

    let storage = StorageManager::init(repo.db_path.as_path()).unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO transactions \
         (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
         VALUES (?1, 'COMMITTED', 'FEATURE', 'src/export/cli.rs', 'src/export/cli.rs', 'test', 'test', ?2)",
        rusqlite::params![tx_id, committed_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries \
         (tx_id, category, entry_type, entity, entity_normalized, change_type, \
          summary, reason, is_breaking, committed_at, origin, author, observed) \
         VALUES (?1, 'FEATURE', 'IMPLEMENTATION', 'src/export/cli.rs', 'src/export/cli.rs', 'MODIFY', \
                 ?2, ?3, 0, ?4, 'LOCAL', 'Test User', NULL)",
        rusqlite::params![tx_id, summary, reason, committed_at],
    )
    .unwrap();

    // Seed an ADR entry so the export contains adr/*.md.
    conn.execute(
        "INSERT INTO transactions \
         (tx_id, status, category, entity, entity_normalized, session_id, source, started_at) \
         VALUES (?1, 'COMMITTED', 'ARCHITECTURE', 'entity', 'entity', 'test', 'test', ?2)",
        rusqlite::params!["tx-export-adr-001", "2026-06-20T08:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_entries \
         (tx_id, category, entry_type, entity, entity_normalized, change_type, \
          summary, reason, is_breaking, committed_at, origin, author, observed) \
         VALUES (?1, 'ARCHITECTURE', 'ARCHITECTURE', 'entity', 'entity', 'CREATE', \
                 'Use Ed25519 for export signatures', 'reason', 0, ?2, 'LOCAL', 'Test User', NULL)",
        rusqlite::params!["tx-export-adr-001", "2026-06-20T08:00:00Z"],
    )
    .unwrap();

    // Apply the signatures to the seeded entries. This mirrors the real commit
    // path and ensures the export contains signed entries.
    conn.execute(
        "UPDATE ledger_entries SET signature = ?1, public_key = ?2 WHERE tx_id = ?3",
        rusqlite::params![sig, pub_key, tx_id],
    )
    .unwrap();

    // Seed one verification run so verification_history.csv is non-trivial.
    conn.execute(
        "INSERT INTO verification_runs (timestamp, plan_json, overall_pass) VALUES (?1, '{}', 1)",
        rusqlite::params!["2026-06-20T09:00:00Z"],
    )
    .unwrap();
    let run_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO verification_results (run_id, command, exit_code, duration_ms, truncated) \
         VALUES (?1, ?2, 0, 123, 0)",
        rusqlite::params![run_id, "cargo nextest run"],
    )
    .unwrap();
}

/// Extract every file in a zip archive into a sorted map.
pub(crate) fn extract_zip_members(zip_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .expect("zip archive should be readable");
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry should be readable");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf)
            .expect("zip entry bytes should be readable");
        out.insert(file.name().to_string(), buf);
    }
    out
}

/// Replace the `generatedAt` timestamp in a manifest.json with a fixed sentinel
/// so two exports produced at different wall-clock times can be compared.
pub(crate) fn normalize_manifest_generated_at(manifest_bytes: &[u8]) -> Vec<u8> {
    let mut manifest: serde_json::Value =
        serde_json::from_slice(manifest_bytes).expect("manifest.json should parse as JSON");
    manifest
        .as_object_mut()
        .expect("manifest should be an object")
        .insert(
            "generatedAt".to_string(),
            serde_json::Value::String("NORMALIZED".to_string()),
        );
    // Re-serialize compactly, matching `serde_json::to_vec` in soc2.rs.
    serde_json::to_vec(&manifest).expect("manifest should re-serialize")
}

/// Assert that two zip byte streams contain byte-identical unzipped members.
///
/// `manifest.json#generatedAt` is a wall-clock timestamp, so it is normalized
/// before comparison. `manifest.sig` is a signature over the original
/// manifest bytes (including the timestamp), so the signatures themselves will
/// differ; instead we verify that each signature is valid for its own
/// manifest and that the public keys match. All other members must be
/// byte-identical.
pub(crate) fn assert_zip_member_parity(zip_a: &[u8], zip_b: &[u8]) {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let members_a = extract_zip_members(zip_a);
    let members_b = extract_zip_members(zip_b);

    let keys_a: Vec<_> = members_a.keys().cloned().collect();
    let keys_b: Vec<_> = members_b.keys().cloned().collect();
    assert_eq!(keys_a, keys_b, "zip member names must match");

    // Verify both signatures independently against their own manifests + pub keys.
    let pub_a = members_a.get("manifest.pub").expect("manifest.pub in A");
    let pub_b = members_b.get("manifest.pub").expect("manifest.pub in B");
    assert_eq!(
        pub_a, pub_b,
        "manifest.pub must be byte-identical (same key)"
    );
    assert_eq!(pub_a.len(), 32, "manifest.pub must be 32 bytes");

    let verifying_key = VerifyingKey::from_bytes(pub_a.as_slice().try_into().unwrap())
        .expect("manifest.pub must be a valid Ed25519 verifying key");

    for (label, members) in [("A", &members_a), ("B", &members_b)] {
        let manifest_json = members
            .get("manifest.json")
            .unwrap_or_else(|| panic!("manifest.json missing in {label}"));
        let sig = members
            .get("manifest.sig")
            .unwrap_or_else(|| panic!("manifest.sig missing in {label}"));
        assert_eq!(sig.len(), 64, "manifest.sig must be 64 bytes in {label}");
        let signature = Signature::from_bytes(sig.as_slice().try_into().unwrap());
        assert!(
            verifying_key.verify(manifest_json, &signature).is_ok(),
            "manifest.sig in {label} must verify against manifest.pub"
        );
    }

    for name in members_a.keys() {
        if name == "manifest.sig" {
            continue;
        }

        let a = members_a.get(name).unwrap();
        let b = members_b.get(name).unwrap();
        let a_norm = if name == "manifest.json" {
            normalize_manifest_generated_at(a)
        } else {
            a.clone()
        };
        let b_norm = if name == "manifest.json" {
            normalize_manifest_generated_at(b)
        } else {
            b.clone()
        };
        assert_eq!(
            a_norm, b_norm,
            "zip member {name} must be byte-identical after generatedAt normalization"
        );
    }
}

#[test]
#[serial(cwd, env)]
fn export_evidence__direct_call_twice__same_unzipped_members() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let layout = Layout::new(repo.root.clone());
    let zip_a = generate_soc2_export(&layout).expect("first export should succeed");
    let zip_b = generate_soc2_export(&layout).expect("second export should succeed");

    assert_zip_member_parity(&zip_a, &zip_b);
}

#[test]
#[serial(cwd, env)]
fn export_evidence__cli_matches_direct_call__same_unzipped_members() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let layout = Layout::new(repo.root.clone());
    let direct_zip = generate_soc2_export(&layout).expect("direct export should succeed");

    let out_path = repo.root.join("ledgerful-soc2-evidence.zip");
    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args([
            "export",
            "evidence",
            "--profile",
            "soc2",
            "--out",
            out_path.as_str(),
        ])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        output.status.success(),
        "ledgerful export evidence should succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let cli_zip = std::fs::read(&out_path).expect("CLI export file should exist");
    assert_zip_member_parity(&direct_zip, &cli_zip);
}

#[cfg(feature = "web")]
#[tokio::test]
#[serial(cwd, env)]
async fn export_evidence__web_matches_direct_call__same_unzipped_members() {
    use ledgerful::commands::web::auth::generate_token;
    use ledgerful::commands::web::server::router;
    use ledgerful::commands::web::state::AppState;
    use std::io::Read;
    use tokio::net::TcpListener;

    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let layout = Layout::new(repo.root.clone());
    let direct_zip = generate_soc2_export(&layout).expect("direct export should succeed");

    let token = generate_token();
    let state = std::sync::Arc::new(AppState::new(layout, token.clone(), None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    let serve = axum::serve(listener, app);
    let handle = tokio::spawn(async move {
        let _ = serve.await;
    });

    let url = format!("http://{}", addr);
    let web_zip = tokio::task::spawn_blocking(move || {
        let resp = ureq::get(&format!("{}/api/compliance/export", url))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .expect("web export request should succeed");
        assert_eq!(resp.status(), 200, "web export must return 200");
        let mut body = Vec::new();
        resp.into_reader().read_to_end(&mut body).unwrap();
        body
    })
    .await
    .unwrap();

    handle.abort();

    assert_zip_member_parity(&direct_zip, &web_zip);
}

#[test]
#[serial(cwd, env)]
fn export_evidence__cli_refuses_src_path() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    std::fs::create_dir_all(repo.root.join("src")).unwrap();

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(["export", "evidence", "--out", "src/evidence.zip"])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        !output.status.success(),
        "export to src/ must fail: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("inside src/"),
        "error must mention src/; got: {stderr}"
    );
}

#[test]
#[serial(cwd, env)]
fn export_evidence__cli_refuses_state_path() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args([
            "export",
            "evidence",
            "--out",
            ".ledgerful/state/evidence.zip",
        ])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        !output.status.success(),
        "export to .ledgerful/state/ must fail: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("inside .ledgerful/state/"),
        "error must mention .ledgerful/state/; got: {stderr}"
    );
}

#[test]
#[serial(cwd, env)]
fn export_evidence__cli_refuses_overwrite_without_force() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let existing = repo.root.join("existing-evidence.zip");
    std::fs::write(&existing, b"placeholder").unwrap();

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(["export", "evidence", "--out", existing.as_str()])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        !output.status.success(),
        "overwrite without --force must fail: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists"),
        "error must mention already exists; got: {stderr}"
    );
}

#[test]
#[serial(cwd, env)]
fn export_evidence__cli_allows_overwrite_with_force() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let existing = repo.root.join("existing-evidence.zip");
    std::fs::write(&existing, b"placeholder").unwrap();

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(["export", "evidence", "--out", existing.as_str(), "--force"])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        output.status.success(),
        "overwrite with --force must succeed: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
#[test]
#[serial(cwd, env)]
fn export_evidence__cli_refuses_symlink_to_state() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    // Create a junction/symlink inside the repo that points to .ledgerful/state.
    let state_dir = repo.root.join(".ledgerful").join("state");
    let link_path = repo.root.join("link_to_state");

    let symlink_ok = std::os::windows::fs::symlink_dir(&state_dir, &link_path);
    if symlink_ok.is_err() {
        let _ = std::fs::remove_dir_all(&link_path);
        if Command::new("cmd")
            .args(["/c", "mklink", "/J", link_path.as_str(), state_dir.as_str()])
            .output()
            .map(|out| !out.status.success())
            .unwrap_or(true)
        {
            // Symlinks/junctions unavailable in this environment; skip.
            return;
        }
    }

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(["export", "evidence", "--out", "link_to_state/evidence.zip"])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        !output.status.success(),
        "symlink-resolved state path must fail: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("inside .ledgerful/state/"),
        "error must mention .ledgerful/state/ after symlink resolution; got: {stderr}"
    );
}

#[cfg(not(windows))]
#[test]
#[serial(cwd, env)]
fn export_evidence__cli_refuses_symlink_to_state() {
    let _non_interactive = non_interactive();
    let repo = setup_export_repo();
    seed_export_ledger(&repo);

    let state_dir = repo.root.join(".ledgerful").join("state");
    let link_path = repo.root.join("link_to_state");

    if std::os::unix::fs::symlink(&state_dir, &link_path).is_err() {
        return;
    }

    let binary = env!("CARGO_BIN_EXE_ledgerful");
    let output = Command::new(binary)
        .args(["export", "evidence", "--out", "link_to_state/evidence.zip"])
        .output()
        .expect("ledgerful export evidence binary should run");

    assert!(
        !output.status.success(),
        "symlink-resolved state path must fail: stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("inside .ledgerful/state/"),
        "error must mention .ledgerful/state/ after symlink resolution; got: {stderr}"
    );
}
