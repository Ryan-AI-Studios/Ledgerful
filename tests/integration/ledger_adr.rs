#![allow(non_snake_case)]

use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_ledger_adr_export() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    let repo_root = dir.path().to_path_buf();

    // Create files so canonicalize works
    fs::create_dir_all(repo_root.join("docs")).unwrap();
    fs::write(repo_root.join("docs/arch.md"), "").unwrap();
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(repo_root.join("src/api.rs"), "").unwrap();

    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage, repo_root.clone(), Config::default());

    // 1. Create an ARCHITECTURE entry
    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/arch.md".to_string(),
            ..Default::default()
        })
        .unwrap();

    manager
        .commit_change(
            tx_id,
            CommitRequest {
                summary: "New system architecture".to_string(),
                reason: "Scalability requirements".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // 2. Create a breaking FEATURE entry
    let tx_id2 = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/api.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    manager
        .commit_change(
            tx_id2,
            CommitRequest {
                summary: "Breaking API change".to_string(),
                reason: "Refactoring for clarity".to_string(),
                is_breaking: true,
                ..Default::default()
            },
            false,
        )
        .unwrap();

    // 3. Export ADRs
    let output_dir = repo_root.join("docs/adr");

    let entries = manager.get_adr_entries(None).unwrap();
    assert_eq!(entries.len(), 2);

    fs::create_dir_all(&output_dir).unwrap();
    for entry in &entries {
        let slug = ledgerful::ledger::adr::slugify_summary(&entry.summary);
        let filename = format!("{:04}-{}.md", entry.id, slug);
        let file_path = output_dir.join(filename);
        let content = ledgerful::ledger::adr::generate_madr_content(
            entry,
            ledgerful::ledger::types::AdrStatus::Proposed,
        );
        fs::write(&file_path, content).unwrap();
    }

    // 4. Verify files exist and content
    let files: Vec<_> = fs::read_dir(&output_dir)
        .unwrap()
        .map(|r| r.unwrap().file_name())
        .collect();
    println!("Exported files: {:?}", files);
    assert_eq!(files.len(), 2);

    let arch_entry = entries
        .iter()
        .find(|e| e.category == Category::Architecture)
        .expect("Architecture entry not found");
    let breaking_entry = entries
        .iter()
        .find(|e| e.is_breaking)
        .expect("Breaking entry not found");

    let arch_slug = ledgerful::ledger::adr::slugify_summary(&arch_entry.summary);
    let arch_filename = format!("{:04}-{}.md", arch_entry.id, arch_slug);
    let arch_file = output_dir.join(&arch_filename);

    assert!(
        arch_file.exists(),
        "Architecture file {} does not exist",
        arch_filename
    );
    let content = fs::read_to_string(arch_file).unwrap();
    assert!(content.contains("# 1. New system architecture"));
    assert!(content.contains("- **Status**: proposed"));
    assert!(content.contains("- **Change type**:"));
    assert!(content.contains("- **Category**: Architecture"));

    let breaking_slug = ledgerful::ledger::adr::slugify_summary(&breaking_entry.summary);
    let breaking_filename = format!("{:04}-{}.md", breaking_entry.id, breaking_slug);
    let breaking_file = output_dir.join(&breaking_filename);
    assert!(
        breaking_file.exists(),
        "Breaking file {} does not exist",
        breaking_filename
    );
}

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};
use camino::Utf8Path;
use ledgerful::cli::AdrSubcommands;
use ledgerful::commands::init::execute_init;
use ledgerful::commands::ledger_adr::execute_ledger_adr;
use ledgerful::export::soc2::generate_soc2_export;
use ledgerful::ledger::types::AdrStatus;
use ledgerful::state::layout::Layout;
use serial_test::serial;
use std::io::Read;
use std::process::Command;

fn adr_bin() -> &'static str {
    option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful")
}

fn setup_adr_cli_repo() -> (
    tempfile::TempDir,
    camino::Utf8PathBuf,
    DirGuard,
    TempEnv,
    TempEnv,
) {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());
    let root = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();
    let cwd = DirGuard::from_utf8(&root);
    let home = TempEnv::set("HOME", dir.path().to_str().unwrap());
    let profile = TempEnv::set("USERPROFILE", dir.path().to_str().unwrap());
    execute_init(false, false).unwrap();
    (dir, root, cwd, home, profile)
}

fn commit_architecture_entry(root: &camino::Utf8Path) -> String {
    fs::create_dir_all(root.join("docs").as_std_path()).unwrap();
    fs::write(root.join("docs/arch.md").as_std_path(), "adr").unwrap();
    let db_path = root
        .join(".ledgerful")
        .join("state")
        .join("ledger.db")
        .into_std_path_buf();
    let mut storage = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(
        &mut storage,
        root.as_std_path().to_path_buf(),
        Config::default(),
    );
    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "docs/arch.md".to_string(),
            ..Default::default()
        })
        .unwrap();
    manager
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                summary: "New system architecture".to_string(),
                reason: "Scalability requirements".to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    tx_id
}

#[test]
#[serial(cwd, env)]
fn ledger_adr_export__update_status_accepted__disk_lifecycle() {
    let _ni = non_interactive();
    let (_dir, root, _cwd, _home, _profile) = setup_adr_cli_repo();
    let tx_id = commit_architecture_entry(&root);

    let first_out = root.join("adr-out-1");
    fs::create_dir_all(first_out.as_std_path()).unwrap();
    execute_ledger_adr(AdrSubcommands::Export {
        output: first_out.to_string(),
        days: None,
    })
    .expect("first export");
    let first = read_first_madr(first_out.as_std_path());
    assert!(
        first.contains("- **Status**: proposed"),
        "absent metadata defaults proposed: {first}"
    );
    assert!(first.contains("- **Change type**:"));

    execute_ledger_adr(AdrSubcommands::UpdateStatus {
        adr_id: tx_id,
        status: AdrStatus::Accepted,
    })
    .expect("update-status accepted");

    let second_out = root.join("adr-out-2");
    fs::create_dir_all(second_out.as_std_path()).unwrap();
    execute_ledger_adr(AdrSubcommands::Export {
        output: second_out.to_string(),
        days: None,
    })
    .expect("second export");
    let second = read_first_madr(second_out.as_std_path());
    assert!(
        second.contains("- **Status**: accepted"),
        "export must read update-status: {second}"
    );
    assert!(
        second.contains("- **Change type**:"),
        "change type line required: {second}"
    );
    assert!(
        !second.contains("- **Status**: MODIFY"),
        "change type must not occupy Status: {second}"
    );

    let list = Command::new(adr_bin())
        .args(["ledger", "adr", "list", "--json"])
        .current_dir(root.as_std_path())
        .output()
        .expect("adr list --json");
    assert!(list.status.success());
    let list_json = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_json.contains("\"kind\":\"ledgerAdr\"")
            || list_json.contains("\"kind\": \"ledgerAdr\""),
        "0320 list keep-green: {list_json}"
    );
    assert!(list_json.contains("schemaVersion") || list_json.contains("\"schema_version\""));
}

#[test]
#[serial(cwd, env)]
fn ledger_adr_export__empty_set__banner_and_no_files() {
    let _ni = non_interactive();
    let (_dir, root, _cwd, _home, _profile) = setup_adr_cli_repo();
    let out = root.join("adr-empty");
    fs::create_dir_all(out.as_std_path()).unwrap();
    execute_ledger_adr(AdrSubcommands::Export {
        output: out.to_string(),
        days: None,
    })
    .expect("empty export succeeds");
    let leftover: Vec<_> = fs::read_dir(out.as_std_path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "empty export must write no files: {leftover:?}"
    );
}

#[test]
#[serial(cwd, env)]
fn ledger_adr_export__bogus_metadata_status__fails_closed() {
    let _ni = non_interactive();
    let (_dir, root, _cwd, _home, _profile) = setup_adr_cli_repo();
    let tx_id = commit_architecture_entry(&root);
    execute_ledger_adr(AdrSubcommands::UpdateStatus {
        adr_id: tx_id.clone(),
        status: AdrStatus::Accepted,
    })
    .expect("seed metadata row");

    let db_path = root
        .join(".ledgerful")
        .join("state")
        .join("ledger.db")
        .into_std_path_buf();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE adr_metadata SET status = 'not-a-status' WHERE adr_id = ?1",
        [&tx_id],
    )
    .unwrap();
    drop(conn);

    let out = root.join("adr-bogus");
    fs::create_dir_all(out.as_std_path()).unwrap();
    let result = execute_ledger_adr(AdrSubcommands::Export {
        output: out.to_string(),
        days: None,
    });
    assert!(result.is_err(), "unknown stored status must fail export");
    let leftover: Vec<_> = fs::read_dir(out.as_std_path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "fail-closed export must write nothing: {leftover:?}"
    );
}

#[test]
#[serial(cwd, env)]
fn soc2_zip_adr_md__uses_lifecycle_status() {
    let _ni = non_interactive();
    let (_dir, root, _cwd, _home, _profile) = setup_adr_cli_repo();
    let tx_id = commit_architecture_entry(&root);
    execute_ledger_adr(AdrSubcommands::UpdateStatus {
        adr_id: tx_id,
        status: AdrStatus::Accepted,
    })
    .expect("update-status");

    let layout = Layout::new(root.clone());
    let zip_bytes = generate_soc2_export(&layout).expect("soc2 zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
    let mut found = false;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().to_string();
        if name.starts_with("adr/") && name.ends_with(".md") {
            let mut body = String::new();
            file.read_to_string(&mut body).unwrap();
            assert!(
                body.contains("- **Status**: accepted"),
                "{name} Status: {body}"
            );
            assert!(
                body.contains("- **Change type**:"),
                "{name} missing Change type: {body}"
            );
            found = true;
        }
    }
    assert!(found, "expected at least one adr/*.md in SOC2 zip");
}

fn read_first_madr(dir: &std::path::Path) -> String {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    let path = files.first().expect("expected a MADR file");
    fs::read_to_string(path).unwrap()
}
