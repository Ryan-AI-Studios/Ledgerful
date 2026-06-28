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
        let content = ledgerful::ledger::adr::generate_madr_content(entry);
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
