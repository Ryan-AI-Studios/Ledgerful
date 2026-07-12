use crate::config::ConfigError;
use crate::config::starter::{publish_starter_config, starter_config_contents};
use crate::git::ignore::add_to_gitignore;
use crate::policy::defaults::DEFAULT_RULES;
use crate::state::layout::Layout;
use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::io::Write as IoWrite;
use tracing::info;

const HOOK_MARKER: &str = "# ledgerful-ledger-gate";
const HOOK_BLOCK_TEMPLATE: &str = "\
# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo \"[Ledgerful] Blocked by ledger state.\"
        echo \"[Ledgerful] Resolve with:\"
        echo \"[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'\"
        echo \"[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
";

/// Additional block appended to the pre-push hook only. Runs a fast scoped
/// verification gate (`ledgerful verify --scope fast`) that uses
/// `test_mapping` to run only the tests covering changed files, falling back
/// to the full suite when shared infrastructure is touched. This keeps the
/// pre-push gate fast (~30-60s for typical scoped changes) while CI runs the
/// full suite. See `docs/Engineering.md` ("Test Tiers" section) for the full layered strategy.
const PRE_PUSH_VERIFY_BLOCK: &str = "\
# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo \"[Ledgerful] Push blocked by verification failure.\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
";

fn install_git_hook(root: &Utf8PathBuf, hook_name: &str, bypass_command: &str) -> Result<bool> {
    let git_dir = root.join(".git");
    if !git_dir.exists() {
        return Ok(false);
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join(hook_name);
    let hook_block = HOOK_BLOCK_TEMPLATE.replace("{bypass_command}", bypass_command);

    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path).into_diagnostic()?;
        if existing.contains(HOOK_MARKER) {
            if !existing.contains(&hook_block) {
                let re = regex::Regex::new(r"(?s)\n?# ledgerful-ledger-gate:.*?\nfi\nfi\n?")
                    .into_diagnostic()?;
                if re.is_match(&existing) {
                    let upgraded = re.replace(&existing, format!("\n{}\n", hook_block).as_str());
                    fs::write(&hook_path, upgraded.as_bytes()).into_diagnostic()?;
                }
            }
            return Ok(false);
        }
        // Append to existing hook
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&hook_path)
            .into_diagnostic()?;
        let block = format!("\n{}\n", hook_block);
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        // Create new hook with shebang
        let content = format!("#!/usr/bin/env bash\n\n{}\n", hook_block);
        fs::write(&hook_path, content).into_diagnostic()?;
        // Set executable bit on Unix; no-op on Windows
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path).into_diagnostic()?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).into_diagnostic()?;
        }
    }

    Ok(true)
}

const COMMIT_MSG_MARKER: &str = "# ledgerful-intent-gate";
const COMMIT_MSG_HOOK_TEMPLATE: &str = "\
# ledgerful-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg \"$1\"
fi
";

const POST_COMMIT_MARKER: &str = "# ledgerful-post-commit-gate";
const POST_COMMIT_HOOK_TEMPLATE: &str = "\
# ledgerful-post-commit-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit \"$@\"
fi
";

fn install_commit_msg_hook(root: &Utf8PathBuf) -> Result<bool> {
    let git_dir = root.join(".git");
    if !git_dir.exists() {
        return Ok(false);
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("commit-msg");

    // Idempotent: skip if our block is already present
    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path).into_diagnostic()?;
        if existing.contains(COMMIT_MSG_MARKER) {
            return Ok(false);
        }
        // Append to existing hook
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&hook_path)
            .into_diagnostic()?;
        let block = format!("\n{}\n", COMMIT_MSG_HOOK_TEMPLATE);
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        // Create new hook with shebang
        let content = format!("#!/usr/bin/env bash\n\n{}\n", COMMIT_MSG_HOOK_TEMPLATE);
        fs::write(&hook_path, content).into_diagnostic()?;
        // Set executable bit on Unix; no-op on Windows
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path).into_diagnostic()?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).into_diagnostic()?;
        }
    }

    Ok(true)
}

fn install_post_commit_hook(root: &Utf8PathBuf) -> Result<bool> {
    let git_dir = root.join(".git");
    if !git_dir.exists() {
        return Ok(false);
    }

    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("post-commit");

    // Idempotent: skip if our block is already present
    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path).into_diagnostic()?;
        if existing.contains(POST_COMMIT_MARKER) {
            return Ok(false);
        }
        // Append to existing hook
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&hook_path)
            .into_diagnostic()?;
        let block = format!("\n{}\n", POST_COMMIT_HOOK_TEMPLATE);
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        // Create new hook with shebang
        let content = format!("#!/usr/bin/env bash\n\n{}\n", POST_COMMIT_HOOK_TEMPLATE);
        fs::write(&hook_path, content).into_diagnostic()?;
        // Set executable bit on Unix; no-op on Windows
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path).into_diagnostic()?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms).into_diagnostic()?;
        }
    }

    Ok(true)
}

fn install_ledger_gate_hooks(root: &Utf8PathBuf) -> Result<Vec<&'static str>> {
    // Skip if a third-party hook manager owns this repo's hooks
    if let Some(manager) =
        crate::commands::hook_repair::detect_third_party_hook_manager(root.as_path())
    {
        eprintln!(
            "INFO: Third-party hook manager '{}' detected. Skipping Ledgerful hook installation. Configure {} to call `ledgerful`.",
            manager.name(),
            manager.name()
        );
        return Ok(vec![]);
    }

    let mut installed = Vec::new();

    if install_git_hook(root, "pre-commit", "git commit --no-verify")? {
        installed.push("pre-commit");
    }

    if install_git_hook(root, "pre-push", "git push --no-verify")? {
        installed.push("pre-push");
    }
    // Append the fast scoped verify gate to the pre-push hook. Idempotent:
    // skips if the verify marker is already present.
    install_pre_push_verify_block(root)?;

    if install_commit_msg_hook(root)? {
        installed.push("commit-msg");
    }

    if install_post_commit_hook(root)? {
        installed.push("post-commit");
    }

    Ok(installed)
}

const VERIFY_GATE_MARKER: &str = "# ledgerful-verify-gate";

/// Append the fast scoped verify block to the pre-push hook if it's not
/// already present. This is separate from `install_git_hook` because the
/// verify block is pre-push-only and has its own marker for idempotency.
fn install_pre_push_verify_block(root: &Utf8PathBuf) -> Result<()> {
    let hook_path = root.join(".git").join("hooks").join("pre-push");
    if !hook_path.exists() {
        return Ok(());
    }
    let block = PRE_PUSH_VERIFY_BLOCK.replace("{bypass_command}", "git push --no-verify");
    let existing = fs::read_to_string(&hook_path).into_diagnostic()?;
    if existing.contains(VERIFY_GATE_MARKER) {
        if !existing.contains(&block) {
            // Upgrade existing block using regex
            let re = regex::Regex::new(r"(?s)\n?# ledgerful-verify-gate:.*?\nfi\nfi\n?")
                .into_diagnostic()?;
            if re.is_match(&existing) {
                let upgraded = re.replace(&existing, format!("\n{}\n", block).as_str());
                fs::write(&hook_path, upgraded.as_bytes()).into_diagnostic()?;
                return Ok(());
            }
        }
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&hook_path)
        .into_diagnostic()?;
    let block = format!("\n{}\n", block);
    file.write_all(block.as_bytes()).into_diagnostic()?;
    Ok(())
}

pub fn execute_init(no_gitignore: bool, enforce: bool) -> Result<()> {
    // 1. Discover repository root
    let root = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            info!("Discovered git repository root at: {:?}", path);
            Utf8PathBuf::from_path_buf(path)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?
        }
        Err(e) => {
            info!(
                "gix::discover failed: {:?}. Using current directory as root",
                e
            );
            Utf8PathBuf::from_path_buf(std::env::current_dir().into_diagnostic()?)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?
        }
    };

    info!("Resolved root for initialization: {}", root);
    let layout = Layout::new(&root);

    // 2. Ensure directory layout
    layout.ensure_state_dir()?;

    // 3. Generate starter configurations
    let config_path = layout.config_file();
    let gate_mode = if enforce { "enforce" } else { "observe" };
    let config_created = !config_path.exists();
    if config_created {
        let starter = starter_config_contents()?;
        let mut contents = starter.contents;
        if contents.contains("[gate]") {
            contents = contents.replace("mode = \"observe\"", &format!("mode = \"{}\"", gate_mode));
        } else {
            contents.push_str("\n[gate]\nmode = \"");
            contents.push_str(gate_mode);
            contents.push_str("\"\n");
        }
        let created = publish_starter_config(config_path.as_std_path(), &contents)?;
        if created {
            if !starter.removed_secret_paths.is_empty() {
                eprintln!(
                    "Starter config omitted {} secret-bearing assignments:",
                    starter.removed_secret_paths.len()
                );
                for path in &starter.removed_secret_paths {
                    eprintln!("  {path}");
                }
                eprintln!("Use environment variables or the repo-local .env file for credentials.");
            }
            info!("Created starter config at {}", config_path);
        }
    }

    let rules_path = layout.rules_file();
    if !rules_path.exists() {
        fs::write(&rules_path, DEFAULT_RULES).map_err(|e| ConfigError::WriteFailed {
            path: rules_path.to_string(),
            source: e,
        })?;
        info!("Created starter rules at {}", rules_path);
    }

    // 4. Update .gitignore
    if !no_gitignore {
        let changed = add_to_gitignore(&root, ".ledgerful/")?;
        if changed {
            info!("Added .ledgerful/ to .gitignore");
        }
    }

    // 5. Install Git ledger gate hooks
    match install_ledger_gate_hooks(&root) {
        Ok(installed) if !installed.is_empty() => {
            println!(
                "Installed Ledgerful ledger gate hooks: {}.",
                installed.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("Warning: could not install Git ledger gate hooks: {e}"),
    }

    // 6. Initialize ledger storage database
    let db_path = layout.state_subdir().join("ledger.db");
    crate::state::storage::StorageManager::init(db_path.as_std_path())?;

    // 7. Print the deterministic detected profile, evidence, and current commands.
    use owo_colors::OwoColorize;
    let profile = crate::platform::repository::detect_repository(root.as_std_path());
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let auto_steps = crate::verify::auto_policy::build_auto_policy(
        &profile,
        &config.verify,
        root.as_std_path(),
        crate::verify::plan::VerifyScope::Full,
    );

    println!("\n{}", "Verification Auto-Policy Details".bold().cyan());
    println!("{}", "  Detected Stack:".bold());
    for ev in &profile.evidence {
        let text = match ev {
            crate::platform::repository::DetectionEvidence::FoundCargoToml => "Cargo (Cargo.toml)",
            crate::platform::repository::DetectionEvidence::FoundDenoJson => "Deno (deno.json)",
            crate::platform::repository::DetectionEvidence::FoundDenoJsonc => "Deno (deno.jsonc)",
            crate::platform::repository::DetectionEvidence::FoundPackageJson => {
                "Node (package.json)"
            }
            crate::platform::repository::DetectionEvidence::FoundLockfile(name) => name.as_str(),
        };
        println!("    • {}", text);
    }
    if profile.evidence.is_empty() {
        println!("    • None (Neutral)");
    }
    if !profile.warnings.is_empty() {
        println!("{}", "  Warnings:".bold().yellow());
        for warn in &profile.warnings {
            let warn_text = match warn {
                crate::platform::repository::DetectionWarning::AmbiguousDenoConfig => {
                    "Found both deno.json and deno.jsonc".to_string()
                }
                crate::platform::repository::DetectionWarning::AmbiguousLockfiles(msg) => {
                    msg.clone()
                }
                crate::platform::repository::DetectionWarning::ConflictingPackageManager(msg) => {
                    msg.clone()
                }
                crate::platform::repository::DetectionWarning::DenoWorkspaceWithoutRootTasks => {
                    "Deno workspace lacks root tasks".to_string()
                }
                crate::platform::repository::DetectionWarning::MalformedManifest(msg) => {
                    msg.clone()
                }
                crate::platform::repository::DetectionWarning::NodeWorkspaceWithoutRootScripts => {
                    "Node workspace lacks root scripts".to_string()
                }
                crate::platform::repository::DetectionWarning::UnreadableManifest(msg) => {
                    format!("Unreadable manifest: {}", msg)
                }
            };
            println!("    • {}", warn_text);
        }
    }
    println!("{}", "  Initial Commands:".bold());
    for step in &auto_steps {
        println!("    • {}", step.command);
    }
    if auto_steps.is_empty() {
        println!("    • None");
    }
    println!();

    if config_created {
        if let Err(e) = write_initial_mode_ledger_entry(&layout, gate_mode) {
            eprintln!("Warning: could not record initial gate mode ledger entry: {e}");
        }
    } else {
        let existing_config = crate::config::load::load_config(&layout).unwrap_or_default();
        let actual_mode = existing_config.gate.mode.clone();
        print_init_status_block(&actual_mode);
        info!("Ledgerful initialized successfully!");
        return Ok(());
    }
    print_init_status_block(gate_mode);
    info!("Ledgerful initialized successfully!");
    Ok(())
}

pub(crate) fn write_initial_mode_ledger_entry(
    layout: &crate::state::layout::Layout,
    gate_mode: &str,
) -> miette::Result<()> {
    use crate::ledger::{
        Category, ChangeType, CommitRequest, EntryType, TransactionManager, TransactionRequest,
    };
    use crate::state::storage::StorageManager;

    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let config = crate::commands::helpers::load_ledger_config(layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: "ledgerful/gate-mode".to_string(),
            planned_action: Some(format!("Initialize gate mode: {}", gate_mode)),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: format!("Gate mode initialized to {}", gate_mode),
                reason: "Initial mode set by ledgerful init".to_string(),
                entry_type: Some(EntryType::Maintenance),
                ..Default::default()
            },
            false,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    Ok(())
}

fn print_init_status_block(gate_mode: &str) {
    use owo_colors::OwoColorize;

    println!("\n{}", "Ledgerful Status".bold().underline());
    println!("  Gate mode: {}", gate_mode.yellow().bold());

    let has_local_model = std::env::var("OLLAMA_API_KEY").is_ok()
        || std::env::var("OLLAMA_CLOUD_API_KEY").is_ok()
        || std::env::var("GEMINI_API_KEY").is_ok();
    let model_line = if has_local_model {
        "cloud env detected"
    } else {
        "none (run 'ledgerful setup ai' or set GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY)"
    };
    println!("  Model:      {}", model_line);

    let keys_dir = crate::ledger::crypto::get_keys_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|_| "~/.ledgerful/keys".to_string());
    println!("  Keys:       {}", keys_dir);
    println!("  Hooks:      commit-msg, post-commit, pre-push (.git/hooks/)");
    println!("  Pending tx: {}", "0".green());
    println!("  Drift:      {}", "0".green());

    println!("\n{}", "Next Steps".bold().underline());
    println!(
        "  1. ledgerful index --incremental    # Index changed files (~5-10s for a medium repo)"
    );
    println!(
        "  2. ledgerful web start              # Launch the local dashboard at http://127.0.0.1:52001"
    );
    println!("  3. ledgerful verify --scope fast    # Run scoped verification on changed files");

    if gate_mode == "observe" {
        println!(
            "\n{} commits are recorded and warned, never blocked. Run 'ledgerful gate mode enforce' when ready.",
            "Notice:".bold().yellow()
        );
    }
}
