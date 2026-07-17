use cozo::*;
use miette::Result;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::state::cozo::queries::*;
use crate::state::graph_kinds::{EdgeKind, NodeKind};
use crate::state::storage_cozo::{CozoStorage, GraphEdge, GraphNode};
use serde_json::json;

pub mod setup_schema {
    /// Controls which optional schema objects are provisioned by
    /// [`super::setup_schema`].
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Options {
        /// Create the vestigial bridge/AI relations (`Turn`, `Session`,
        /// `Memory`, `Decision`) when true. These relations are gated by
        /// `bridge.enabled` so a disabled bridge never touches them.
        pub include_bridge_tables: bool,
    }
}

pub fn setup_schema(storage: &CozoStorage, options: setup_schema::Options) -> Result<()> {
    let existing = storage.get_relations()?;

    if !existing.contains(&"node".to_string()) {
        storage.run_script(CREATE_NODE_TABLE)?;
    }
    if !existing.contains(&"edge".to_string()) {
        storage.run_script(CREATE_EDGE_TABLE)?;
    }
    if !existing.contains(&"ledger_link".to_string()) {
        storage.run_script(CREATE_LEDGER_LINK_TABLE)?;
    }
    if !existing.contains(&"project_symbol".to_string()) {
        storage.run_script(CREATE_PROJECT_SYMBOL_TABLE)?;
    }

    // FTS Index
    if !existing.contains(&"node:fts_idx".to_string()) {
        storage.run_script(CREATE_FTS_INDEX)?;
    }

    // Bridge relations — gated by `bridge.enabled` to keep the default
    // install free of vestigial AI/bridge schema.
    if options.include_bridge_tables {
        if !existing.contains(&"Turn".to_string()) {
            storage.run_script(CREATE_TURN_TABLE)?;
        }
        if !existing.contains(&"Session".to_string()) {
            storage.run_script(CREATE_SESSION_TABLE)?;
        }
        if !existing.contains(&"Memory".to_string()) {
            storage.run_script(CREATE_MEMORY_TABLE)?;
        }
        if !existing.contains(&"Decision".to_string()) {
            storage.run_script(CREATE_DECISION_TABLE)?;
        }
    }

    // Metadata & Migration
    if !existing.contains(&"cozo_meta".to_string()) {
        storage.run_script(":create cozo_meta { key: String => value: String }")?;
        storage.run_script("?[key, value] <- [[\"cozo_schema_version\", \"2\"]] :put cozo_meta")?;
    }

    // Now ensure ledger_entry exists
    let existing = storage.get_relations()?;
    if !existing.contains(&"ledger_entry".to_string()) {
        storage.run_script(CREATE_LEDGER_ENTRY_TABLE)?;
    }

    // K4: Service boundary and communication relations
    let existing = storage.get_relations()?;
    if !existing.contains(&"service_roots".to_string()) {
        storage.run_script(CREATE_SERVICE_ROOTS_TABLE)?;
    }
    if !existing.contains(&"service_dependencies".to_string()) {
        storage.run_script(CREATE_SERVICE_DEPENDENCIES_TABLE)?;
    }

    migrate_cozo_schema(storage)?;

    Ok(())
}

pub fn migrate_cozo_schema(storage: &CozoStorage) -> Result<()> {
    let res =
        storage.run_script("?[value] := *cozo_meta{key: \"cozo_schema_version\", value: value}")?;
    let version = res
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| {
            if let DataValue::Str(s) = v {
                s.parse::<u32>().ok()
            } else {
                None
            }
        })
        .unwrap_or(1);

    if version < 2 {
        info!("[migrate] Upgrading CozoDB schema from v1 to v2");

        // 1. Check if ledger_entry exists before trying to migrate data
        let existing = storage.get_relations()?;
        if existing.contains(&"ledger_entry".to_string()) {
            let current_entries = match storage.run_script("?[id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id] := *ledger_entry{id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id}") {
                 Ok(res) => res,
                 Err(e) => {
                     warn!("Failed to query old ledger_entry for migration: {}. Skipping data migration.", e);
                     NamedRows { headers: Vec::new(), rows: Vec::new(), next: None }
                 }
             };

            storage.run_script("::remove ledger_entry")?;
            storage.run_script(CREATE_LEDGER_ENTRY_TABLE)?;

            if !current_entries.rows.is_empty() {
                let mut script = String::from(
                    "?[id, tx_id, category, entry_type, entity_normalized, change_type, summary, reason, committed_at, is_breaking, verification_status, trace_id, signature, public_key, risk, related_tickets] <- [\n",
                );
                for (i, row) in current_entries.rows.iter().enumerate() {
                    let mut row_vals = Vec::new();
                    for val in row {
                        match val {
                            DataValue::Str(s) => {
                                let escaped = serde_json::to_string(s.as_str())
                                    .unwrap_or_else(|_| "\"\"".to_string());
                                row_vals.push(escaped);
                            }
                            DataValue::Num(Num::Int(n)) => row_vals.push(n.to_string()),
                            DataValue::Bool(b) => row_vals.push(b.to_string()),
                            _ => row_vals.push("null".to_string()),
                        }
                    }

                    // Append empty strings for new signature and public_key,
                    // 'Low' for risk, and '[]' for related_tickets
                    script.push_str(&format!(
                        "  [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, \"\", \"\", \"Low\", \"[]\"]{}\n",
                        row_vals[0], row_vals[1], row_vals[2], row_vals[3], row_vals[4], row_vals[5],
                        row_vals[6], row_vals[7], row_vals[8], row_vals[9], row_vals[10], row_vals[11],
                        if i == current_entries.rows.len() - 1 { "" } else { "," }
                    ));
                }

                script.push_str("] :put ledger_entry");
                storage.run_script(&script)?;
            }
        } else {
            // Just create it if it didn't exist
            storage.run_script(CREATE_LEDGER_ENTRY_TABLE)?;
        }

        storage.run_script("?[key, value] <- [[\"cozo_schema_version\", \"2\"]] :put cozo_meta")?;
    }

    if version < 3 {
        info!("[migrate] Upgrading CozoDB schema from v2 to v3 (URN IDs and typed kinds)");

        // Migrate 'node' table
        let nodes = storage.run_script("?[id, label, category, risk_score, metadata] := *node{id, label, category, risk_score, metadata}")?;
        if !nodes.rows.is_empty() {
            let mut new_nodes = Vec::new();
            let mut old_ids = Vec::new();
            for row in nodes.rows {
                if let (
                    Some(DataValue::Str(id)),
                    Some(DataValue::Str(label)),
                    Some(DataValue::Str(cat)),
                    Some(risk_val),
                    Some(meta_val),
                ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
                {
                    // Convert old category to NodeKind
                    let kind = match cat.as_str() {
                        "file" => NodeKind::File,
                        "symbol" | "code" => NodeKind::Symbol,
                        _ => NodeKind::File, // Default
                    };

                    // Build new URN ID
                    let new_id = crate::platform::urn::build_urn(kind, id);

                    // Update metadata with schema_version
                    let mut metadata: serde_json::Value = if let DataValue::Json(j) = meta_val {
                        serde_json::to_value(j).unwrap_or(json!({}))
                    } else {
                        json!({})
                    };
                    if let Some(obj) = metadata.as_object_mut() {
                        obj.insert("schema_version".to_string(), json!("v1"));
                    }

                    let risk_score = match risk_val {
                        DataValue::Num(Num::Float(f)) => *f,
                        DataValue::Num(Num::Int(i)) => *i as f64,
                        _ => 0.0,
                    };

                    new_nodes.push(GraphNode {
                        id: new_id,
                        label: label.to_string(),
                        category: kind,
                        risk_score,
                        metadata: Some(metadata),
                    });
                    old_ids.push(id.to_string());
                }
            }

            // Put new nodes
            storage.insert_nodes(&new_nodes)?;

            // Remove old nodes (only if they are different from new IDs)
            storage.remove_nodes_by_id(&old_ids)?;
        }

        // Migrate 'edge' table
        let edges = storage.run_script("?[source, target, relation, confidence, provenance_id] := *edge{source, target, relation, confidence, provenance_id}")?;
        if !edges.rows.is_empty() {
            let mut new_edges = Vec::new();
            let mut old_edge_triples = Vec::new();
            for row in edges.rows {
                if let (
                    Some(DataValue::Str(src)),
                    Some(DataValue::Str(tgt)),
                    Some(DataValue::Str(rel)),
                    Some(conf_val),
                    Some(prov_val),
                ) = (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
                {
                    // Guess kinds
                    let src_kind = if rel == "calls" || rel == "call" {
                        NodeKind::Symbol
                    } else {
                        NodeKind::File
                    };
                    let tgt_kind = if rel == "calls" || rel == "call" {
                        NodeKind::Symbol
                    } else {
                        NodeKind::File
                    };

                    let new_src = crate::platform::urn::build_urn(src_kind, src);
                    let new_tgt = crate::platform::urn::build_urn(tgt_kind, tgt);

                    let new_rel = match rel.as_str() {
                        "calls" | "call" => EdgeKind::Calls,
                        _ => EdgeKind::DependsOn,
                    };

                    let confidence = match conf_val {
                        DataValue::Num(Num::Float(f)) => *f,
                        DataValue::Num(Num::Int(i)) => *i as f64,
                        _ => 1.0,
                    };

                    new_edges.push(GraphEdge {
                        source: new_src,
                        target: new_tgt,
                        relation: new_rel,
                        confidence,
                        provenance_id: prov_val.to_string(),
                    });
                    old_edge_triples.push((src.to_string(), tgt.to_string(), rel.to_string()));
                }
            }

            // Put new edges
            storage.insert_edges(&new_edges)?;

            // Remove old edges
            if !old_edge_triples.is_empty() {
                for chunk in old_edge_triples.chunks(100) {
                    let mut params = std::collections::BTreeMap::new();
                    let batch: Vec<DataValue> = chunk
                        .iter()
                        .map(|(s, t, r)| {
                            DataValue::List(Box::new(vec![
                                DataValue::Str(s.clone().into()),
                                DataValue::Str(t.clone().into()),
                                DataValue::Str(r.clone().into()),
                            ]))
                        })
                        .collect();
                    params.insert("batch".to_string(), DataValue::List(Box::new(batch)));
                    let script = "old_edges[source, target, relation] <- $batch\n?[source, target, relation] := old_edges[source, target, relation], *edge{source, target, relation}\n:rm edge {source, target, relation}";
                    storage.run_script_with_params(script, params, ScriptMutability::Mutable)?;
                }
            }
        }

        storage.run_script("?[key, value] <- [[\"cozo_schema_version\", \"3\"]] :put cozo_meta")?;
    }

    Ok(())
}

pub fn initialize_instance(db_path: &Path, read_only: bool) -> Result<DbInstance> {
    let engine = if db_path.as_os_str().is_empty() {
        "mem"
    } else {
        "sqlite"
    };
    debug!(
        "CozoStorage selecting engine '{}' for path {:?} (read_only: {})",
        engine, db_path, read_only
    );

    let is_new = engine == "sqlite" && !db_path.exists();

    // Bounded Retry Loop for Locks
    let mut retries = 0;
    let max_retries = if read_only { 3 } else { 5 };
    let base_delay_ms = 100;

    let db = loop {
        match DbInstance::new(engine, db_path, Default::default()) {
            Ok(db) => break db,
            Err(e) if engine == "sqlite" && retries < max_retries => {
                let err_debug = format!("{:?}", e).to_lowercase();
                if err_debug.contains("lock")
                    || err_debug.contains("access is denied")
                    || err_debug.contains("os error 33")
                    || err_debug.contains("unable to open database file")
                {
                    retries += 1;
                    if retries == 3 {
                        // Attempt to clean locks / remove lock files in Cozo directory
                        warn!(
                            "CozoDB locked or blocked. Attempting to clear lock files in directory {:?}",
                            db_path
                        );
                        if let Some(parent) = db_path.parent() {
                            let lock_file = parent.join("ledger.cozo").join("lock");
                            let _ = std::fs::remove_file(lock_file);
                        }
                    }
                    let delay = base_delay_ms * (2u64.pow(retries - 1));
                    debug!("CozoDB is locked, retrying in {}ms", delay);
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    continue;
                }
                return Err(miette::miette!("Failed to initialize CozoDB: {:?}", e));
            }
            Err(e) => return Err(miette::miette!("Failed to initialize CozoDB: {:?}", e)),
        }
    };

    // Cold Start Verification
    if engine == "sqlite"
        && !is_new
        && let Err(e) = db.run_script(
            GET_RELATIONS,
            Default::default(),
            ScriptMutability::Immutable,
        )
    {
        return Err(miette::miette!(
            "CozoDB Cold Start Verification failed: {}",
            e
        ));
    }

    Ok(db)
}
