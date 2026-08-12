use crate::federated::links::{omitted_honesty_message, path_basename, present_federated_links};
use crate::federated::scanner::FederatedScanner;
use crate::federated::schema::{FederatedSchema, PublicInterface};
use crate::federated::storage::{
    clear_federated_dependencies, get_federated_links, prune_dead_and_self_links,
    save_federated_dependencies, upsert_federated_link_by_path,
};
use crate::git::repo::open_repo;
use crate::index::storage::get_public_symbols;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::env;
use std::fs;

pub fn execute_federate_export(dry_run: bool, out: Option<String>) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let repo = open_repo(&current_dir).into_diagnostic()?;
    let _repo_root = repo
        .workdir()
        .ok_or_else(|| miette::miette!("Could not determine repository root"))?
        .to_path_buf();

    let layout = crate::commands::helpers::get_layout()?;
    let storage = StorageManager::init_with_layout(&layout)?;

    let repo_name = layout
        .root
        .file_name()
        .map(|s| s.to_string())
        .ok_or_else(|| miette::miette!("Could not determine repository name for export"))?;

    if !dry_run && out.is_none() {
        println!(
            "Exporting public interfaces for {}...",
            repo_name.if_supports_color(Stream::Stdout, |s| s.cyan())
        );
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
            "SUCCESS".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
            out_path
                .display()
                .to_string()
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
    } else if dry_run {
        println!(
            "\n{}",
            "--- FEDERATED SCHEMA PREVIEW ---"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow()))
        );
        println!("{}", schema_json);
        println!(
            "{}",
            "--- END PREVIEW ---"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow()))
        );
    } else {
        let schema_path = layout.state_subdir().join("schema.json");
        fs::write(&schema_path, schema_json).into_diagnostic()?;

        println!(
            "{} Schema exported to {}",
            "SUCCESS".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
            schema_path.if_supports_color(Stream::Stdout, |s| s.cyan())
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
    let layout = crate::commands::helpers::get_layout()?;
    let mut storage = StorageManager::init_with_layout(&layout)?;

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
            "WARNING:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            format!(
                "local impact cache {reason} — dependency discovery below may not reflect the current working tree."
            ).if_supports_color(Stream::Stdout, |s| s.yellow())

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
        println!(
            "{} {}",
            "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            warning
        );
    }

    if siblings.is_empty() {
        println!("No siblings with Ledgerful schemas found.");
    }

    let timestamp = Utc::now().to_rfc3339();
    // 0034: collect cross-sibling scan-degradation warnings and dedup before
    // printing. The local-repo walk re-runs per sibling with identical
    // root/budget, so a budget/deadline breach produces byte-identical text
    // on every iteration — without dedup, an 8-sibling scan would print the
    // same WARN line 8 times.
    let mut cross_sibling_warnings: Vec<String> = Vec::new();
    for (path, schema, sibling_warnings) in &siblings {
        // 0184: store name = folder basename (path identity), not schema.repo_name.
        let store_name = path_basename(path.as_str());
        println!(
            "  Processing {}: {}",
            store_name.if_supports_color(Stream::Stdout, |s| s.cyan()),
            path.if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
        // TA31 R1: a sibling can now be discovered with data-quality
        // warnings (e.g. an empty ledger entity) instead of being
        // hard-skipped. Surface those warnings the same way scan-level
        // warnings are printed above, so the user sees what needs
        // attention.
        for warning in sibling_warnings {
            println!(
                "{} {}: {}",
                "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                store_name,
                warning
            );
        }
        let store_name =
            upsert_federated_link_by_path(storage.get_connection(), path.as_str(), &timestamp)?;

        // Task 2.2: Discover and save dependencies under the basename key
        clear_federated_dependencies(storage.get_connection(), &store_name)?;
        let (dependencies, scan_warnings) =
            scanner.discover_dependencies(&local_packet, &store_name, schema)?;

        for (local_symbol, sibling_symbol) in dependencies {
            save_federated_dependencies(
                storage.get_connection(),
                &store_name,
                &local_symbol,
                &sibling_symbol,
            )?;
        }
        // 0034: collect scan degradation warnings for cross-sibling dedup.
        cross_sibling_warnings.extend(scan_warnings);

        // Import federated ledger entries if present (trace_id = basename)
        if let Some(entries) = &schema.ledger {
            crate::ledger::federation::import_federated_entries(
                storage.get_connection_mut(),
                &repo_root,
                &store_name,
                entries,
            )
            .into_diagnostic()?;
        }
    }

    // 0184: prune Dead/Self only (not "absent from this scan").
    // Always run — including when discovery found zero siblings — so a
    // husk/self-only cache is cleaned when status honesty points here.
    let pruned = prune_dead_and_self_links(storage.get_connection(), layout.root.as_str())?;
    if pruned > 0 {
        println!(
            "{} Pruned {} dead or self-referential federated link(s).",
            "INFO".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold())),
            pruned
        );
    }

    // 0034: dedup cross-sibling degradation warnings (the walk re-runs per
    // sibling with identical root/budget, so breaches produce identical text).
    cross_sibling_warnings.sort();
    cross_sibling_warnings.dedup();
    for warning in cross_sibling_warnings {
        println!(
            "{} {}",
            "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            warning
        );
    }

    if !siblings.is_empty() {
        println!(
            "{} Processed {} sibling(s).",
            "SUCCESS".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
            siblings.len()
        );
    }
    Ok(())
}

pub fn execute_federate_status() -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let _repo = open_repo(&current_dir).into_diagnostic()?;

    let layout = crate::commands::helpers::get_layout()?;
    let storage = StorageManager::init_with_layout(&layout)?;

    let raw = get_federated_links(storage.get_connection())?;
    // 0184: path identity — present Live peers only (RO; no DELETE).
    let presented = present_federated_links(&raw, layout.root.as_str());

    if raw.is_empty() {
        println!("No federated links found. Run 'ledgerful federate scan' to discover siblings.");
        return Ok(());
    }

    if presented.omitted_total() > 0 {
        println!(
            "{} {}",
            "WARN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            omitted_honesty_message(presented.omitted_total())
        );
    }

    if presented.live.is_empty() {
        println!(
            "No live federated peers. Run 'ledgerful federate scan' to discover siblings and prune the cache."
        );
        return Ok(());
    }

    println!(
        "{} known federated repositories:",
        presented
            .live
            .len()
            .if_supports_color(Stream::Stdout, |s| s.bold())
    );
    for link in presented.live {
        println!(
            "- {} (at {})",
            link.name.if_supports_color(Stream::Stdout, |s| s.cyan()),
            link.path.if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
        println!(
            "  Last scanned: {}",
            link.last_scanned
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
        );
    }

    Ok(())
}
