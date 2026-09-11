use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use clap::Args;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream};
use std::collections::HashMap;

#[derive(Args, Debug)]
pub struct EndpointsArgs {
    /// Filter by method (e.g. GET, POST)
    #[arg(short, long)]
    method: Option<String>,
    /// Filter by path pattern
    #[arg(short, long)]
    path: Option<String>,
    /// Show auth details
    #[arg(long)]
    auth: bool,
    /// Only show endpoints matched by the change set (handler symbol, impl file,
    /// registration file, or optional blast edges) — not registration-file only
    #[arg(long)]
    changed: bool,
    /// Include fixture and in-file test routes omitted from the default list
    #[arg(long)]
    include_fixtures: bool,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

impl EndpointsArgs {
    /// Whether `--json` is set (machine-output selection).
    pub fn wants_json(&self) -> bool {
        self.json
    }

    /// Long flag names that are present (values stripped). Used for `argv_hash` shape.
    pub fn present_flag_names(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.method.is_some() {
            f.push("method");
        }
        if self.path.is_some() {
            f.push("path");
        }
        if self.auth {
            f.push("auth");
        }
        if self.changed {
            f.push("changed");
        }
        if self.include_fixtures {
            f.push("include-fixtures");
        }
        if self.json {
            f.push("json");
        }
        f
    }
}

/// One `api_routes` row as selected for the endpoints list surface.
#[derive(Debug, Clone, PartialEq)]
struct EndpointRow {
    id: i64,
    method: String,
    path_pattern: String,
    handler_symbol_name: Option<String>,
    framework: String,
    auth_requirements: Option<String>,
    owning_service: Option<String>,
    consumers: Option<String>,
    file_path: Option<String>,
    route_confidence: f64,
    handler_symbol_id: Option<i64>,
    evidence: Option<String>,
    route_source: String,
    mount_prefix: Option<String>,
    handler_file: Option<String>,
}

/// Dedupe key: (method uppercase, path_pattern, framework) — exact 0118 3-tuple.
type EndpointDedupeKey = (String, String, String);

/// Collapse stacked identical route identities to one row.
///
/// Keep-best: higher `route_confidence`; then non-empty handler; then lex lower
/// handler; then lower `id`. After dedupe, sort `path_pattern ASC`, `method ASC`,
/// `framework ASC`, then `id ASC` for multi-framework same path+method determinism.
fn dedupe_endpoint_rows(rows: Vec<EndpointRow>) -> Vec<EndpointRow> {
    let mut best: HashMap<EndpointDedupeKey, EndpointRow> = HashMap::new();

    for row in rows {
        let key = (
            row.method.to_uppercase(),
            row.path_pattern.clone(),
            row.framework.clone(),
        );
        match best.get(&key) {
            None => {
                best.insert(key, row);
            }
            Some(prev) => {
                if endpoint_row_better_than(&row, prev) {
                    best.insert(key, row);
                }
            }
        }
    }

    let mut out: Vec<EndpointRow> = best.into_values().collect();
    out.sort_by(|a, b| {
        a.path_pattern
            .cmp(&b.path_pattern)
            .then_with(|| a.method.cmp(&b.method))
            .then_with(|| a.framework.cmp(&b.framework))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Whether `cand` should replace `prev` under keep-best rules.
fn endpoint_row_better_than(cand: &EndpointRow, prev: &EndpointRow) -> bool {
    // 1. Higher route_confidence
    if cand.route_confidence > prev.route_confidence {
        return true;
    }
    if cand.route_confidence < prev.route_confidence {
        return false;
    }

    let cand_handler = cand.handler_symbol_name.as_deref().unwrap_or("");
    let prev_handler = prev.handler_symbol_name.as_deref().unwrap_or("");
    let cand_nonempty = !cand_handler.is_empty();
    let prev_nonempty = !prev_handler.is_empty();

    // 2. Non-empty handler preferred
    if cand_nonempty && !prev_nonempty {
        return true;
    }
    if !cand_nonempty && prev_nonempty {
        return false;
    }

    // 3. Lex lower handler
    if cand_handler < prev_handler {
        return true;
    }
    if cand_handler > prev_handler {
        return false;
    }

    // 4. Lower id
    cand.id < prev.id
}

/// SELECT `api_routes` with optional method/path filters, apply optional
/// `--changed` key filter, then emit-time dedupe.
///
/// Returns `(deduped_rows, raw_sql_was_empty)` so empty-state fencing stays on
/// raw SQL emptiness (before filter/dedupe), matching `execute_endpoints`.
fn query_filter_and_dedupe_endpoints(
    conn: &rusqlite::Connection,
    method: Option<&str>,
    path: Option<&str>,
    matched_route_keys: Option<&std::collections::HashSet<(String, String)>>,
) -> Result<(Vec<EndpointRow>, bool)> {
    let mut query = String::from(
        "SELECT ar.id, ar.method, ar.path_pattern, ar.handler_symbol_name, ar.framework, \
         ar.auth_requirements, ar.owning_service, ar.consumers, pf.file_path, \
         ar.route_confidence, ar.handler_symbol_id, ar.evidence, ar.route_source, \
         ar.mount_prefix, impl_pf.file_path \
         FROM api_routes ar \
         LEFT JOIN project_files pf ON ar.handler_file_id = pf.id \
         LEFT JOIN project_symbols ps ON ar.handler_symbol_id = ps.id \
         LEFT JOIN project_files impl_pf ON ps.file_id = impl_pf.id \
         WHERE 1=1",
    );
    let mut params: Vec<String> = Vec::new();

    if let Some(m) = method {
        query.push_str(" AND ar.method = ?");
        params.push(m.to_uppercase());
    }
    if let Some(p) = path {
        query.push_str(" AND ar.path_pattern LIKE ?");
        params.push(format!("%{}%", p));
    }

    query.push_str(" ORDER BY path_pattern ASC");

    let mut stmt = conn.prepare(&query).into_diagnostic()?;
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let all_rows: Vec<EndpointRow> = stmt
        .query_map(&params_refs[..], |row| {
            Ok(EndpointRow {
                id: row.get::<_, i64>(0)?,
                method: row.get::<_, String>(1)?,
                path_pattern: row.get::<_, String>(2)?,
                handler_symbol_name: row.get::<_, Option<String>>(3)?,
                framework: row.get::<_, String>(4)?,
                auth_requirements: row.get::<_, Option<String>>(5)?,
                owning_service: row.get::<_, Option<String>>(6)?,
                consumers: row.get::<_, Option<String>>(7)?,
                file_path: row.get::<_, Option<String>>(8)?,
                route_confidence: row.get::<_, f64>(9)?,
                handler_symbol_id: row.get::<_, Option<i64>>(10)?,
                evidence: row.get::<_, Option<String>>(11)?,
                route_source: row
                    .get::<_, Option<String>>(12)?
                    .unwrap_or_else(|| "DECORATOR".to_string()),
                mount_prefix: row.get::<_, Option<String>>(13)?,
                handler_file: row.get::<_, Option<String>>(14)?,
            })
        })
        .into_diagnostic()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;

    // Load-bearing empty-state fence: raw SQL emptiness before dedupe/--changed.
    let all_rows_empty = all_rows.is_empty();

    // Apply --changed filter: keep routes matched by shared affected-flows lib.
    let filtered: Vec<EndpointRow> = if let Some(keys) = matched_route_keys {
        all_rows
            .into_iter()
            .filter(|r| keys.contains(&(r.method.to_uppercase(), r.path_pattern.clone())))
            .collect()
    } else {
        all_rows
    };

    // Emit-time dedupe after SELECT and after --changed filter (human + JSON).
    Ok((dedupe_endpoint_rows(filtered), all_rows_empty))
}

pub fn execute_endpoints(args: EndpointsArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let conn = storage.get_connection();

    // --changed: uncapped match keys from shared affected-flows library
    // (handler symbol / impl file / registration file). Report payloads still
    // apply FLOWS_CAP; the filter must not inherit that truncate.
    // JSON keys stay non-breaking; only which rows appear widens vs the old
    // registration-file-only filter.
    //
    // endpoints --changed is a route-map filter on the working-tree diff (local
    // symbols via map_snapshot_to_packet), NOT full blast-radius impact.
    // P4–P5 blast edges are dropped intentionally (`blast_radius: None`).
    // No execute_impact_silent / federation / cache rewrite (0146).
    let matched_route_keys: Option<std::collections::HashSet<(String, String)>> = if args.changed {
        use crate::git::RepoSnapshot;
        use crate::git::repo::{get_head_info, open_repo};
        use crate::git::status::collect_changed_files_for_filter;
        use crate::impact::enrichment::affected_flows::{
            AffectedFlowsOpts, match_affected_route_keys,
        };
        use crate::impact::orchestrator::map_snapshot_to_packet;

        let wt_changes = collect_changed_files_for_filter(&layout)?;
        if wt_changes.is_empty() {
            // Empty short-circuit: no symbol extract / impact; honest CleanDiff
            // vs NoIndexedData still decided downstream via all_rows_empty.
            Some(std::collections::HashSet::new())
        } else {
            let repo = open_repo(layout.root.as_std_path())?;
            let (head_hash, branch_name) = get_head_info(&repo)?;
            let snapshot = RepoSnapshot {
                head_hash: head_hash.clone(),
                branch_name,
                is_clean: false,
                changes: wt_changes,
            };
            // Symbol extract only — no orchestrator.run / federation / persist.
            let packet = map_snapshot_to_packet(snapshot, layout.root.as_std_path())?;
            let opts = AffectedFlowsOpts {
                head_hash: packet.head_hash.clone(),
            };
            // Non-available statuses (empty_map / missing_table / no_change_seeds)
            // → empty set (honest empty --changed). Matcher Err (true failures)
            // propagates so we never claim "no endpoints changed" after a fault.
            let set =
                match_affected_route_keys(conn, &packet.changes, None /* blast */, &opts)?;
            Some(set)
        }
    } else {
        None
    };

    let (rows, all_rows_empty) = query_filter_and_dedupe_endpoints(
        conn,
        args.method.as_deref(),
        args.path.as_deref(),
        matched_route_keys.as_ref(),
    )?;

    let (rows, fixtures_omitted) = omit_fixture_endpoint_rows(rows, args.include_fixtures);

    if args.json {
        let mut results = Vec::new();
        for row in &rows {
            results.push(endpoint_row_to_json(row));
        }
        let mut output = crate::output::empty::format_json_empty_state(results, "results", || {
            if all_rows_empty {
                (
                    crate::output::empty::EmptyReason::NoIndexedData,
                    "No endpoints indexed. Endpoints are extracted from HTTP route registrations \
                     (Axum, Express, etc.). Run `ledgerful index --incremental` if routes exist, \
                     or confirm your framework is supported."
                        .to_string(),
                )
            } else {
                (
                    crate::output::empty::EmptyReason::CleanDiff,
                    "No endpoints changed in the current diff.".to_string(),
                )
            }
        });
        attach_fixture_flags(&mut output, args.include_fixtures, fixtures_omitted);
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        let mut table = Table::new();
        table.set_header(vec![
            "Method",
            "Path",
            "Handler",
            "Framework",
            "Service",
            "Auth",
        ]);

        for row in &rows {
            // Parse as Option<Vec<String>> — the writer's exact type
            // (routes.rs serializes Option<Vec<String>>). Every real state is a
            // success arm: "null" → Ok(None), "[]" → Ok(Some([])), '["a"]' → Ok(Some).
            // Neighbours parse Vec<String> and recover null via parse failure;
            // we deliberately do not copy that pattern here. Malformed → Invalid.
            let auth_str = match &row.auth_requirements {
                Some(aj) => format_auth_requirements(aj),
                None => "Unknown".to_string(),
            };

            table.add_row(vec![
                row.method.clone(),
                row.path_pattern.clone(),
                row.handler_symbol_name
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                row.framework.clone(),
                row.owning_service
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                auth_str,
            ]);
        }
        if rows.is_empty() {
            if all_rows_empty {
                println!(
                    "{}",
                    "  No endpoints indexed. Endpoints are extracted from HTTP route registrations \
                     (Axum, Express, etc.). Run `ledgerful index --incremental` if routes exist, \
                     or confirm your framework is supported.".if_supports_color(Stream::Stdout, |s| s.dimmed())

                );
            } else {
                println!(
                    "{}",
                    "  No endpoints changed in the current diff."
                        .if_supports_color(Stream::Stdout, |s| s.dimmed())
                );
            }
        }
        println!("{}", table);
        print_endpoints_omit_footer(fixtures_omitted);
    }

    Ok(())
}

fn omit_fixture_endpoint_rows(
    rows: Vec<EndpointRow>,
    include_fixtures: bool,
) -> (Vec<EndpointRow>, usize) {
    if include_fixtures {
        return (rows, 0);
    }
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for row in rows {
        if is_fixture_endpoint_row(&row) {
            omitted += 1;
        } else {
            kept.push(row);
        }
    }
    (kept, omitted)
}

fn is_fixture_endpoint_row(row: &EndpointRow) -> bool {
    if row.route_source == "TEST" {
        return true;
    }
    row.file_path
        .as_deref()
        .is_some_and(crate::index::test_mapping::is_test_path)
}

fn attach_fixture_flags(output: &mut serde_json::Value, include_fixtures: bool, omitted: usize) {
    if let Some(obj) = output.as_object_mut() {
        obj.insert(
            "includeFixtures".to_string(),
            serde_json::json!(include_fixtures),
        );
        obj.insert("fixturesOmitted".to_string(), serde_json::json!(omitted));
    }
}

fn print_endpoints_omit_footer(omitted: usize) {
    if omitted > 0 {
        println!("  {omitted} fixture routes omitted. Pass --include-fixtures to show them.");
    }
}

fn slash_normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn join_mount(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}/{path}")
    }
}

fn handler_unresolved_reason(
    handler: Option<&str>,
    symbol_id: Option<i64>,
    evidence: Option<&str>,
) -> Option<&'static str> {
    let name = handler.unwrap_or("");
    let sentinel =
        name.is_empty() || name == "unknown" || name == "<unknown>" || name == "<func_literal>";
    if !sentinel && symbol_id.is_none() {
        return Some("missing_symbol");
    }
    let ev = evidence.unwrap_or("");
    if sentinel && ev.contains("-> <closure>") {
        return Some("closure");
    }
    if sentinel && ev.contains("-> <not_identifier>") {
        return Some("not_identifier");
    }
    if sentinel {
        return Some("unresolved");
    }
    None
}

fn endpoint_row_to_json(row: &EndpointRow) -> serde_json::Value {
    let mut item = serde_json::Map::new();
    item.insert("method".to_string(), serde_json::json!(row.method));
    item.insert("path".to_string(), serde_json::json!(row.path_pattern));
    item.insert(
        "handler".to_string(),
        serde_json::json!(row.handler_symbol_name),
    );
    item.insert("framework".to_string(), serde_json::json!(row.framework));
    item.insert("service".to_string(), serde_json::json!(row.owning_service));

    if let Some(raw) = row.auth_requirements.as_deref() {
        match serde_json::from_str::<Option<Vec<String>>>(raw) {
            Ok(Some(v)) if !v.is_empty() => {
                item.insert("auth".to_string(), serde_json::json!(v));
                item.insert("authSource".to_string(), serde_json::json!("inferred"));
            }
            Ok(_) => {}
            Err(_) => {
                item.insert("authParse".to_string(), serde_json::json!("invalid"));
            }
        }
    }

    if let Some(raw) = row.consumers.as_deref() {
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v) if !v.is_null() => {
                item.insert("consumers".to_string(), v);
            }
            Ok(_) => {}
            Err(_) => {
                item.insert("consumersParse".to_string(), serde_json::json!("invalid"));
            }
        }
    }

    if let Some(reg) = row.file_path.as_deref().filter(|p| !p.is_empty()) {
        let registration = slash_normalize(reg);
        item.insert(
            "registrationFile".to_string(),
            serde_json::json!(registration),
        );
        if let Some(impl_file) = row.handler_file.as_deref().filter(|p| !p.is_empty()) {
            let impl_norm = slash_normalize(impl_file);
            if impl_norm != registration {
                item.insert("handlerFile".to_string(), serde_json::json!(impl_norm));
            }
        }
    }

    if let Some(reason) = handler_unresolved_reason(
        row.handler_symbol_name.as_deref(),
        row.handler_symbol_id,
        row.evidence.as_deref(),
    ) {
        item.insert(
            "handlerUnresolvedReason".to_string(),
            serde_json::json!(reason),
        );
    }

    if let Some(prefix) = row.mount_prefix.as_deref() {
        item.insert("mountPrefix".to_string(), serde_json::json!(prefix));
        item.insert(
            "mountedPath".to_string(),
            serde_json::json!(join_mount(prefix, &row.path_pattern)),
        );
        item.insert("mountProvenance".to_string(), serde_json::json!("nest"));
    } else if row.framework == "Axum" {
        item.insert(
            "mountedPath".to_string(),
            serde_json::json!(row.path_pattern),
        );
        item.insert("mountProvenance".to_string(), serde_json::json!("literal"));
    } else {
        item.insert("mountProvenance".to_string(), serde_json::json!("unknown"));
    }

    serde_json::Value::Object(item)
}

/// Format stored `auth_requirements` JSON for the human Auth column.
///
/// Parses as `Option<Vec<String>>` — the writer's exact type — so `"null"`,
/// `"[]"`, and non-empty arrays are all success arms. Empty / null → `"None"`.
/// Preserves stored order for determinism.
fn format_auth_requirements(aj: &str) -> String {
    match serde_json::from_str::<Option<Vec<String>>>(aj) {
        Ok(None) => "None".to_string(),
        Ok(Some(v)) if v.is_empty() => "None".to_string(),
        Ok(Some(v)) => v.join(", "),
        Err(_) => "Invalid".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EndpointRow, attach_fixture_flags, dedupe_endpoint_rows, endpoint_row_better_than,
        endpoint_row_to_json, format_auth_requirements, omit_fixture_endpoint_rows,
        query_filter_and_dedupe_endpoints,
    };
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;

    fn row(
        id: i64,
        method: &str,
        path: &str,
        handler: Option<&str>,
        framework: &str,
        confidence: f64,
    ) -> EndpointRow {
        EndpointRow {
            id,
            method: method.to_string(),
            path_pattern: path.to_string(),
            handler_symbol_name: handler.map(str::to_string),
            framework: framework.to_string(),
            auth_requirements: None,
            owning_service: None,
            consumers: None,
            file_path: None,
            route_confidence: confidence,
            handler_symbol_id: None,
            evidence: None,
            route_source: "DECORATOR".to_string(),
            mount_prefix: None,
            handler_file: None,
        }
    }

    fn in_memory_storage() -> StorageManager {
        let conn = Connection::open_in_memory().unwrap();
        let mut conn = conn;
        get_migrations().to_latest(&mut conn).unwrap();
        StorageManager::init_from_conn(conn)
    }

    fn seed_route(
        conn: &Connection,
        file_id: i64,
        method: &str,
        path: &str,
        handler: &str,
        framework: &str,
        confidence: f64,
    ) {
        conn.execute(
            "INSERT INTO api_routes
                (method, path_pattern, handler_symbol_name, handler_file_id,
                 framework, route_source, is_dynamic, route_confidence, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                method,
                path,
                handler,
                file_id,
                framework,
                "DECORATOR",
                0,
                confidence,
                "2026-05-01T00:00:00Z",
            ],
        )
        .unwrap();
    }

    /// Four shapes `routes.rs` can write — all must land in the Ok arm.
    #[test]
    fn format_auth_requirements_writer_shapes() {
        assert_eq!(format_auth_requirements("null"), "None");
        assert_eq!(format_auth_requirements("[]"), "None");
        assert_eq!(format_auth_requirements(r#"["secured"]"#), "secured");
        assert_eq!(format_auth_requirements(r#"["a","b"]"#), "a, b");
    }

    #[test]
    fn format_auth_requirements_ok_arm_not_error_recovery() {
        // Unlike neighbours that parse Vec and recover null via Err, null must parse Ok.
        let parsed: Result<Option<Vec<String>>, _> = serde_json::from_str("null");
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap(), None);
    }

    #[test]
    fn dedupe_collapses_identical_method_path_framework() {
        let rows = vec![
            row(1, "GET", "/changes", Some("changes_handler"), "Axum", 1.0),
            row(2, "GET", "/changes", Some("changes_handler"), "Axum", 1.0),
            row(3, "GET", "/changes", Some("changes_handler"), "Axum", 1.0),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path_pattern, "/changes");
        assert_eq!(out[0].method, "GET");
        assert_eq!(out[0].framework, "Axum");
        // Tie → lower id
        assert_eq!(out[0].id, 1);
    }

    #[test]
    fn dedupe_keeps_get_and_post_same_path() {
        let rows = vec![
            row(1, "GET", "/users", Some("get_users"), "Axum", 1.0),
            row(2, "POST", "/users", Some("create_user"), "Axum", 1.0),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 2);
        let methods: Vec<&str> = out.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
    }

    #[test]
    fn dedupe_keeps_axum_and_express_same_path() {
        // Express first by insertion / lower id so HashMap order cannot fake ASC.
        let rows = vec![
            row(2, "GET", "/health", Some("express_health"), "Express", 1.0),
            row(1, "GET", "/health", Some("axum_health"), "Axum", 1.0),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 2);
        // path+method equal → framework ASC (Axum before Express)
        assert_eq!(out[0].framework, "Axum");
        assert_eq!(out[0].path_pattern, "/health");
        assert_eq!(out[0].method, "GET");
        assert_eq!(out[1].framework, "Express");
        assert_eq!(out[1].path_pattern, "/health");
        assert_eq!(out[1].method, "GET");
    }

    #[test]
    fn dedupe_sort_stable_path_then_method_then_framework() {
        let rows = vec![
            row(1, "POST", "/z", Some("z_post"), "Axum", 1.0),
            row(2, "GET", "/a", Some("a_get"), "Express", 1.0),
            row(3, "GET", "/z", Some("z_get"), "Axum", 1.0),
            row(4, "DELETE", "/a", Some("a_del"), "Axum", 1.0),
            row(5, "GET", "/a", Some("a_get_axum"), "Axum", 1.0),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 5);
        // path ASC, method ASC, framework ASC
        assert_eq!(out[0].path_pattern, "/a");
        assert_eq!(out[0].method, "DELETE");
        assert_eq!(out[1].path_pattern, "/a");
        assert_eq!(out[1].method, "GET");
        assert_eq!(out[1].framework, "Axum");
        assert_eq!(out[2].path_pattern, "/a");
        assert_eq!(out[2].method, "GET");
        assert_eq!(out[2].framework, "Express");
        assert_eq!(out[3].path_pattern, "/z");
        assert_eq!(out[3].method, "GET");
        assert_eq!(out[4].path_pattern, "/z");
        assert_eq!(out[4].method, "POST");
    }

    /// SELECT + filter + dedupe against a real migrated SQLite conn: three stacked
    /// identical routes collapse to one emit row alongside a distinct second route.
    #[test]
    fn query_filter_and_dedupe_collapses_stacked_identical_routes() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/routes.rs",
                "Rust",
                "hash_stack",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        // Three stacked identical identities (legacy multi-pass index residue).
        seed_route(
            conn,
            file_id,
            "GET",
            "/changes",
            "changes_handler",
            "Axum",
            1.0,
        );
        seed_route(
            conn,
            file_id,
            "GET",
            "/changes",
            "changes_handler",
            "Axum",
            1.0,
        );
        seed_route(
            conn,
            file_id,
            "GET",
            "/changes",
            "changes_handler",
            "Axum",
            1.0,
        );
        // Distinct route must survive.
        seed_route(
            conn,
            file_id,
            "POST",
            "/changes",
            "create_change",
            "Axum",
            1.0,
        );

        let raw_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM api_routes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw_count, 4, "fixture must leave stacked rows in the table");

        let (rows, raw_empty) =
            query_filter_and_dedupe_endpoints(conn, None, None, None).expect("query+dedupe");
        assert!(!raw_empty);
        assert_eq!(
            rows.len(),
            2,
            "three stacked GET /changes Axum collapse to one; POST remains"
        );
        assert_eq!(rows[0].method, "GET");
        assert_eq!(rows[0].path_pattern, "/changes");
        assert_eq!(rows[0].framework, "Axum");
        assert_eq!(rows[1].method, "POST");
        assert_eq!(rows[1].path_pattern, "/changes");

        // Uniqueness of emit keys (method upper, path, framework).
        let mut keys: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.method.to_uppercase(),
                    r.path_pattern.clone(),
                    r.framework.clone(),
                )
            })
            .collect();
        keys.sort();
        let mut uniq = keys.clone();
        uniq.dedup();
        assert_eq!(
            keys, uniq,
            "emit rows must be unique on (method, path, framework)"
        );
    }

    #[test]
    fn query_filter_and_dedupe_raw_empty_fence() {
        let storage = in_memory_storage();
        let conn = storage.get_connection();
        let (rows, raw_empty) =
            query_filter_and_dedupe_endpoints(conn, None, None, None).expect("query+dedupe");
        assert!(raw_empty);
        assert!(rows.is_empty());
    }

    /// `--changed` path: matched_route_keys = Some(...) filters before emit-time
    /// dedupe. Stacked GET /changes collapse to one; unrelated only if key present.
    #[test]
    fn query_filter_and_dedupe_with_matched_route_keys() {
        use std::collections::HashSet;

        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/routes.rs",
                "Rust",
                "hash_changed",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();

        // Three stacked identical GET /changes Axum identities.
        for _ in 0..3 {
            seed_route(
                conn,
                file_id,
                "GET",
                "/changes",
                "changes_handler",
                "Axum",
                1.0,
            );
        }
        // Unrelated route — only appears when its key is in the set.
        seed_route(
            conn,
            file_id,
            "POST",
            "/other",
            "other_handler",
            "Axum",
            1.0,
        );

        let mut keys = HashSet::new();
        keys.insert(("GET".to_string(), "/changes".to_string()));

        let (rows, raw_empty) =
            query_filter_and_dedupe_endpoints(conn, None, None, Some(&keys)).expect("query+dedupe");
        assert!(!raw_empty, "table has rows so raw SQL is non-empty");
        assert_eq!(
            rows.len(),
            1,
            "three stacked GET /changes collapse to one emit"
        );
        assert_eq!(rows[0].method, "GET");
        assert_eq!(rows[0].path_pattern, "/changes");
        assert_eq!(rows[0].framework, "Axum");

        let mut emit_keys: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.method.to_uppercase(),
                    r.path_pattern.clone(),
                    r.framework.clone(),
                )
            })
            .collect();
        emit_keys.sort();
        let mut uniq = emit_keys.clone();
        uniq.dedup();
        assert_eq!(
            emit_keys, uniq,
            "emit rows must be unique on (method, path, framework)"
        );

        // With both keys present, unrelated POST /other is included (deduped).
        keys.insert(("POST".to_string(), "/other".to_string()));
        let (rows2, _) =
            query_filter_and_dedupe_endpoints(conn, None, None, Some(&keys)).expect("query+dedupe");
        assert_eq!(rows2.len(), 2);
        assert!(
            rows2
                .iter()
                .any(|r| r.method == "POST" && r.path_pattern == "/other")
        );
    }

    /// CleanDiff fence: empty matched key set with a non-empty table → empty
    /// emit but `all_rows_empty == false` (not NoIndexedData).
    #[test]
    fn query_filter_and_dedupe_empty_keys_clean_diff_fence() {
        use std::collections::HashSet;

        let storage = in_memory_storage();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "src/routes.rs",
                "Rust",
                "hash_clean_diff",
                100,
                "2026-05-01T00:00:00Z",
            ),
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        seed_route(
            conn,
            file_id,
            "GET",
            "/changes",
            "changes_handler",
            "Axum",
            1.0,
        );

        let empty_keys = HashSet::new();
        let (rows, raw_empty) =
            query_filter_and_dedupe_endpoints(conn, None, None, Some(&empty_keys))
                .expect("query+dedupe");
        assert!(rows.is_empty(), "no matched keys → empty emit");
        assert!(
            !raw_empty,
            "table has rows: all_rows_empty must stay false (CleanDiff fence)"
        );
    }

    #[test]
    fn dedupe_keep_best_higher_confidence() {
        let rows = vec![
            row(1, "GET", "/probe", Some("low"), "Axum", 0.5),
            row(2, "GET", "/probe", Some("high"), "Axum", 0.9),
            row(3, "GET", "/probe", Some("mid"), "Axum", 0.7),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 2);
        assert_eq!(out[0].handler_symbol_name.as_deref(), Some("high"));
        assert!((out[0].route_confidence - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn dedupe_keep_best_nonempty_handler_then_lex_lower_then_lower_id() {
        // Same confidence: non-empty beats empty
        assert!(endpoint_row_better_than(
            &row(2, "GET", "/x", Some("h"), "Axum", 1.0),
            &row(1, "GET", "/x", None, "Axum", 1.0),
        ));
        // Same confidence + both non-empty: lex lower handler
        assert!(endpoint_row_better_than(
            &row(2, "GET", "/x", Some("aaa"), "Axum", 1.0),
            &row(1, "GET", "/x", Some("zzz"), "Axum", 1.0),
        ));
        // Full tie except id: lower id wins
        assert!(endpoint_row_better_than(
            &row(1, "GET", "/x", Some("h"), "Axum", 1.0),
            &row(9, "GET", "/x", Some("h"), "Axum", 1.0),
        ));
        assert!(!endpoint_row_better_than(
            &row(9, "GET", "/x", Some("h"), "Axum", 1.0),
            &row(1, "GET", "/x", Some("h"), "Axum", 1.0),
        ));
    }

    #[test]
    fn dedupe_method_case_normalized_in_key() {
        let rows = vec![
            row(1, "get", "/items", Some("h"), "Axum", 1.0),
            row(2, "GET", "/items", Some("h"), "Axum", 1.0),
        ];
        let out = dedupe_endpoint_rows(rows);
        assert_eq!(out.len(), 1);
        // lower id wins on full tie
        assert_eq!(out[0].id, 1);
    }

    /// Empty-state fence: raw-empty must drive NoIndexedData path, independent of
    /// post-dedupe emptiness. Documents ordering contract (all_rows_empty before
    /// dedupe); helper on empty input stays empty.
    #[test]
    fn empty_state_fence_raw_empty_before_dedupe() {
        let all_rows: Vec<EndpointRow> = Vec::new();
        let all_rows_empty = all_rows.is_empty();
        let deduped = dedupe_endpoint_rows(all_rows);
        assert!(all_rows_empty);
        assert!(deduped.is_empty());
        // Filter-empty (non-raw) is a different reason: raw non-empty → not NoIndexedData.
        let stacked = [
            row(1, "GET", "/a", Some("h"), "Axum", 1.0),
            row(2, "GET", "/a", Some("h"), "Axum", 1.0),
        ];
        let raw_empty = stacked.is_empty();
        let after_filter: Vec<EndpointRow> = Vec::new(); // --changed matched nothing
        let after_dedupe = dedupe_endpoint_rows(after_filter);
        assert!(!raw_empty);
        assert!(after_dedupe.is_empty());
        // all_rows_empty stays false so empty-state reason is CleanDiff, not NoIndexedData
    }

    #[allow(clippy::too_many_arguments)]
    fn row_with(
        id: i64,
        method: &str,
        path: &str,
        handler: Option<&str>,
        framework: &str,
        file_path: Option<&str>,
        route_source: &str,
        mount_prefix: Option<&str>,
        evidence: Option<&str>,
        symbol_id: Option<i64>,
        auth: Option<&str>,
        consumers: Option<&str>,
    ) -> EndpointRow {
        let mut r = row(id, method, path, handler, framework, 1.0);
        r.file_path = file_path.map(str::to_string);
        r.route_source = route_source.to_string();
        r.mount_prefix = mount_prefix.map(str::to_string);
        r.evidence = evidence.map(str::to_string);
        r.handler_symbol_id = symbol_id;
        r.auth_requirements = auth.map(str::to_string);
        r.consumers = consumers.map(str::to_string);
        r
    }

    #[test]
    fn format_auth_requirements_invalid_is_invalid() {
        assert_eq!(format_auth_requirements("{not-json"), "Invalid");
    }

    #[test]
    fn endpoints_default_omits_test_path_and_test_source() {
        let rows = vec![
            row_with(
                1,
                "GET",
                "/items",
                Some("listUsers"),
                "gin",
                Some("tests/fixtures/go_sample/pkg/handlers.go"),
                "METHOD_CALL",
                None,
                None,
                None,
                None,
                None,
            ),
            row_with(
                2,
                "GET",
                "/probe",
                Some("unknown"),
                "Axum",
                Some("src/commands/web/server/startup.rs"),
                "TEST",
                None,
                Some("get(/probe) -> <closure>"),
                None,
                None,
                None,
            ),
            row_with(
                3,
                "GET",
                "/session",
                Some("session_handler"),
                "Axum",
                Some("src/commands/web/server/router.rs"),
                "BUILDER",
                Some("/api"),
                Some("get(/session) -> session_handler"),
                Some(1),
                None,
                None,
            ),
        ];
        let (kept, omitted) = omit_fixture_endpoint_rows(rows, false);
        assert_eq!(omitted, 2);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path_pattern, "/session");
    }

    #[test]
    fn endpoints_windows_test_path_still_omits() {
        let rows = vec![row_with(
            1,
            "GET",
            "/users",
            Some("listUsers"),
            "gin",
            Some(r"tests\fixtures\go_sample\pkg\handlers.go"),
            "METHOD_CALL",
            None,
            None,
            None,
            None,
            None,
        )];
        let (kept, omitted) = omit_fixture_endpoint_rows(rows, false);
        assert_eq!(omitted, 1);
        assert!(kept.is_empty());
    }

    #[test]
    fn endpoints_omit_footer_counts_tuples_not_paths() {
        let rows = vec![
            row_with(
                1,
                "GET",
                "/users",
                Some("listUsers"),
                "gin",
                Some("tests/fixtures/go_sample/pkg/handlers.go"),
                "METHOD_CALL",
                None,
                None,
                None,
                None,
                None,
            ),
            row_with(
                2,
                "POST",
                "/users",
                Some("createUser"),
                "gin",
                Some("tests/fixtures/go_sample/pkg/handlers.go"),
                "METHOD_CALL",
                None,
                None,
                None,
                None,
                None,
            ),
        ];
        let (_kept, omitted) = omit_fixture_endpoint_rows(rows, false);
        assert_eq!(omitted, 2);
    }

    #[test]
    fn endpoints_include_fixtures_restores() {
        let rows = vec![
            row_with(
                1,
                "GET",
                "/items",
                Some("listUsers"),
                "gin",
                Some("tests/fixtures/go_sample/pkg/handlers.go"),
                "METHOD_CALL",
                None,
                None,
                None,
                None,
                None,
            ),
            row_with(
                2,
                "GET",
                "/probe",
                Some("unknown"),
                "Axum",
                Some("src/startup.rs"),
                "TEST",
                None,
                None,
                None,
                None,
                None,
            ),
            row_with(
                3,
                "GET",
                "/session",
                Some("session_handler"),
                "Axum",
                Some("src/router.rs"),
                "BUILDER",
                None,
                None,
                Some(1),
                None,
                None,
            ),
        ];
        let (kept, omitted) = omit_fixture_endpoint_rows(rows, true);
        assert_eq!(omitted, 0);
        assert_eq!(kept.len(), 3);
        let mut envelope = crate::output::empty::format_json_list_envelope(
            kept.iter().map(endpoint_row_to_json).collect(),
            "results",
        );
        attach_fixture_flags(&mut envelope, true, omitted);
        assert_eq!(envelope["includeFixtures"], true);
        assert_eq!(envelope["fixturesOmitted"], 0);
    }

    #[test]
    fn endpoints_include_fixtures_echoed_on_empty() {
        let mut empty = crate::output::empty::format_json_empty_state(
            Vec::<serde_json::Value>::new(),
            "results",
            || {
                (
                    crate::output::empty::EmptyReason::NoIndexedData,
                    "No endpoints indexed.".to_string(),
                )
            },
        );
        attach_fixture_flags(&mut empty, false, 0);
        assert_eq!(empty["includeFixtures"], false);
        assert_eq!(empty["fixturesOmitted"], 0);
        assert_eq!(empty["resultCount"], 0);

        let mut clean = crate::output::empty::format_json_empty_state(
            Vec::<serde_json::Value>::new(),
            "results",
            || {
                (
                    crate::output::empty::EmptyReason::CleanDiff,
                    "No endpoints changed in the current diff.".to_string(),
                )
            },
        );
        attach_fixture_flags(&mut clean, true, 0);
        assert_eq!(clean["includeFixtures"], true);
        assert_eq!(clean["fixturesOmitted"], 0);
        assert_eq!(clean["emptyReason"], "cleanDiff");
    }

    #[test]
    fn endpoints_json_emits_locations_and_mount() {
        let nested = row_with(
            1,
            "GET",
            "/endpoints/changed",
            Some("endpoints_changed_handler"),
            "Axum",
            Some("src/commands/web/server/router.rs"),
            "BUILDER",
            Some("/api"),
            Some("get(/endpoints/changed) -> endpoints_changed_handler"),
            Some(9),
            None,
            None,
        );
        let v = endpoint_row_to_json(&nested);
        assert_eq!(v["registrationFile"], "src/commands/web/server/router.rs");
        assert_eq!(v["mountPrefix"], "/api");
        assert_eq!(v["mountedPath"], "/api/endpoints/changed");
        assert_eq!(v["mountProvenance"], "nest");
        assert!(v.get("handlerUnresolvedReason").is_none());

        let literal = row_with(
            2,
            "GET",
            "/health",
            Some("health_handler"),
            "Axum",
            Some("src/commands/web/server/router.rs"),
            "BUILDER",
            None,
            Some("get(/health) -> health_handler"),
            Some(8),
            None,
            None,
        );
        let v = endpoint_row_to_json(&literal);
        assert!(v.get("mountPrefix").is_none());
        assert_eq!(v["mountedPath"], "/health");
        assert_eq!(v["mountProvenance"], "literal");

        let go = row_with(
            3,
            "GET",
            "/items",
            Some("listUsers"),
            "gin",
            Some("pkg/handlers.go"),
            "METHOD_CALL",
            None,
            None,
            Some(7),
            None,
            None,
        );
        let v = endpoint_row_to_json(&go);
        assert!(v.get("mountedPath").is_none());
        assert_eq!(v["mountProvenance"], "unknown");
    }

    #[test]
    fn endpoints_unknown_handler_has_reason() {
        let closure = row_with(
            1,
            "GET",
            "/probe",
            Some("unknown"),
            "Axum",
            Some("src/startup.rs"),
            "TEST",
            None,
            Some("get(/probe) -> <closure>"),
            None,
            None,
            None,
        );
        assert_eq!(
            endpoint_row_to_json(&closure)["handlerUnresolvedReason"],
            "closure"
        );

        let missing = row_with(
            2,
            "GET",
            "/session",
            Some("session_handler"),
            "Axum",
            Some("src/router.rs"),
            "BUILDER",
            None,
            Some("get(/session) -> session_handler"),
            None,
            None,
            None,
        );
        assert_eq!(
            endpoint_row_to_json(&missing)["handlerUnresolvedReason"],
            "missing_symbol"
        );
    }

    #[test]
    fn endpoints_auth_parse_invalid_not_null() {
        let row = row_with(
            1,
            "GET",
            "/health",
            Some("health_handler"),
            "Axum",
            Some("src/router.rs"),
            "BUILDER",
            None,
            Some("get(/health) -> health_handler"),
            Some(1),
            Some("not-json"),
            Some("{"),
        );
        let v = endpoint_row_to_json(&row);
        assert_eq!(v["authParse"], "invalid");
        assert!(v.get("auth").is_none());
        assert_eq!(v["consumersParse"], "invalid");
        assert!(v.get("consumers").is_none());
        assert!(v.get("authSource").is_none());
    }
}
