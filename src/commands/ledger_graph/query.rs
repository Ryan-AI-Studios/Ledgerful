use super::assemble::parse_urn;
use crate::state::graph_kinds::NodeKind;
use crate::state::storage_cozo::CozoStorage;
use cozo::{DataValue, ScriptMutability};
use miette::{IntoDiagnostic, Result};
use std::collections::BTreeMap;

pub(super) fn resolve_qualified_name(
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

pub(super) fn get_changed_files_from_db(
    conn: &rusqlite::Connection,
    tx_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT path FROM changed_files WHERE snapshot_id = (SELECT snapshot_id FROM transactions WHERE tx_id = ?1)",
        )
        .into_diagnostic()?;
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

pub(super) fn is_real_file_path(s: &str) -> bool {
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

pub(super) fn get_legacy_fallback_edges(
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

pub(super) fn query_outgoing_edges(
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

pub(super) fn query_incoming_edges(
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

pub(super) fn get_node_details(
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

pub(super) fn resolve_tx_id(
    db: &crate::ledger::db::LedgerDb,
    tx_id_or_prefix: &str,
) -> Result<String> {
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
