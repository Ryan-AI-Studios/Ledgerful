use crate::commands::hook_repair::{HooksDirResolution, resolve_hooks_dir};
use crate::commands::hook_template::{
    GateKind, INTENT_GATE_MARKER, LEDGER_GATE_MARKER, POST_COMMIT_GATE_MARKER,
    ensure_gate_on_hook_file, intent_gate_block, ledger_gate_block, post_commit_gate_block,
};
use crate::config::ConfigError;
use crate::config::starter::{publish_starter_config, starter_config_contents};
use crate::git::ignore::add_to_gitignore;
use crate::policy::defaults::DEFAULT_RULES;
use crate::state::layout::Layout;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use std::fs;
use std::io::Write as IoWrite;
use tracing::info;

/// Shared hooks directory for install (linked worktrees via commondir — not `root/.git/hooks`).
fn hooks_dir_for_install(root: &Utf8Path) -> Option<Utf8PathBuf> {
    match resolve_hooks_dir(root) {
        HooksDirResolution::Found { hooks_dir } => Some(hooks_dir),
        HooksDirResolution::OutsideRepo { .. } | HooksDirResolution::CannotLook { .. } => None,
    }
}

fn set_executable_unix(hook_path: &Utf8Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(hook_path.as_std_path())
            .into_diagnostic()?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(hook_path.as_std_path(), perms).into_diagnostic()?;
    }
    let _ = hook_path;
    Ok(())
}

/// Install or stamp-aware-upgrade the ledger-gate block on pre-commit / pre-push.
/// Uses shared ensure (0121) — no independent body-diff silent rewrite.
fn install_git_hook(root: &Utf8PathBuf, hook_name: &str, bypass_command: &str) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join(hook_name);
    let hook_block = ledger_gate_block(bypass_command);

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(LEDGER_GATE_MARKER) {
            // Stamp-aware ensure: refresh stale product bodies only.
            let _ = ensure_gate_on_hook_file(&hook_path, GateKind::Ledger, bypass_command, false)?;
            return Ok(false);
        }
        // Append to existing hook
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{hook_block}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{hook_block}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
    }

    Ok(true)
}

fn install_commit_msg_hook(root: &Utf8PathBuf) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("commit-msg");
    let template = intent_gate_block();

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(INTENT_GATE_MARKER) {
            let _ = ensure_gate_on_hook_file(
                &hook_path,
                GateKind::Intent,
                "git commit --no-verify",
                false,
            )?;
            return Ok(false);
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{template}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{template}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
    }

    Ok(true)
}

fn install_post_commit_hook(root: &Utf8PathBuf) -> Result<bool> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(false);
    };
    fs::create_dir_all(&hooks_dir).into_diagnostic()?;

    let hook_path = hooks_dir.join("post-commit");
    let template = post_commit_gate_block();

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        if existing.contains(POST_COMMIT_GATE_MARKER) {
            let _ = ensure_gate_on_hook_file(
                &hook_path,
                GateKind::PostCommit,
                "git commit --no-verify",
                false,
            )?;
            return Ok(false);
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = format!("\n{template}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
    } else {
        let content = format!("#!/usr/bin/env bash\n\n{template}");
        fs::write(hook_path.as_std_path(), content).into_diagnostic()?;
        set_executable_unix(&hook_path)?;
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

/// Append or stamp-aware-refresh the fast scoped verify block on pre-push.
/// Shared ensure (0121) supersedes the prior silent body-diff rewrite.
fn install_pre_push_verify_block(root: &Utf8PathBuf) -> Result<()> {
    let Some(hooks_dir) = hooks_dir_for_install(root) else {
        return Ok(());
    };
    let hook_path = hooks_dir.join("pre-push");
    if !hook_path.exists() {
        return Ok(());
    }
    let _ = ensure_gate_on_hook_file(
        &hook_path,
        GateKind::Verify,
        "git push --no-verify",
        true, // append if marker absent
    )?;
    Ok(())
}

pub fn execute_init(no_gitignore: bool, enforce: bool) -> Result<()> {
    // 1. Discover work root + shared state dir (linked worktrees share main).
    // Non-git cwd keeps Layout::new (private state under cwd) intentionally.
    let (root, layout) = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            info!("Discovered git repository root at: {:?}", path);
            let work_root = Utf8PathBuf::from_path_buf(path)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?;
            let state_dir = crate::commands::helpers::resolve_state_dir(&repo)?;
            let layout = Layout::from_roots(&work_root, state_dir);
            (work_root, layout)
        }
        Err(e) => {
            info!(
                "gix::discover failed: {:?}. Using current directory as root",
                e
            );
            let root = Utf8PathBuf::from_path_buf(std::env::current_dir().into_diagnostic()?)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?;
            let layout = Layout::new(&root);
            (root, layout)
        }
    };

    info!(
        "Resolved init work_root={} state_dir={}",
        layout.root, layout.state_dir
    );

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
        // Enforce init defaults require_signing=true (0072 M2).
        if enforce {
            if contents.contains("require_signing") {
                contents = contents.replace("require_signing = false", "require_signing = true");
            } else if contents.contains("[intent]") {
                contents = contents.replacen("[intent]", "[intent]\nrequire_signing = true", 1);
            } else {
                contents.push_str("\n[intent]\nrequire_signing = true\n");
            }
        }
        // Auto-pin freshly generated public key into trusted_public_keys (0072 pin C).
        if let Ok(keys_dir) = crate::ledger::crypto::get_keys_dir() {
            let _ = crate::ledger::crypto::get_or_create_keys_in(&keys_dir);
            if let Ok(Some(pub_hex)) = crate::ledger::crypto::read_public_key_hex(&keys_dir) {
                let pin_line = format!("trusted_public_keys = [\"{}\"]\n", pub_hex);
                if contents.contains("[intent]") {
                    if contents.contains("trusted_public_keys") {
                        // leave explicit template alone
                    } else {
                        contents =
                            contents.replacen("[intent]", &format!("[intent]\n{}", pin_line), 1);
                    }
                } else {
                    contents.push_str(&format!("\n[intent]\n{}", pin_line));
                }
            }
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
    crate::state::storage::StorageManager::init_with_layout(&layout)?;

    // 7. Print the deterministic detected profile, evidence, and current commands.
    use owo_colors::{OwoColorize, Stream, Style};
    let profile = crate::platform::repository::detect_repository(root.as_std_path());
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let auto_steps = crate::verify::auto_policy::build_auto_policy(
        &profile,
        &config.verify,
        root.as_std_path(),
        crate::verify::plan::VerifyScope::Full,
    );

    println!(
        "\n{}",
        "Verification Auto-Policy Details"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
    );
    println!(
        "{}",
        "  Detected Stack:".if_supports_color(Stream::Stdout, |s| s.bold())
    );
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
        println!(
            "{}",
            "  Warnings:"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow()))
        );
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
    println!(
        "{}",
        "  Initial Commands:".if_supports_color(Stream::Stdout, |s| s.bold())
    );
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
        // 0154: product success line must survive default WARN floor (not tracing INFO).
        println!("Ledgerful initialized successfully!");
        return Ok(());
    }
    print_init_status_block(gate_mode);
    // 0154: product success line must survive default WARN floor (not tracing INFO).
    println!("Ledgerful initialized successfully!");
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

    let mut storage = StorageManager::init_with_layout(layout)?;
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
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "\n{}",
        "Ledgerful Status"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "  Gate mode: {}",
        gate_mode.if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
    );

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
    println!(
        "  Pending tx: {}",
        "0".if_supports_color(Stream::Stdout, |s| s.green())
    );
    println!(
        "  Drift:      {}",
        "0".if_supports_color(Stream::Stdout, |s| s.green())
    );
    println!("  Timings:    recorded locally (opt out: ledgerful timings --opt-out)");

    println!(
        "\n{}",
        "Next Steps"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
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
            "Notice:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow()))
        );
    }
}
