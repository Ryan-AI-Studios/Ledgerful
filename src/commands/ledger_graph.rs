use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::graph_kinds::NodeKind;
use crate::state::storage::StorageManager;
use crate::state::storage_cozo::CozoStorage;
use clap::Args;
use cozo::{DataValue, ScriptMutability};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet, VecDeque};

#[derive(Args, Debug)]
pub struct LedgerGraphArgs {
    /// Transaction ID (or prefix)
    pub tx_id: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphRelation {
    pub entity_id: String,
    pub label: String,
    pub category: String,
    pub relation: String,
    pub exactness: String,          // "exact", "derived", "heuristic"
    pub attribution_source: String, // "token_provenance", "changed_files", "ledger_link", "knowledge_graph", "heuristic_fallback"
}

pub fn execute_ledger_graph(args: LedgerGraphArgs) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout.root)?;
    let cozo = storage
        .cozo
        .as_ref()
        .ok_or_else(|| miette::miette!("CozoDB not available"))?;

    let db = crate::ledger::db::LedgerDb::new(storage.get_connection());

    // Resolve prefix
    let full_id = resolve_tx_id(&db, &args.tx_id)?;
    let tx_opt = db
        .get_transaction(&full_id)
        .map_err(|e| miette::miette!("{}", e))?;

    let mut exact_relations = Vec::new();
    let mut derived_relations = Vec::new();
    let mut heuristic_relations = Vec::new();

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    // 1. Gather Token Provenance (Exact Symbols & Files)
    let token_prov = db
        .get_token_provenance_for_tx(&full_id)
        .map_err(|e| miette::miette!("{}", e))?;
    for prov in &token_prov {
        let file_urn = crate::platform::urn::build_urn(NodeKind::File, &prov.entity_normalized);
        if visited.insert(file_urn.clone()) {
            let exists = layout.root.join(&prov.entity_normalized).exists();
            let label = if exists {
                prov.entity_normalized.clone()
            } else {
                format!("[HISTORICAL] {}", prov.entity_normalized)
            };
            exact_relations.push(GraphRelation {
                entity_id: file_urn.clone(),
                label,
                category: "file".to_string(),
                relation: format!("affects ({})", prov.action.to_string().to_lowercase()),
                exactness: "exact".to_string(),
                attribution_source: "token_provenance".to_string(),
            });
            queue.push_back((file_urn, 0));
        }

        // Try to resolve qualified name from project_symbol table
        let symbol_urn = if let Some(qn) = resolve_qualified_name(
            cozo,
            &prov.entity_normalized,
            &prov.symbol_name,
            &prov.symbol_type,
        )? {
            crate::platform::urn::build_urn(NodeKind::Symbol, &qn)
        } else {
            format!(
                "urn:ledgerful:symbol:historical:{}::{}",
                prov.entity_normalized, prov.symbol_name
            )
        };

        if visited.insert(symbol_urn.clone()) {
            let is_historical = symbol_urn.contains(":historical:");
            let label = if is_historical {
                format!("[HISTORICAL] {}", prov.symbol_name)
            } else {
                prov.symbol_name.clone()
            };
            exact_relations.push(GraphRelation {
                entity_id: symbol_urn.clone(),
                label,
                category: "symbol".to_string(),
                relation: format!(
                    "{} ({})",
                    prov.action.to_string().to_lowercase(),
                    prov.symbol_type.to_lowercase()
                ),
                exactness: "exact".to_string(),
                attribution_source: "token_provenance".to_string(),
            });
            queue.push_back((symbol_urn, 0));
        }
    }

    // 2. Staged/Committed changed files (Exact Files)
    let changed_files = get_changed_files_from_db(storage.get_connection(), &full_id)?;
    for file_path in &changed_files {
        let file_urn = crate::platform::urn::build_urn(NodeKind::File, file_path);
        if visited.insert(file_urn.clone()) {
            let exists = layout.root.join(file_path).exists();
            let label = if exists {
                file_path.clone()
            } else {
                format!("[HISTORICAL] {}", file_path)
            };
            exact_relations.push(GraphRelation {
                entity_id: file_urn.clone(),
                label,
                category: "file".to_string(),
                relation: "modified".to_string(),
                exactness: "exact".to_string(),
                attribution_source: "changed_files".to_string(),
            });
            queue.push_back((file_urn, 0));
        }
    }

    // 3. Exact target entity ledger links
    if let Some(ref tx) = tx_opt
        && is_real_file_path(&tx.entity_normalized)
    {
        let is_synthetic = !tx.entity_normalized.contains('.')
            && !tx.entity_normalized.contains('/')
            && !tx.entity_normalized.contains('\\')
            && !layout.root.join(&tx.entity_normalized).exists();

        if !is_synthetic {
            let file_urn = crate::platform::urn::build_urn(NodeKind::File, &tx.entity_normalized);
            if visited.insert(file_urn.clone()) {
                let exists = layout.root.join(&tx.entity_normalized).exists();
                let label = if exists {
                    tx.entity_normalized.clone()
                } else {
                    format!("[HISTORICAL] {}", tx.entity_normalized)
                };
                exact_relations.push(GraphRelation {
                    entity_id: file_urn.clone(),
                    label,
                    category: "file".to_string(),
                    relation: "affects".to_string(),
                    exactness: "exact".to_string(),
                    attribution_source: "ledger_link".to_string(),
                });
                queue.push_back((file_urn, 0));
            }
        }
    }

    // 3.2. Link from transaction_links
    let stmt = storage.get_connection().prepare(
        "SELECT entity_normalized FROM transaction_links WHERE tx_id = ?1 AND entity_type = 'FILE'",
    );
    if let Ok(mut stmt) = stmt
        && let Ok(mut rows) = stmt.query([&full_id])
    {
        while let Ok(Some(row)) = rows.next() {
            if let Ok(file_path) = row.get::<_, String>(0)
                && is_real_file_path(&file_path)
            {
                let file_urn = crate::platform::urn::build_urn(NodeKind::File, &file_path);
                if visited.insert(file_urn.clone()) {
                    let exists = layout.root.join(&file_path).exists();
                    let label = if exists {
                        file_path.clone()
                    } else {
                        format!("[HISTORICAL] {}", file_path)
                    };
                    exact_relations.push(GraphRelation {
                        entity_id: file_urn.clone(),
                        label,
                        category: "file".to_string(),
                        relation: "linked".to_string(),
                        exactness: "exact".to_string(),
                        attribution_source: "ledger_link".to_string(),
                    });
                    queue.push_back((file_urn, 0));
                }
            }
        }
    }

    // 3.5. Knowledge Graph committed transaction edges
    let tx_urn = crate::platform::urn::build_urn(NodeKind::LedgerTransaction, &full_id);
    if let Ok(edges) = query_outgoing_edges(cozo, &tx_urn, &layout.root) {
        for (target_urn, label, category, relation) in edges {
            if visited.insert(target_urn.clone()) {
                exact_relations.push(GraphRelation {
                    entity_id: target_urn.clone(),
                    label,
                    category,
                    relation,
                    exactness: "exact".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                queue.push_back((target_urn, 0));
            }
        }
    }

    // 4. Degraded legacy fallbacks (if set is empty)
    if exact_relations.is_empty()
        && let Some(ref tx) = tx_opt
    {
        let derived = get_legacy_fallback_edges(tx);
        for (id, label, cat, rel) in derived {
            if visited.insert(id.clone()) {
                heuristic_relations.push(GraphRelation {
                    entity_id: id.clone(),
                    label,
                    category: cat,
                    relation: rel,
                    exactness: "heuristic".to_string(),
                    attribution_source: "heuristic_fallback".to_string(),
                });
                queue.push_back((id, 0));
            }
        }
    }

    // BFS Neighborhood Traversal (Derived Relationships)
    let max_depth = 2;
    let max_nodes = 150;

    while let Some((curr_urn, depth)) = queue.pop_front() {
        if depth >= max_depth
            || (exact_relations.len() + derived_relations.len() + heuristic_relations.len())
                >= max_nodes
        {
            continue;
        }

        let outgoing = query_outgoing_edges(cozo, &curr_urn, &layout.root)?;
        let incoming = query_incoming_edges(cozo, &curr_urn, &layout.root)?;

        for (target_urn, target_label, target_category, relation) in outgoing {
            if (exact_relations.len() + derived_relations.len() + heuristic_relations.len())
                >= max_nodes
            {
                break;
            }
            let target_cat_lower = target_category.to_lowercase();
            if target_cat_lower == "ledger_transaction"
                || target_cat_lower == "transaction"
                || target_cat_lower == "adr"
            {
                continue;
            }
            if visited.insert(target_urn.clone()) {
                derived_relations.push(GraphRelation {
                    entity_id: target_urn.clone(),
                    label: target_label,
                    category: target_category,
                    relation: format!("{} (derived)", relation),
                    exactness: "derived".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                if depth + 1 < max_depth {
                    queue.push_back((target_urn, depth + 1));
                }
            }
        }

        for (source_urn, source_label, source_category, relation) in incoming {
            if (exact_relations.len() + derived_relations.len() + heuristic_relations.len())
                >= max_nodes
            {
                break;
            }
            let source_cat_lower = source_category.to_lowercase();
            if source_cat_lower == "ledger_transaction"
                || source_cat_lower == "transaction"
                || source_cat_lower == "adr"
            {
                continue;
            }
            if visited.insert(source_urn.clone()) {
                derived_relations.push(GraphRelation {
                    entity_id: source_urn.clone(),
                    label: source_label,
                    category: source_category,
                    relation: format!("inv_{} (derived)", relation),
                    exactness: "derived".to_string(),
                    attribution_source: "knowledge_graph".to_string(),
                });
                if depth + 1 < max_depth {
                    queue.push_back((source_urn, depth + 1));
                }
            }
        }
    }

    exact_relations.sort();
    derived_relations.sort();
    heuristic_relations.sort();

    if args.json {
        let output = serde_json::json!({
            "exact": exact_relations,
            "derived": derived_relations,
            "heuristic": heuristic_relations,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        println!(
            "{} {}",
            "Graph neighborhood for transaction:".bold(),
            full_id.cyan()
        );

        println!("\n{}", "Exact Relations".green().bold());
        if exact_relations.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &exact_relations {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }

        println!(
            "\n{}",
            "Derived Relations (Transitive / Structural Neighborhood)"
                .yellow()
                .bold()
        );
        if derived_relations.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &derived_relations {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }

        println!("\n{}", "Heuristic Fallbacks".red().bold());
        if heuristic_relations.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &heuristic_relations {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }
    }

    Ok(())
}

fn resolve_qualified_name(
    cozo: &CozoStorage,
    file_path: &str,
    symbol_name: &str,
    symbol_type: &str,
) -> Result<Option<String>> {
    let query = "?[qn] := *project_symbol{file_path: $fp, symbol_name: $sn, symbol_kind: $sk, qualified_name: qn}";
    let mut params = BTreeMap::new();
    params.insert("fp".to_string(), DataValue::Str(file_path.into()));
    params.insert("sn".to_string(), DataValue::Str(symbol_name.into()));
    params.insert("sk".to_string(), DataValue::Str(symbol_type.into()));
    let res = cozo.run_script_with_params(query, params, ScriptMutability::Immutable)?;

    let mut qns: Vec<String> = res
        .rows
        .into_iter()
        .filter_map(|row| {
            if let Some(DataValue::Str(qn)) = row.first() {
                Some(qn.to_string())
            } else {
                None
            }
        })
        .collect();
    qns.sort();
    Ok(qns.into_iter().next())
}

fn get_changed_files_from_db(conn: &rusqlite::Connection, tx_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM changed_files WHERE snapshot_id = (SELECT snapshot_id FROM transactions WHERE tx_id = ?1)"
    ).into_diagnostic()?;
    let rows = stmt
        .query_map([tx_id], |row| row.get::<_, String>(0))
        .into_diagnostic()?;
    let mut files = Vec::new();
    for r in rows {
        let f = r.into_diagnostic()?;
        if is_real_file_path(&f) {
            files.push(f);
        }
    }
    Ok(files)
}

fn is_real_file_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.contains("drift_adoption:") {
        return false;
    }
    if uuid::Uuid::parse_str(s).is_ok() {
        return false;
    }
    if !s.contains('.') && !s.contains('/') && !s.contains('\\') && s.len() > 20 {
        return false;
    }
    true
}

fn get_legacy_fallback_edges(
    tx: &crate::ledger::Transaction,
) -> Vec<(String, String, String, String)> {
    let mut derived = Vec::new();
    if tx.entity_normalized.contains('/')
        || tx.entity_normalized.contains('\\')
        || tx.entity_normalized.contains('.')
    {
        let file_urn = crate::platform::urn::build_urn(NodeKind::File, &tx.entity_normalized);
        derived.push((
            file_urn,
            tx.entity_normalized.clone(),
            "file".to_string(),
            "affects".to_string(),
        ));
    }

    if let Some(ref ticket) = tx.issue_ref {
        derived.push((
            format!("urn:ledgerful:ticket:{}", ticket),
            ticket.clone(),
            "ticket".to_string(),
            "resolves".to_string(),
        ));
    }
    derived
}

fn query_outgoing_edges(
    cozo: &CozoStorage,
    source_urn: &str,
    root_path: &camino::Utf8Path,
) -> Result<Vec<(String, String, String, String)>> {
    let query = "?[target, relation] := *edge{source: $src, target: target, relation: relation}";
    let mut params = BTreeMap::new();
    params.insert("src".to_string(), DataValue::Str(source_urn.into()));
    let res = cozo.run_script_with_params(query, params, ScriptMutability::Immutable)?;

    let mut results = Vec::new();
    for row in res.rows {
        if let (Some(DataValue::Str(target_urn)), Some(DataValue::Str(relation))) =
            (row.first(), row.get(1))
        {
            let (label, category) = get_node_details(cozo, target_urn.as_ref(), root_path)?;
            results.push((
                target_urn.to_string(),
                label,
                category,
                relation.to_string(),
            ));
        }
    }
    results.sort_by(|a, b| (&a.0, &a.3).cmp(&(&b.0, &b.3)));
    Ok(results)
}

fn query_incoming_edges(
    cozo: &CozoStorage,
    target_urn: &str,
    root_path: &camino::Utf8Path,
) -> Result<Vec<(String, String, String, String)>> {
    let query = "?[source, relation] := *edge{source: source, target: $tgt, relation: relation}";
    let mut params = BTreeMap::new();
    params.insert("tgt".to_string(), DataValue::Str(target_urn.into()));
    let res = cozo.run_script_with_params(query, params, ScriptMutability::Immutable)?;

    let mut results = Vec::new();
    for row in res.rows {
        if let (Some(DataValue::Str(source_urn)), Some(DataValue::Str(relation))) =
            (row.first(), row.get(1))
        {
            let (label, category) = get_node_details(cozo, source_urn.as_ref(), root_path)?;
            results.push((
                source_urn.to_string(),
                label,
                category,
                relation.to_string(),
            ));
        }
    }
    results.sort_by(|a, b| (&a.0, &a.3).cmp(&(&b.0, &b.3)));
    Ok(results)
}

fn get_node_details(
    cozo: &CozoStorage,
    urn: &str,
    root_path: &camino::Utf8Path,
) -> Result<(String, String)> {
    let query = "?[label, category] := *node{id: $urn, label: label, category: category}";
    let mut params = BTreeMap::new();
    params.insert("urn".to_string(), DataValue::Str(urn.into()));
    let res = cozo.run_script_with_params(query, params, ScriptMutability::Immutable)?;

    let (label, category) = if let Some(row) = res.rows.first()
        && let (Some(DataValue::Str(label)), Some(DataValue::Str(category))) =
            (row.first(), row.get(1))
    {
        (label.to_string(), category.to_string())
    } else {
        parse_urn(urn, root_path)
    };

    if category == "file" && !label.starts_with("[HISTORICAL]") {
        let path_str = label.trim_start_matches("[HISTORICAL] ").trim();
        if !root_path.join(path_str).exists() {
            return Ok((format!("[HISTORICAL] {}", path_str), category));
        }
    }

    Ok((label, category))
}

fn parse_urn(urn: &str, root_path: &camino::Utf8Path) -> (String, String) {
    if urn.starts_with("urn:ledgerful:") {
        let parts: Vec<&str> = urn.split(':').collect();
        if parts.len() >= 4 && parts[2] == "symbol" && parts[3] == "historical" {
            let identifier = parts[4..].join(":");
            if let Some(pos) = identifier.find("::") {
                let symbol_name = &identifier[pos + 2..];
                return (
                    format!("[HISTORICAL] {}", symbol_name),
                    "symbol".to_string(),
                );
            }
            return (format!("[HISTORICAL] {}", identifier), "symbol".to_string());
        }
        if parts.len() >= 3 {
            let kind = parts[2];
            let identifier = parts[3..].join(":");
            let label = if kind == "file" {
                let file_exists = root_path.join(&identifier).exists();
                if file_exists {
                    identifier
                } else {
                    format!("[HISTORICAL] {}", identifier)
                }
            } else {
                identifier
            };
            return (label, kind.to_string());
        }
    }
    (urn.to_string(), "unknown".to_string())
}

fn resolve_tx_id(db: &crate::ledger::db::LedgerDb, tx_id_or_prefix: &str) -> Result<String> {
    // 1. Exact full UUID match
    if tx_id_or_prefix.len() == 36
        && db
            .get_transaction(tx_id_or_prefix)
            .map_err(|e| miette::miette!("{}", e))?
            .is_some()
    {
        return Ok(tx_id_or_prefix.to_string());
    }

    // 2. UUID prefix match
    let uuid_matches = db
        .resolve_tx_id_fuzzy(tx_id_or_prefix)
        .map_err(|e| miette::miette!("{}", e))?;
    if uuid_matches.len() == 1 {
        return Ok(uuid_matches[0].clone());
    }
    if uuid_matches.len() > 1 {
        return Err(miette::miette!(
            "Ambiguous transaction ID prefix '{}': matched {}",
            tx_id_or_prefix,
            uuid_matches.join(", ")
        ));
    }

    // 3. Entity / basename fuzzy match against PENDING transactions
    let needle = tx_id_or_prefix.to_lowercase();
    let needle_base = std::path::Path::new(tx_id_or_prefix)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(tx_id_or_prefix)
        .to_lowercase();

    let pending = db.get_all_pending().map_err(|e| miette::miette!("{}", e))?;
    let entity_matches: Vec<String> = pending
        .into_iter()
        .filter(|tx| {
            let entity_lower = tx.entity.to_lowercase();
            let norm_lower = tx.entity_normalized.to_lowercase();
            let entity_base = std::path::Path::new(&tx.entity_normalized)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&tx.entity_normalized)
                .to_lowercase();

            entity_lower.contains(&needle)
                || norm_lower.contains(&needle)
                || entity_base == needle_base
        })
        .map(|tx| tx.tx_id)
        .collect();

    match entity_matches.len() {
        0 => Err(miette::miette!(
            "Transaction not found: {}",
            tx_id_or_prefix
        )),
        1 => Ok(entity_matches[0].clone()),
        _ => Err(miette::miette!(
            "Ambiguous entity lookup '{}': matched {} pending transactions. Use the transaction ID prefix instead.",
            tx_id_or_prefix,
            entity_matches.len()
        )),
    }
}
