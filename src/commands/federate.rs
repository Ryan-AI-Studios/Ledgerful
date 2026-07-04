use crate::federated::scanner::FederatedScanner;
use crate::federated::schema::{FederatedSchema, PublicInterface};
use crate::federated::storage::{
    clear_federated_dependencies, get_federated_links, save_federated_dependencies,
    update_federated_link,
};
use crate::git::repo::open_repo;
use crate::index::storage::get_public_symbols;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::env;
use std::fs;

pub fn execute_federate_export(dry_run: bool, out: Option<String>) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = open_repo(&current_dir).into_diagnostic()?;
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Could not determine repository root"))?
        .to_path_buf();

    let layout = Layout::new(repo_root.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path())?;

    let repo_name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| miette::miette!("Could not determine repository name for export"))?
        .to_string();

    if !dry_run && out.is_none() {
        println!("Exporting public interfaces for {}...", repo_name.cyan());
    }

    let symbols = get_public_symbols(storage.get_connection())?;
    let mut public_interfaces = symbols
        .into_iter()
        .map(|s| PublicInterface {
            symbol: s.name,
            file: s.file_path,
            kind: s.kind,
        })
        .collect::<Vec<_>>();

    public_interfaces.retain(|interface| {
        crate::impact::redact::sanitize_prompt(
            &interface.symbol,
            crate::impact::redact::DEFAULT_MAX_BYTES,
        )
        .redactions
        .is_empty()
    });

    let ledger_entries =
        crate::ledger::federation::export_ledger_entries(storage.get_connection(), 30)
            .into_diagnostic()?;

    let mut schema = FederatedSchema::new(repo_name, public_interfaces).with_ledger(ledger_entries);
    schema.generated_at = Utc::now().to_rfc3339();
    schema.binary_version = env!("CARGO_PKG_VERSION").to_string();
    let schema_json = serde_json::to_string_pretty(&schema).into_diagnostic()?;

    if let Some(out_path) = out {
        let out_path = std::path::Path::new(&out_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).into_diagnostic()?;
        }
        fs::write(out_path, schema_json).into_diagnostic()?;
        println!(
            "{} Schema exported to {}",
            "SUCCESS".green().bold(),
            out_path.display().to_string().cyan()
        );
    } else if dry_run {
        println!("\n{}", "--- FEDERATED SCHEMA PREVIEW ---".bold().yellow());
        println!("{}", schema_json);
        println!("{}", "--- END PREVIEW ---".bold().yellow());
    } else {
        let schema_path = layout.state_subdir().join("schema.json");
        fs::write(&schema_path, schema_json).into_diagnostic()?;

        println!(
            "{} Schema exported to {}",
            "SUCCESS".green().bold(),
            schema_path.cyan()
        );
    }
    Ok(())
}

pub fn execute_federate_scan() -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = open_repo(&current_dir).into_diagnostic()?;
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Could not determine repository root"))?
        .to_path_buf();

    let utf8_repo_root = Utf8PathBuf::from_path_buf(repo_root.clone())
        .map_err(|_| miette::miette!("Invalid UTF-8 path"))?;
    let layout = Layout::new(repo_root.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;

    let local_packet = storage
        .get_latest_packet()?
        .ok_or_else(|| miette::miette!(
            "No local index found. Run 'ledgerful index --incremental' or 'ledgerful scan --impact' first, then run 'ledgerful federate export' to make this repo discoverable."
        ))?;

    // CG-F35 (requirement #1, #6): `local_packet` drives dependency discovery
    // against every sibling repo found below, and the result is the
    // cross-repo trust surface other repos will read via `federate status`.
    // A stale/corrupt local cache is a bigger problem here than in a purely
    // local query, so warn clearly (not just to stderr — this command
    // already prints user-facing progress with `println!`) before scanning.
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    if let Some(reason) = crate::state::reports::warn_if_impact_stale(&layout, &config) {
        println!(
            "{} {}",
            "WARNING:".yellow().bold(),
            format!(
                "local impact cache {reason} — dependency discovery below may not reflect the current working tree."
            )
            .yellow()
        );
    }

    println!("Scanning for sibling repositories...");

    // TA31 R2: `federate scan` is the one call site that opts into
    // auto-syncing stale/missing sibling schema.json files, gated by the
    // `[federation] auto_sync_siblings` config flag (default `false`).
    // Other `scan_siblings()` callers (the `GET /api/projects` HTTP
    // handler in `src/commands/web/server/handlers.rs`, and
    // `src/federated/refresh.rs`) now load federation config for the
    // scan-reliability controls (exclusions/budget/timeouts) via
    // `with_federation_config`, but still deliberately do NOT pass
    // `auto_sync` — auto-sync spawns blocking subprocesses per sibling,
    // and running that synchronously inside an HTTP request handler
    // would be a latency/DoS hazard.
    let scanner = FederatedScanner::new(utf8_repo_root)
        .with_auto_sync(config.federation.auto_sync_siblings)
        .with_federation_config(&config.federation);
    let (siblings, warnings) = scanner.scan_siblings()?;

    for warning in &warnings {
        println!("{} {}", "WARN".yellow().bold(), warning);
    }

    if siblings.is_empty() {
        println!("No siblings with Ledgerful schemas found.");
        return Ok(());
    }

    let timestamp = Utc::now().to_rfc3339();
    // 0034: collect cross-sibling scan-degradation warnings and dedup before
    // printing. The local-repo walk re-runs per sibling with identical
    // root/budget, so a budget/deadline breach produces byte-identical text
    // on every iteration — without dedup, an 8-sibling scan would print the
    // same WARN line 8 times.
    let mut cross_sibling_warnings: Vec<String> = Vec::new();
    for (path, schema, sibling_warnings) in &siblings {
        println!(
            "  Processing {}: {}",
            schema.repo_name.cyan(),
            path.dimmed()
        );
        // TA31 R1: a sibling can now be discovered with data-quality
        // warnings (e.g. an empty ledger entity) instead of being
        // hard-skipped. Surface those warnings the same way scan-level
        // warnings are printed above, so the user sees what needs
        // attention.
        for warning in sibling_warnings {
            println!(
                "{} {}: {}",
                "WARN".yellow().bold(),
                schema.repo_name,
                warning
            );
        }
        update_federated_link(
            storage.get_connection(),
            &schema.repo_name,
            path.as_str(),
            &timestamp,
        )?;

        // Task 2.2: Discover and save dependencies
        clear_federated_dependencies(storage.get_connection(), &schema.repo_name)?;
        let (dependencies, scan_warnings) =
            scanner.discover_dependencies(&local_packet, &schema.repo_name, schema)?;

        for (local_symbol, sibling_symbol) in dependencies {
            save_federated_dependencies(
                storage.get_connection(),
                &schema.repo_name,
                &local_symbol,
                &sibling_symbol,
            )?;
        }
        // 0034: collect scan degradation warnings for cross-sibling dedup.
        cross_sibling_warnings.extend(scan_warnings);

        // Import federated ledger entries if present
        if let Some(entries) = &schema.ledger {
            crate::ledger::federation::import_federated_entries(
                storage.get_connection_mut(),
                &repo_root,
                &schema.repo_name,
                entries,
            )
            .into_diagnostic()?;
        }
    }

    // 0034: dedup cross-sibling degradation warnings (the walk re-runs per
    // sibling with identical root/budget, so breaches produce identical text).
    cross_sibling_warnings.sort();
    cross_sibling_warnings.dedup();
    for warning in cross_sibling_warnings {
        println!("{} {}", "WARN".yellow().bold(), warning);
    }

    println!(
        "{} Processed {} sibling(s).",
        "SUCCESS".green().bold(),
        siblings.len()
    );
    Ok(())
}

pub fn execute_federate_status() -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = open_repo(&current_dir).into_diagnostic()?;
    let repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Could not determine repository root"))?;

    let layout = Layout::new(repo_root.to_string_lossy().as_ref());
    let db_path = layout.state_subdir().join("ledger.db");
    let storage = StorageManager::init(db_path.as_std_path())?;

    let links = get_federated_links(storage.get_connection())?;

    if links.is_empty() {
        println!("No federated links found. Run 'ledgerful federate scan' to discover siblings.");
        return Ok(());
    }

    println!("{} known federated repositories:", links.len().bold());
    for (name, path, last_scan) in links {
        println!("- {} (at {})", name.cyan(), path.dimmed());
        println!("  Last scanned: {}", last_scan.dimmed());
    }

    Ok(())
}
