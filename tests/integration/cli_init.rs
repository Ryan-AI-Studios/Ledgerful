use camino::Utf8Path;
use ledgerful::commands::init::execute_init;
use serial_test::serial;
use std::fs;
use tempfile::tempdir;

use crate::common::{DirGuard, TempEnv, non_interactive, setup_git_repo};

#[test]
#[serial(env, cwd)]
fn test_init_command_integration() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_init(false);
    assert!(result.is_ok());

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.exists());
    assert!(cg_dir.join("config.toml").exists());
    assert!(cg_dir.join("rules.toml").exists());
    assert!(cg_dir.join("logs").exists());

    let gitignore = root.join(".gitignore");
    assert!(gitignore.exists());
    let gitignore_content = fs::read_to_string(gitignore).unwrap();
    assert!(gitignore_content.contains(".ledgerful/"));

    let pre_commit = fs::read_to_string(root.join(".git").join("hooks").join("pre-commit"))
        .expect("pre-commit hook should be installed");
    assert!(pre_commit.contains("# ledgerful-ledger-gate"));
    assert!(
        pre_commit.contains("ledgerful ledger status --compact --exit-code --verify-signatures")
    );
    assert!(pre_commit.contains("git commit --no-verify"));

    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");
    assert!(pre_push.contains("# ledgerful-ledger-gate"));
    assert!(pre_push.contains("ledgerful ledger status --compact --exit-code --verify-signatures"));
    assert!(pre_push.contains("git push --no-verify"));
}

#[test]
#[serial(env, cwd)]
fn test_init_no_gitignore() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_init(true);
    assert!(result.is_ok());

    let cg_dir = root.join(".ledgerful");
    assert!(cg_dir.exists());
    assert!(!root.join(".gitignore").exists());
    assert!(root.join(".git").join("hooks").join("pre-commit").exists());
    assert!(root.join(".git").join("hooks").join("pre-push").exists());
}

#[test]
#[serial(env, cwd)]
fn test_init_uses_default_config_template_env() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let template = root.join("default-config.toml");

    fs::write(
        &template,
        "[core]\nstrict = true\nauto_fix = true\n\n[hotspots]\nlimit = 3\n",
    )
    .unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);
    let _env = TempEnv::set(
        "LEDGERFUL_DEFAULT_CONFIG",
        template.as_std_path().to_str().unwrap(),
    );

    let result = execute_init(false);
    assert!(result.is_ok());

    let config = fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(config.contains("strict = true"));
    assert!(config.contains("limit = 3"));
}

#[test]
#[serial(env, cwd)]
fn init_template_secret_is_omitted_even_without_gitignore() {
    const SENTINEL: &str = "TA33-INTEGRATION-SENTINEL";
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let template = root.join("default-config.toml");
    fs::write(
        &template,
        format!(
            "[core]\nstrict = true\n[gemini]\napi_key = \"{SENTINEL}\"\n\
             [local_model]\nbase_url = \"http://127.0.0.1:8081\"\n\
             database_url = \"postgres://user:{SENTINEL}@localhost/db\"\n"
        ),
    )
    .unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _env = TempEnv::set(
        "LEDGERFUL_DEFAULT_CONFIG",
        template.as_std_path().to_str().unwrap(),
    );

    execute_init(true).expect("init should sanitize the starter template");

    let config = fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(!config.contains(SENTINEL));
    assert!(!config.contains("api_key"));
    assert!(!config.contains("database_url"));
    assert!(config.contains("strict = true"));
}

#[test]
#[serial(env, cwd)]
fn init_malformed_template_fails_closed_without_publishing_config() {
    const SENTINEL: &str = "TA33-MALFORMED-SENTINEL";
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let template = root.join("default-config.toml");
    fs::write(&template, format!("[gemini]\napi_key = \"{SENTINEL}")).unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _env = TempEnv::set(
        "LEDGERFUL_DEFAULT_CONFIG",
        template.as_std_path().to_str().unwrap(),
    );

    let error = execute_init(false)
        .expect_err("malformed starter must fail closed")
        .to_string();

    assert!(!error.contains(SENTINEL));
    assert!(!root.join(".ledgerful").join("config.toml").exists());
}

#[test]
#[serial(env, cwd)]
fn init_home_template_is_sanitized_when_no_explicit_template_is_set() {
    const SENTINEL: &str = "TA33-HOME-SENTINEL";
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let user_config = root.join(".ledgerful");
    fs::create_dir_all(&user_config).unwrap();
    fs::write(
        user_config.join("default-config.toml"),
        format!("[core]\nstrict = true\n[gemini]\napi_key = \"{SENTINEL}\"\n"),
    )
    .unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _explicit = TempEnv::remove("LEDGERFUL_DEFAULT_CONFIG");
    let _profile = TempEnv::set("USERPROFILE", root.as_str());
    let _home = TempEnv::set("HOME", root.as_str());

    execute_init(false).expect("home template should sanitize");

    let config = fs::read_to_string(root.join(".ledgerful").join("config.toml")).unwrap();
    assert!(!config.contains(SENTINEL));
    assert!(config.contains("strict = true"));
}

#[test]
#[serial(env, cwd)]
fn init_existing_config_wins_over_malformed_template() {
    const EXISTING: &str = "[core]\nstrict = true\n";
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();
    let config_dir = root.join(".ledgerful");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), EXISTING).unwrap();
    let template = root.join("malformed.toml");
    fs::write(&template, "[gemini]\napi_key = \"unterminated").unwrap();
    setup_git_repo(tmp.path());
    let _guard = DirGuard::from_utf8(root);
    let _env = TempEnv::set(
        "LEDGERFUL_DEFAULT_CONFIG",
        template.as_std_path().to_str().unwrap(),
    );

    execute_init(false).expect("existing config must bypass malformed starter");

    assert_eq!(
        fs::read_to_string(config_dir.join("config.toml")).unwrap(),
        EXISTING
    );
}

#[test]
#[serial(env, cwd)]
fn test_init_git_hooks_are_idempotent() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    execute_init(false).unwrap();
    execute_init(false).unwrap();

    let pre_commit = fs::read_to_string(root.join(".git").join("hooks").join("pre-commit"))
        .expect("pre-commit hook should be installed");
    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");

    assert_eq!(pre_commit.matches("# ledgerful-ledger-gate").count(), 1);
    assert_eq!(pre_push.matches("# ledgerful-ledger-gate").count(), 1);
}

#[test]
#[serial(env, cwd)]
fn test_init_appends_git_hooks_without_replacing_existing_content() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let hooks_dir = root.join(".git").join("hooks");
    fs::write(
        hooks_dir.join("pre-commit"),
        "#!/usr/bin/env bash\necho existing pre-commit\n",
    )
    .unwrap();
    fs::write(
        hooks_dir.join("pre-push"),
        "#!/usr/bin/env bash\necho existing pre-push\n",
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);

    execute_init(false).unwrap();

    let pre_commit = fs::read_to_string(root.join(".git").join("hooks").join("pre-commit"))
        .expect("pre-commit hook should be installed");
    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");

    assert!(pre_commit.contains("echo existing pre-commit"));
    assert!(pre_commit.contains("# ledgerful-ledger-gate"));
    assert!(pre_push.contains("echo existing pre-push"));
    assert!(pre_push.contains("# ledgerful-ledger-gate"));
}

#[test]
#[serial(env, cwd)]
fn test_init_upgrades_old_ledgerful_owned_verify_gate() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let hooks_dir = root.join(".git").join("hooks");
    let old_block = "\n# ledgerful-verify-gate: fast scoped verification (pre-push only)\n\
if command -v ledgerful &>/dev/null; then\n\
    if ! ledgerful verify --scope fast 2>/dev/null; then\n\
        echo \"\"\n\
        echo \"  Pre-push quality gate FAILED (ledgerful verify --scope fast).\"\n\
        echo \"  Fix the above errors before pushing.\"\n\
        echo \"\"\n\
        echo \"  Bypass (not recommended): git push --no-verify\"\n\
        exit 1\n\
    fi\n\
fi\n";
    fs::write(
        hooks_dir.join("pre-push"),
        format!(
            "#!/usr/bin/env bash\necho existing user pre-push\n{}",
            old_block
        ),
    )
    .unwrap();

    let _guard = DirGuard::from_utf8(root);

    execute_init(false).unwrap();

    let pre_push = fs::read_to_string(root.join(".git").join("hooks").join("pre-push"))
        .expect("pre-push hook should be installed");

    assert!(
        pre_push.contains("echo existing user pre-push"),
        "Should retain user content"
    );
    assert!(
        !pre_push.contains("Pre-push quality gate FAILED"),
        "Old block should be removed"
    );
    assert!(
        pre_push.contains("[Ledgerful] Push blocked by verification failure."),
        "New block should be inserted"
    );
}

#[test]
#[serial(env, cwd)]
fn test_init_fresh_repo_does_not_emit_unsafe_aliases() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    let result = execute_init(false);
    assert!(result.is_ok());

    let rules_file = root.join(".ledgerful").join("rules.toml");
    let rules_content = fs::read_to_string(rules_file).unwrap();

    // Must not emit abstract executable aliases or Cargo commands
    assert!(
        !rules_content.contains("\"build\""),
        "Emitted unsafe build alias"
    );
    assert!(
        !rules_content.contains("\"lint\""),
        "Emitted unsafe lint alias"
    );
    assert!(
        !rules_content.contains("\"test\""),
        "Emitted unsafe test alias"
    );
    assert!(
        !rules_content.contains("cargo test"),
        "Emitted Cargo commands in a fresh repo"
    );
    assert!(
        !rules_content.contains("cargo clippy"),
        "Emitted Cargo commands in a fresh repo"
    );
}

#[test]
#[serial(env, cwd)]
fn test_init_empty_repo_and_add_scaffold_after_init() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    let _guard = DirGuard::from_utf8(root);

    // 1. Init on empty repo
    execute_init(false).unwrap();
    let layout = ledgerful::state::layout::Layout::new(root);
    let config = ledgerful::config::load::load_config(&layout).unwrap_or_default();
    assert_eq!(
        config.verify.effective_mode(),
        ledgerful::config::model::VerifyMode::Auto
    );

    let profile1 = ledgerful::platform::repository::detect_repository(root.as_std_path());
    assert!(
        profile1.evidence.is_empty(),
        "Empty repo should have no evidence"
    );
    let auto_steps1 = ledgerful::verify::auto_policy::build_auto_policy(
        &profile1,
        &config.verify,
        root.as_std_path(),
        ledgerful::verify::plan::VerifyScope::Full,
    );
    assert_eq!(
        auto_steps1.len(),
        2,
        "Empty repo should yield git diff neutral steps"
    );

    // 2. Add scaffold (e.g. package.json)
    fs::write(
        root.join("package.json"),
        "{\"scripts\": {\"test:ci\": \"jest\"}}",
    )
    .unwrap();

    // 3. Re-run init (should be idempotent, not overwrite rules)
    execute_init(false).unwrap();

    // 4. Verification plan should now dynamically pick up the new scaffold
    let profile2 = ledgerful::platform::repository::detect_repository(root.as_std_path());
    assert!(!profile2.evidence.is_empty(), "Should detect package.json");
    let auto_steps2 = ledgerful::verify::auto_policy::build_auto_policy(
        &profile2,
        &config.verify,
        root.as_std_path(),
        ledgerful::verify::plan::VerifyScope::Full,
    );
    assert!(
        !auto_steps2.is_empty(),
        "Should dynamically add verification steps based on scaffold"
    );
    assert!(
        auto_steps2
            .iter()
            .any(|s| s.command.contains("npm run test:ci"))
    );
}

#[test]
#[serial(env, cwd)]
fn init_skips_hooks_when_lefthook_present() {
    let _env_non_interactive = non_interactive();
    let tmp = tempdir().unwrap();
    let root = Utf8Path::from_path(tmp.path()).unwrap();

    setup_git_repo(tmp.path());

    // Create lefthook.yml to signal third-party hook manager
    fs::write(root.join("lefthook.yml"), "pre-push:\n  commands:\n").unwrap();

    let _guard = DirGuard::from_utf8(root);

    let result = execute_init(false);
    assert!(result.is_ok());

    // No git hooks should be installed when a third-party manager (lefthook) is present
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    assert!(
        !hook_path.exists(),
        "pre-push hook should not be installed when lefthook.yml is present"
    );
    assert!(
        !root.join(".git").join("hooks").join("pre-commit").exists(),
        "pre-commit hook should not be installed when lefthook.yml is present"
    );
}
