use ledgerful::config::model::Config;
use ledgerful::ledger::*;
use ledgerful::state::storage::StorageManager;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tempfile::{TempDir, tempdir};

/// Returns the absolute path to a template SQLite database file that has been
/// fully migrated. Tests copy this file into their own tempdirs instead of
/// re-running migrations each time.
fn template_db_path() -> &'static Path {
    static TEMPLATE_DIR: OnceLock<TempDir> = OnceLock::new();
    static TEMPLATE_DB_PATH: OnceLock<PathBuf> = OnceLock::new();
    TEMPLATE_DB_PATH.get_or_init(|| {
        let dir = TEMPLATE_DIR.get_or_init(|| tempdir().unwrap());
        let db_path = dir.path().join("ledger.db");
        let _ = StorageManager::init(&db_path).unwrap();
        db_path
    })
}

/// Open a `StorageManager` in the given tempdir by copying the pre-migrated
/// template `.db` file instead of running migrations.
fn storage_in_dir(dir: &Path) -> StorageManager {
    let db_path = dir.join("ledger.db");
    std::fs::copy(template_db_path(), &db_path).unwrap();
    StorageManager::init(&db_path).unwrap()
}

#[test]
fn test_register_tech_stack_rule() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    let rule = TechStackRule {
        category: "DATABASE".to_string(),
        name: "SQLite".to_string(),
        version_constraint: Some(">=3.35.0".to_string()),
        rules: vec![
            "NO JSONB columns".to_string(),
            "NO stored procedures".to_string(),
        ],
        locked: true,
        status: "ACTIVE".to_string(),
        entity_type: "FILE".to_string(),
        registered_at: "2026-01-01T00:00:00Z".to_string(),
    };

    db.insert_tech_stack_rule(&rule)
        .expect("Should insert rule");

    let rules = db.get_tech_stack_rules(None).expect("Should get rules");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].category, "DATABASE");
    assert_eq!(rules[0].name, "SQLite");
    assert_eq!(rules[0].rules.len(), 2);
    assert!(rules[0].locked);

    let single_rule = db
        .get_tech_stack_rule("DATABASE")
        .expect("Should get single rule")
        .unwrap();
    assert_eq!(single_rule.name, "SQLite");
}

#[test]
fn test_register_commit_validator() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    let validator = CommitValidator {
        id: None,
        category: "FEATURE".to_string(),
        name: "cargo-check".to_string(),
        description: Some("Run cargo check on changed entity".to_string()),
        executable: "cargo".to_string(),
        args: vec!["check".to_string(), "{entity}".to_string()],
        timeout_ms: 30000,
        glob: Some("src/**/*.rs".to_string()),
        validation_level: ValidationLevel::Error,
        enabled: true,
    };

    db.insert_commit_validator(&validator)
        .expect("Should insert validator");

    let validators = db
        .get_commit_validators(None)
        .expect("Should get validators");
    assert_eq!(validators.len(), 1);
    assert_eq!(validators[0].name, "cargo-check");
    assert_eq!(validators[0].args.len(), 2);
    assert_eq!(validators[0].validation_level, ValidationLevel::Error);
}

#[test]
fn test_register_category_mapping() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    // Insert the required referenced tech stack rule first
    let rule = TechStackRule {
        category: "BACKEND_LANG".to_string(),
        name: "Rust".to_string(),
        version_constraint: None,
        rules: vec![],
        locked: false,
        status: "ACTIVE".to_string(),
        entity_type: "FILE".to_string(),
        registered_at: "2026-01-01T00:00:00Z".to_string(),
    };
    db.insert_tech_stack_rule(&rule)
        .expect("Should insert rule");

    let mapping = CategoryStackMapping {
        id: None,
        ledger_category: "ARCHITECTURE".to_string(),
        stack_category: "BACKEND_LANG".to_string(),
        glob: None,
        description: Some("Backend language constraints".to_string()),
    };

    db.insert_category_mapping(&mapping)
        .expect("Should insert mapping");

    let mappings = db.get_category_mappings(None).expect("Should get mappings");
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].ledger_category, "ARCHITECTURE");
    assert_eq!(mappings[0].stack_category, "BACKEND_LANG");
}

#[test]
fn test_register_watcher_pattern() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    let pattern = WatcherPattern {
        id: None,
        glob: "**/Cargo.toml".to_string(),
        category: "INFRA".to_string(),
        source: "DB".to_string(),
        description: Some("Cargo config drift".to_string()),
    };

    db.insert_watcher_pattern(&pattern)
        .expect("Should insert pattern");

    let patterns = db.get_watcher_patterns().expect("Should get patterns");
    assert_eq!(patterns.len(), 1);
    assert_eq!(patterns[0].glob, "**/Cargo.toml");
}

#[test]
fn test_serde_defaults() {
    let payload = r#"{"category": "TEST_CAT", "name": "Test Rule"}"#;
    let rule: TechStackRule = serde_json::from_str(payload).unwrap();
    assert_eq!(rule.category, "TEST_CAT");
    assert_eq!(rule.name, "Test Rule");
    assert_eq!(rule.status, "ACTIVE");
    assert_eq!(rule.entity_type, "FILE");
    assert!(rule.rules.is_empty());
    assert!(!rule.locked);

    let validator_payload = r#"{"name": "test-v", "executable": "ls"}"#;
    let validator: CommitValidator = serde_json::from_str(validator_payload).unwrap();
    assert_eq!(validator.name, "test-v");
    assert_eq!(validator.executable, "ls");
    assert_eq!(validator.category, "ALL");
    assert_eq!(validator.timeout_ms, 5000);
    assert!(validator.enabled);
}

#[test]
fn test_foreign_key_enforcement() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    let mapping = CategoryStackMapping {
        id: None,
        ledger_category: "ARCH".to_string(),
        stack_category: "NON_EXISTENT".to_string(),
        glob: None,
        description: None,
    };

    // This should fail because "NON_EXISTENT" is not in tech_stack.category
    let result = db.insert_category_mapping(&mapping);
    assert!(result.is_err());
}

#[test]
fn test_category_filtering() {
    let dir = tempdir().unwrap();
    let storage = storage_in_dir(dir.path());
    let db = LedgerDb::new(storage.get_connection());

    db.insert_tech_stack_rule(&TechStackRule {
        category: "CAT1".to_string(),
        name: "Name1".to_string(),
        ..Default::default()
    })
    .unwrap();

    db.insert_tech_stack_rule(&TechStackRule {
        category: "CAT2".to_string(),
        name: "Name2".to_string(),
        ..Default::default()
    })
    .unwrap();

    let all = db.get_tech_stack_rules(None).unwrap();
    assert_eq!(all.len(), 2);

    let cat1 = db.get_tech_stack_rules(Some("CAT1")).unwrap();
    assert_eq!(cat1.len(), 1);
    assert_eq!(cat1[0].name, "Name1");
}

#[test]
fn test_tech_stack_enforcement_at_start() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());

    {
        let db = LedgerDb::new(storage.get_connection_mut());
        // 1. Register a tech stack rule with a forbidden term
        db.insert_tech_stack_rule(&TechStackRule {
            category: "DATABASE".to_string(),
            name: "Postgres".to_string(),
            rules: vec![
                "NO stored procedures".to_string(),
                "NO triggers".to_string(),
            ],
            ..Default::default()
        })
        .expect("Should insert rule");

        // 2. Map FEATURE to DATABASE
        db.insert_category_mapping(&CategoryStackMapping {
            ledger_category: "FEATURE".to_string(),
            stack_category: "DATABASE".to_string(),
            ..Default::default()
        })
        .expect("Should insert mapping");
    }

    let mut manager = TransactionManager::new(&mut storage, dir.path().to_path_buf(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg.ledger.enforcement_enabled = true;
        cfg
    });

    // Create a dummy file so normalization works
    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    // 3. Start a change that violates the rule
    let req = TransactionRequest {
        operation_id: None,
        category: Category::Feature,
        entity: "main.rs".to_string(),
        planned_action: Some("Add STORED PROCEDURES for user auth".to_string()), // uppercase to test case-insensitivity
        source: None,
        issue_ref: None,
    };

    let result = manager.start_change(req);

    // 4. Assert it is blocked with RuleViolation
    match result {
        Err(LedgerError::RuleViolation(msg)) => {
            assert!(msg.contains("forbidden term: stored procedures"));
        }
        _ => panic!("Expected RuleViolation error, got {:?}", result),
    }

    // 5. Test another forbidden term
    let req2 = TransactionRequest {
        operation_id: None,
        category: Category::Feature,
        entity: "main.rs".to_string(),
        planned_action: Some("Add triggers for logging".to_string()),
        source: None,
        issue_ref: None,
    };
    let result2 = manager.start_change(req2);
    match result2 {
        Err(LedgerError::RuleViolation(msg)) => {
            assert!(msg.contains("forbidden term: triggers"));
        }
        _ => panic!("Expected RuleViolation error, got {:?}", result2),
    }

    // 6. Valid change should pass
    let req_ok = TransactionRequest {
        operation_id: None,
        category: Category::Feature,
        entity: "main.rs".to_string(),
        planned_action: Some("Add new API endpoint".to_string()),
        source: None,
        issue_ref: None,
    };
    let result_ok = manager.start_change(req_ok);
    assert!(result_ok.is_ok());
}

fn get_failing_validator() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell".to_string(),
            vec!["-Command".to_string(), "exit 1".to_string()],
        )
    } else {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "exit 1".to_string()],
        )
    }
}

fn get_timeout_validator() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell".to_string(),
            vec!["-Command".to_string(), "Start-Sleep -Seconds 2".to_string()],
        )
    } else {
        ("sleep".to_string(), vec!["2".to_string()])
    }
}

fn get_path_validator() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "Write-Output $args[0]; exit 1".to_string(),
                "{entity}".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "echo $0; exit 1".to_string(),
                "{entity}".to_string(),
            ],
        )
    }
}

#[test]
fn test_commit_validator_blocking() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    // 1. Register a failing ERROR validator for FEATURE
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let conn = storage.get_connection();
        let db = LedgerDb::new(conn);

        let (exe, args) = get_failing_validator();

        db.insert_commit_validator(&CommitValidator {
            category: "FEATURE".to_string(),
            name: "fail-validator".to_string(),
            executable: exe,
            args,
            validation_level: ValidationLevel::Error,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    }

    let mut storage_mut = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage_mut, dir.path().to_path_buf(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg
    });

    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    // 2. Start transaction
    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // 3. Attempt commit - should be blocked with ValidatorFailed
    let result = manager.commit_change(
        tx_id,
        CommitRequest {
            summary: "test commit".to_string(),
            ..Default::default()
        },
        false,
    );

    match result {
        Err(LedgerError::ValidatorFailed(name, _msg)) => {
            assert_eq!(name, "fail-validator");
        }
        _ => panic!("Expected ValidatorFailed error, got {:?}", result),
    }
}

#[test]
fn test_commit_validator_warning() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    // 1. Register a failing WARNING validator
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let db = LedgerDb::new(storage.get_connection());

        let (exe, args) = get_failing_validator();

        db.insert_commit_validator(&CommitValidator {
            category: "FEATURE".to_string(),
            name: "warn-validator".to_string(),
            executable: exe,
            args,
            validation_level: ValidationLevel::Warning,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    }

    let mut storage_mut = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage_mut, dir.path().to_path_buf(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg
    });

    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // 2. Attempt commit - should pass despite failure
    let result = manager.commit_change(
        tx_id,
        CommitRequest {
            summary: "test commit".to_string(),
            ..Default::default()
        },
        false,
    );

    assert!(result.is_ok());
}

#[test]
fn test_commit_validator_timeout() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    // 1. Register a timeout validator
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let db = LedgerDb::new(storage.get_connection());

        let (exe, args) = get_timeout_validator();

        db.insert_commit_validator(&CommitValidator {
            category: "FEATURE".to_string(),
            name: "timeout-validator".to_string(),
            executable: exe,
            args,
            timeout_ms: 100, // Very short timeout
            validation_level: ValidationLevel::Error,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    }

    let mut storage_mut = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage_mut, dir.path().to_path_buf(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg
    });

    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // 2. Attempt commit - should be blocked due to timeout
    let result = manager.commit_change(
        tx_id,
        CommitRequest {
            summary: "test commit".to_string(),
            ..Default::default()
        },
        false,
    );

    match result {
        Err(LedgerError::ValidatorFailed(name, msg)) => {
            assert_eq!(name, "timeout-validator");
            assert!(msg.contains("Validator timed out"));
        }
        _ => panic!(
            "Expected ValidatorFailed error due to timeout, got {:?}",
            result
        ),
    }
}

#[test]
fn test_commit_validator_absolute_path() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    // 1. Register a validator that prints its argument (the entity path)
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let db = LedgerDb::new(storage.get_connection());

        let (exe, args) = get_path_validator();

        db.insert_commit_validator(&CommitValidator {
            category: "FEATURE".to_string(),
            name: "path-validator".to_string(),
            executable: exe,
            args,
            validation_level: ValidationLevel::Error,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    }

    let mut storage_mut = StorageManager::init(&db_path).unwrap();
    let repo_root = dir.path().to_path_buf();
    let mut manager = TransactionManager::new(&mut storage_mut, repo_root.clone(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg
    });

    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // 2. Attempt commit
    let result = manager.commit_change(
        tx_id,
        CommitRequest {
            summary: "test commit".to_string(),
            ..Default::default()
        },
        false,
    );

    match result {
        Err(LedgerError::ValidatorFailed(_, msg)) => {
            // Check if the output contains the absolute path
            let expected_abs_path = repo_root.join("main.rs");
            let expected_str = expected_abs_path.to_string_lossy().to_string();

            // Normalize path for comparison (case and slashes)
            let msg_lower = msg.to_lowercase().replace('\\', "/");
            let expected_lower = expected_str.to_lowercase().replace('\\', "/");

            assert!(
                msg_lower.contains(&expected_lower),
                "Message did not contain absolute path. Msg: {}, Expected: {}",
                msg,
                expected_str
            );
        }
        _ => panic!("Expected ValidatorFailed error, got {:?}", result),
    }
}

#[test]
fn test_all_category_validators() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ledger.db");
    // 1. Register an 'ALL' category validator
    {
        let storage = StorageManager::init(&db_path).unwrap();
        let db = LedgerDb::new(storage.get_connection());

        let (exe, args) = get_failing_validator();

        db.insert_commit_validator(&CommitValidator {
            category: "ALL".to_string(),
            name: "global-validator".to_string(),
            executable: exe,
            args,
            validation_level: ValidationLevel::Error,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    }

    let mut storage_mut = StorageManager::init(&db_path).unwrap();
    let mut manager = TransactionManager::new(&mut storage_mut, dir.path().to_path_buf(), {
        let mut cfg = Config::default();
        cfg.gate.mode = "enforce".to_string();
        cfg
    });

    std::fs::write(dir.path().join("main.rs"), "").unwrap();

    // 2. Start transaction for a specific category (BUGFIX)
    let tx_id = manager
        .start_change(TransactionRequest {
            category: Category::Bugfix,
            entity: "main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // 3. Attempt commit - should be blocked by the 'global-validator'
    let result = manager.commit_change(
        tx_id,
        CommitRequest {
            summary: "test commit".to_string(),
            ..Default::default()
        },
        false,
    );

    match result {
        Err(LedgerError::ValidatorFailed(name, _)) => {
            assert_eq!(name, "global-validator");
        }
        _ => panic!(
            "Expected ValidatorFailed error from global-validator, got {:?}",
            result
        ),
    }
}

#[test]
fn test_verification_gate_blocks_high_risk_categories() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());
    let repo_root = dir.path().to_path_buf();

    // Create entity files
    for entity in ["src/main.rs", "src/lib.rs"] {
        let entity_path = repo_root.join(entity);
        std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
        std::fs::write(&entity_path, "").unwrap();
    }

    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, config);

    // Test that Architecture category requires verification
    for category in [
        Category::Architecture,
        Category::Feature,
        Category::Bugfix,
        Category::Infra,
    ] {
        let entity = "src/main.rs";
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category,
                entity: entity.to_string(),
                ..Default::default()
            })
            .unwrap();

        // Commit without verification_status should fail
        let result = tx_mgr.commit_change(
            tx_id.clone(),
            CommitRequest {
                summary: "test commit".to_string(),
                reason: "testing".to_string(),
                ..Default::default()
            },
            false,
        );

        match result {
            Err(LedgerError::VerificationRequired(cat)) => {
                assert_eq!(
                    cat,
                    format!("{} (both status and basis required)", category)
                );
            }
            _ => panic!(
                "Expected VerificationRequired for {:?}, got {:?}",
                category, result
            ),
        }

        // Clean up: rollback so we can reuse the entity
        tx_mgr
            .rollback_change(tx_id, "test rollback".to_string())
            .unwrap();
    }
}

#[test]
fn test_verification_gate_allows_with_status() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // Commit WITH verification_status should succeed
    let result = tx_mgr.commit_change(
        tx_id,
        CommitRequest {
            summary: "Architectural change".to_string(),
            reason: "Refactored core module".to_string(),
            verification_status: Some(VerificationStatus::Verified),
            verification_basis: Some(VerificationBasis::Tests),
            ..Default::default()
        },
        false,
    );

    assert!(result.is_ok(), "Should succeed with verification status");
}

#[test]
fn test_verification_gate_force_override() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/main.rs");
    std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
    std::fs::write(&entity_path, "").unwrap();

    let mut config = Config::default();
    config.gate.mode = "enforce".to_string();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Feature,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // Commit with force=true should bypass the verification gate
    let result = tx_mgr.commit_change(
        tx_id,
        CommitRequest {
            summary: "Feature change".to_string(),
            reason: "New feature".to_string(),
            ..Default::default()
        },
        true,
    );

    assert!(result.is_ok(), "Should succeed with --force override");
}

#[test]
fn test_verification_gate_disabled_by_default() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());
    let repo_root = dir.path().to_path_buf();

    let entity_path = repo_root.join("src/main.rs");
    let _ = std::fs::create_dir_all(entity_path.parent().unwrap());
    std::fs::write(&entity_path, "").unwrap();

    // Default config has verify_to_commit = false
    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, Config::default());

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Architecture,
            entity: "src/main.rs".to_string(),
            ..Default::default()
        })
        .unwrap();

    // Should succeed without verification when gate is disabled
    let result = tx_mgr.commit_change(
        tx_id,
        CommitRequest {
            summary: "Architectural change".to_string(),
            reason: "Refactored".to_string(),
            ..Default::default()
        },
        false,
    );

    assert!(
        result.is_ok(),
        "Should succeed when verify_to_commit is disabled (default)"
    );
}

#[test]
fn test_verification_gate_allows_low_risk_categories() {
    let dir = tempdir().unwrap();
    let mut storage = storage_in_dir(dir.path());
    let repo_root = dir.path().to_path_buf();

    for (entity, _category) in [
        ("docs/readme.md", Category::Docs),
        ("chore.log", Category::Chore),
        ("refactor.rs", Category::Refactor),
        ("tooling.sh", Category::Tooling),
    ] {
        let entity_path = repo_root.join(entity);
        std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
        std::fs::write(&entity_path, "").unwrap();
    }

    let mut config = Config::default();
    config.ledger.verify_to_commit = true;

    let mut tx_mgr = TransactionManager::new(&mut storage, repo_root, config);

    // Low-risk categories should not require verification
    for (entity, category) in [
        ("docs/readme.md", Category::Docs),
        ("chore.log", Category::Chore),
        ("refactor.rs", Category::Refactor),
        ("tooling.sh", Category::Tooling),
    ] {
        let tx_id = tx_mgr
            .start_change(TransactionRequest {
                category,
                entity: entity.to_string(),
                ..Default::default()
            })
            .unwrap();

        let result = tx_mgr.commit_change(
            tx_id,
            CommitRequest {
                summary: format!("{:?} change", category),
                reason: "testing".to_string(),
                ..Default::default()
            },
            false,
        );

        assert!(
            result.is_ok(),
            "{:?} should not require verification",
            category
        );
    }
}
