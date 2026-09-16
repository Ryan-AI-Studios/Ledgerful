use crate::federated::links::{
    classify_peer_freshness, format_freshness_line, omitted_honesty_message, path_basename,
    present_federated_links,
};
use crate::federated::scanner::FederatedScanner;
use crate::federated::schema::{FederatedSchema, PublicInterface};
use crate::federated::storage::{
    clear_federated_dependencies, get_federated_links, prune_dead_and_self_links,
    save_federated_dependencies, upsert_federated_link_by_path,
};
use crate::git::repo::{get_head_info, open_repo};
use crate::index::storage::get_public_symbols;
use crate::state::storage::StorageManager;
use camino::Utf8PathBuf;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use rusqlite::Connection;
use serde::Serialize;
use std::env;
use std::fs;
use std::io::Write;

const PREVIEW_KIND: &str = "federateExportPreview";
const PREVIEW_ZERO_NEXT: &str = "ledgerful scan --impact";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FederateExportPreviewInterface {
    pub symbol: String,
    pub file: String,
    pub kind: crate::index::symbols::SymbolKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FederateExportPreview {
    pub schema_version: u32,
    pub kind: String,
    pub dry_run: bool,
    pub repo_name: String,
    pub binary_version: String,
    pub wire_schema_version: String,
    pub limit: u64,
    pub truncated: bool,
    pub result_count: usize,
    pub total_matching: usize,
    pub interfaces: Vec<FederateExportPreviewInterface>,
    pub ledger_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

pub(crate) fn resolve_distinct_line(values: impl IntoIterator<Item = Option<i64>>) -> Option<i64> {
    let mut seen: Option<i64> = None;
    for value in values {
        let Some(line) = value else {
            continue;
        };
        match seen {
            None => seen = Some(line),
            Some(prev) if prev != line => return None,
            Some(_) => {}
        }
    }
    seen
}

pub(crate) fn lookup_interface_line(
    conn: &Connection,
    interface: &PublicInterface,
) -> Result<Option<i64>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT ps.line_start
             FROM project_symbols ps
             INNER JOIN project_files pf ON ps.file_id = pf.id
             WHERE pf.file_path = ? AND ps.symbol_name = ?
               AND ps.symbol_kind = ?",
        )
        .into_diagnostic()?;
    let rows = stmt
        .query_map(
            (
                interface.file.as_str(),
                interface.symbol.as_str(),
                interface.kind.as_str(),
            ),
            |row| row.get::<_, Option<i64>>(0),
        )
        .into_diagnostic()?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row.into_diagnostic()?);
    }
    Ok(resolve_distinct_line(values))
}

pub(crate) struct PreviewEnvelopeInput {
    pub repo_name: String,
    pub binary_version: String,
    pub head: Option<String>,
    pub limit: u64,
    pub ledger_count: u64,
    pub generated_at: Option<String>,
    pub interfaces: Vec<PublicInterface>,
}

pub(crate) fn build_preview_envelope(
    input: PreviewEnvelopeInput,
    mut line_for: impl FnMut(&PublicInterface) -> Result<Option<i64>>,
) -> Result<FederateExportPreview> {
    let PreviewEnvelopeInput {
        repo_name,
        binary_version,
        head,
        limit,
        ledger_count,
        generated_at,
        mut interfaces,
    } = input;
    interfaces.sort();
    let total_matching = interfaces.len();
    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    let truncated = total_matching > cap;
    let mut emitted = Vec::new();
    for interface in interfaces.into_iter().take(cap) {
        let line = line_for(&interface)?;
        emitted.push(FederateExportPreviewInterface {
            symbol: interface.symbol,
            file: interface.file.replace('\\', "/"),
            kind: interface.kind,
            line,
        });
    }
    let result_count = emitted.len();
    let next = if total_matching == 0 {
        Some(PREVIEW_ZERO_NEXT.to_string())
    } else {
        None
    };
    Ok(FederateExportPreview {
        schema_version: 1,
        kind: PREVIEW_KIND.to_string(),
        dry_run: true,
        repo_name,
        binary_version,
        wire_schema_version: FederatedSchema::VERSION.to_string(),
        limit,
        truncated,
        result_count,
        total_matching,
        interfaces: emitted,
        ledger_count,
        head,
        generated_at,
        next,
    })
}

pub(crate) fn render_preview_human(preview: &FederateExportPreview) -> String {
    let mut out = String::new();
    out.push_str("Federated schema preview\n");
    out.push_str(&format!("repo: {}\n", preview.repo_name));
    if let Some(head) = &preview.head {
        out.push_str(&format!("head: {head}\n"));
    }
    out.push_str(&format!("binary: {}\n", preview.binary_version));
    out.push_str(&format!(
        "interfaces: {} of {}\n",
        preview.result_count, preview.total_matching
    ));
    if preview.truncated {
        out.push_str("truncated; pass --limit\n");
    }
    for row in &preview.interfaces {
        let kind = row.kind.as_str();
        if let Some(line) = row.line {
            out.push_str(&format!(
                "  {}:{} {} ({kind})\n",
                row.file, line, row.symbol
            ));
        } else {
            out.push_str(&format!("  {} {} ({kind})\n", row.file, row.symbol));
        }
    }
    if let Some(next) = &preview.next {
        out.push_str(&format!("next: {next}\n"));
    }
    out
}

pub fn execute_federate_export(
    dry_run: bool,
    json: bool,
    out: Option<String>,
    limit: u64,
) -> Result<()> {
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

    let preview = dry_run || json;
    if !preview && out.is_none() {
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

    if preview {
        let head = match get_head_info(&repo) {
            Ok((Some(hash), _)) => Some(hash),
            _ => None,
        };
        let conn = storage.get_connection();
        let envelope = build_preview_envelope(
            PreviewEnvelopeInput {
                repo_name,
                binary_version: env!("CARGO_PKG_VERSION").to_string(),
                head,
                limit,
                ledger_count: ledger_entries.len() as u64,
                generated_at: Some(Utc::now().to_rfc3339()),
                interfaces: public_interfaces,
            },
            |interface| lookup_interface_line(conn, interface),
        )?;
        if json {
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &envelope).into_diagnostic()?;
            stdout.write_all(b"\n").into_diagnostic()?;
        } else {
            print!("{}", render_preview_human(&envelope));
        }
        return Ok(());
    }

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

pub fn execute_federate_status(json: bool) -> Result<()> {
    let current_dir = env::current_dir().into_diagnostic()?;
    let _repo = open_repo(&current_dir).into_diagnostic()?;

    let layout = crate::commands::helpers::get_layout()?;
    let storage = StorageManager::init_with_layout(&layout)?;

    let raw = get_federated_links(storage.get_connection())?;
    // 0184: path identity — present Live peers only (RO; no DELETE).
    let presented = present_federated_links(&raw, layout.root.as_str());

    if json {
        return emit_federate_status_json(&presented);
    }

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
    for link in &presented.live {
        let freshness = classify_peer_freshness(&link.path, &link.last_scanned);
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
        println!(
            "{}",
            format_freshness_line(freshness.status, freshness.reason)
        );
    }

    Ok(())
}

fn emit_federate_status_json(presented: &crate::federated::links::PresentedLinks) -> Result<()> {
    let mut peers = Vec::new();
    let mut any_stale = false;
    for link in &presented.live {
        let freshness = classify_peer_freshness(&link.path, &link.last_scanned);
        if freshness.status == crate::federated::links::FreshnessStatus::Stale {
            any_stale = true;
        }
        let mut item = serde_json::json!({
            "name": link.name,
            "path": link.path,
            "lastScanned": link.last_scanned,
            "freshness": {
                "status": freshness.status.as_str(),
                "reason": freshness.reason,
            }
        });
        if let Some(generated) = freshness.schema_generated_at
            && let Some(obj) = item.as_object_mut()
        {
            obj.insert("schemaGeneratedAt".into(), serde_json::json!(generated));
        }
        peers.push(item);
    }

    let mut envelope = serde_json::json!({
        "schemaVersion": 1,
        "peers": peers,
        "omitted": {
            "self": presented.omitted_self,
            "dead": presented.omitted_dead,
            "duplicate": presented.omitted_dup_extra,
        }
    });
    if (any_stale || presented.live.is_empty())
        && let Some(obj) = envelope.as_object_mut()
    {
        obj.insert("next".into(), serde_json::json!("ledgerful federate scan"));
    }

    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &envelope).into_diagnostic()?;
    stdout.write_all(b"\n").into_diagnostic()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;
    use rusqlite::Connection;

    fn iface(symbol: &str, file: &str, kind: SymbolKind) -> PublicInterface {
        PublicInterface {
            symbol: symbol.to_string(),
            file: file.to_string(),
            kind,
        }
    }

    fn envelope_ok(
        interfaces: Vec<PublicInterface>,
        limit: u64,
        line_for: impl FnMut(&PublicInterface) -> Result<Option<i64>>,
    ) -> FederateExportPreview {
        build_preview_envelope(
            PreviewEnvelopeInput {
                repo_name: "repo".to_string(),
                binary_version: "0.2.13".to_string(),
                head: Some("abc".to_string()),
                limit,
                ledger_count: 0,
                generated_at: None,
                interfaces,
            },
            line_for,
        )
        .expect("preview envelope")
    }

    #[test]
    fn status_read_path_does_not_scan_or_delete() {
        let src = include_str!("federate.rs");
        let status_fn = src
            .split("pub fn execute_federate_status")
            .nth(1)
            .unwrap_or("");
        let status_fn = status_fn.split("#[cfg(test)]").next().unwrap_or(status_fn);
        assert!(!status_fn.contains("execute_federate_scan"));
        assert!(!status_fn.contains("execute_federate_export"));
        assert!(!status_fn.contains("DELETE FROM"));
        assert!(!status_fn.contains("prune_dead"));
    }

    #[test]
    fn dry_run_preview_json_has_kind_and_no_banners() {
        let preview = envelope_ok(vec![iface("A", "a.rs", SymbolKind::Function)], 200, |_| {
            Ok(None)
        });
        let json = serde_json::to_string(&preview).expect("json");
        assert_eq!(preview.kind, "federateExportPreview");
        assert_eq!(preview.schema_version, 1);
        assert!(preview.dry_run);
        assert!(!json.contains("FEDERATED SCHEMA PREVIEW"));
        assert!(!json.contains("schema_version"));
    }

    #[test]
    fn dry_run_human_has_no_json_object() {
        let preview = envelope_ok(vec![iface("A", "a.rs", SymbolKind::Function)], 200, |_| {
            Ok(None)
        });
        let human = render_preview_human(&preview);
        assert!(human.to_lowercase().contains("preview"));
        assert!(!human.contains('{'));
        assert!(!human.contains("FEDERATED SCHEMA PREVIEW"));
    }

    #[test]
    fn preview_omits_line_when_lookup_misses() {
        let preview = envelope_ok(vec![iface("A", "a.rs", SymbolKind::Function)], 200, |_| {
            Ok(None)
        });
        assert_eq!(preview.interfaces.len(), 1);
        assert_eq!(preview.interfaces[0].line, None);
    }

    #[test]
    fn preview_includes_line_when_project_symbols_hit() {
        let conn = Connection::open_in_memory().expect("mem");
        conn.execute_batch(
            "CREATE TABLE project_files (id INTEGER PRIMARY KEY, file_path TEXT);
             CREATE TABLE project_symbols (
                 file_id INTEGER, symbol_name TEXT, symbol_kind TEXT, line_start INTEGER
             );
             INSERT INTO project_files (id, file_path) VALUES (1, 'src/lib.rs');
             INSERT INTO project_symbols (file_id, symbol_name, symbol_kind, line_start)
             VALUES (1, 'entry', 'Function', 12);",
        )
        .expect("seed");
        let hit = iface("entry", "src/lib.rs", SymbolKind::Function);
        assert_eq!(
            lookup_interface_line(&conn, &hit).expect("lookup"),
            Some(12)
        );
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT ps.line_start FROM project_symbols ps
                 INNER JOIN project_files pf ON ps.file_id = pf.id
                 WHERE pf.file_path = ? AND ps.symbol_name = ? AND ps.symbol_kind = ?",
            )
            .expect("prep");
        let camel_miss: Vec<Option<i64>> = stmt
            .query_map(("src/lib.rs", "entry", "function"), |row| {
                row.get::<_, Option<i64>>(0)
            })
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        assert!(
            resolve_distinct_line(camel_miss).is_none(),
            "camelCase kind must miss PascalCase DB rows"
        );
    }

    #[test]
    fn preview_truncates_at_limit_and_sets_truncated() {
        let preview = envelope_ok(
            vec![
                iface("b", "z.rs", SymbolKind::Function),
                iface("a", "m.rs", SymbolKind::Struct),
                iface("c", "a.rs", SymbolKind::Function),
            ],
            2,
            |_| Ok(None),
        );
        assert!(preview.truncated);
        assert_eq!(preview.result_count, 2);
        assert_eq!(preview.total_matching, 3);
        assert_eq!(preview.interfaces[0].symbol, "a");
        assert_eq!(preview.interfaces[1].symbol, "b");
    }

    #[test]
    fn preview_zero_sets_next_scan_impact() {
        let preview = envelope_ok(Vec::new(), 200, |_| Ok(None));
        assert_eq!(preview.next.as_deref(), Some("ledgerful scan --impact"));
        assert_eq!(preview.total_matching, 0);
        let human = render_preview_human(&preview);
        assert!(human.contains("ledgerful scan --impact"));
    }

    #[test]
    fn resolve_distinct_line_omits_on_conflict_or_null_only() {
        assert_eq!(resolve_distinct_line([Some(1), Some(1)]), Some(1));
        assert_eq!(resolve_distinct_line([Some(1), Some(2)]), None);
        assert_eq!(resolve_distinct_line([None, None]), None);
        assert_eq!(resolve_distinct_line([None, Some(4)]), Some(4));
    }
}
