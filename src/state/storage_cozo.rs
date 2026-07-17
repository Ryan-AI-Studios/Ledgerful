use cozo::*;
use miette::Result;
use serde_json::json;
use std::path::Path;
use tracing::debug;

use crate::state::cozo::{init, queries};

use crate::state::graph_kinds::{EdgeKind, NodeKind};

#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub category: NodeKind,
    pub risk_score: f64,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub relation: EdgeKind,
    pub confidence: f64,
    pub provenance_id: String,
}

pub struct CozoStorage {
    db: DbInstance,
}

impl CozoStorage {
    pub fn new(db_path: &Path) -> Result<Self> {
        Self::new_with_options(db_path, false)
    }

    pub fn new_read_only(db_path: &Path) -> Result<Self> {
        Self::new_with_options(db_path, true)
    }

    pub fn new_in_memory() -> Result<Self> {
        Self::new_with_options(Path::new(""), false)
    }

    /// Create an in-memory storage without running schema setup.
    /// The caller is responsible for calling `setup_schema_with_options`.
    pub fn new_in_memory_bare() -> Result<Self> {
        let db = init::initialize_instance(Path::new(""), false)?;
        Ok(Self { db })
    }

    fn new_with_options(db_path: &Path, read_only: bool) -> Result<Self> {
        let db = init::initialize_instance(db_path, read_only)?;
        let storage = Self { db };
        if !read_only {
            let include_bridge_tables = crate::bridge::client::is_bridge_enabled_or_default();
            storage.setup_schema_with_options(init::setup_schema::Options {
                include_bridge_tables,
            })?;
        }
        Ok(storage)
    }

    pub fn run_script(&self, script: &str) -> Result<NamedRows> {
        self.db
            .run_script(script, Default::default(), ScriptMutability::Mutable)
            .map_err(|e| miette::miette!("CozoDB script error: {:?}. Script was: '{}'", e, script))
    }

    pub fn run_script_with_params(
        &self,
        script: &str,
        params: std::collections::BTreeMap<String, DataValue>,
        mutability: ScriptMutability,
    ) -> Result<NamedRows> {
        self.db
            .run_script(script, params, mutability)
            .map_err(|e| miette::miette!("CozoDB script error: {:?}", e))
    }

    pub fn query_nodes_by_category(&self, category: &str) -> Result<NamedRows> {
        let mut params = std::collections::BTreeMap::new();
        params.insert("cat".into(), DataValue::Str(category.into()));
        self.run_script_with_params(
            "?[id, label] := *node{id, label, category: $cat}",
            params,
            ScriptMutability::Immutable,
        )
    }

    pub fn query_edges_by_source(&self, source: &str, relation: &str) -> Result<NamedRows> {
        let mut params = std::collections::BTreeMap::new();
        params.insert("src".into(), DataValue::Str(source.into()));
        params.insert("rel".into(), DataValue::Str(relation.into()));
        self.run_script_with_params(
            "?[source, target, relation] := *edge{source, target, relation}, source = $src, relation = $rel",
            params,
            ScriptMutability::Immutable,
        )
    }

    pub fn query_edges_by_target(&self, target: &str, relation: &str) -> Result<NamedRows> {
        let mut params = std::collections::BTreeMap::new();
        params.insert("tgt".into(), DataValue::Str(target.into()));
        params.insert("rel".into(), DataValue::Str(relation.into()));
        self.run_script_with_params(
            "?[source, target, relation] := *edge{source, target, relation}, target = $tgt, relation = $rel",
            params,
            ScriptMutability::Immutable,
        )
    }

    pub fn shutdown(self) {
        debug!("Shutting down CozoDB instance");
        drop(self.db);
    }

    pub fn setup_schema(&self) -> Result<()> {
        self.setup_schema_with_options(init::setup_schema::Options::default())
    }

    pub fn setup_schema_with_options(&self, options: init::setup_schema::Options) -> Result<()> {
        init::setup_schema(self, options)
    }

    pub fn migrate_cozo_schema(&self) -> Result<()> {
        init::migrate_cozo_schema(self)
    }

    pub fn get_relations(&self) -> Result<Vec<String>> {
        let res = self.run_script("::relations")?;
        let mut relations = Vec::new();
        for row in res.rows {
            if let Some(DataValue::Str(name)) = row.first() {
                relations.push(name.to_string());
            }
        }
        Ok(relations)
    }

    pub fn get_indices(&self, relation: &str) -> Result<Vec<String>> {
        let script = format!("::indices {}", relation);
        let res = self.run_script(&script)?;
        let mut indices = Vec::new();
        for row in res.rows {
            if let Some(DataValue::Str(name)) = row.first() {
                indices.push(name.to_string());
            }
        }
        Ok(indices)
    }

    pub fn verify_embedding_dimension(&self, relation: &str, expected_dim: usize) -> Result<()> {
        let script = format!("::columns {}", relation);
        let res = self.run_script(&script)?;
        for row in res.rows {
            if let (Some(DataValue::Str(name)), Some(DataValue::Str(typ))) =
                (row.first(), row.get(1))
                && name == "embedding"
            {
                let expected = format!("<F32; {}>", expected_dim);
                if typ != &expected {
                    return Err(miette::miette!(
                        "Dimension mismatch for relation '{}': expected {}, found {}.",
                        relation,
                        expected,
                        typ
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn node_count(&self) -> Result<usize> {
        let res = self.run_script(queries::node_count_query())?;
        if let Some(row) = res.rows.first()
            && let Some(DataValue::Num(Num::Int(count))) = row.first()
        {
            return Ok(*count as usize);
        }
        Ok(0)
    }

    pub fn edge_count(&self) -> Result<usize> {
        let res = self.run_script(queries::edge_count_query())?;
        if let Some(row) = res.rows.first()
            && let Some(DataValue::Num(Num::Int(count))) = row.first()
        {
            return Ok(*count as usize);
        }
        Ok(0)
    }

    pub fn put_node_batch(&self, nodes: &[GraphNode]) -> Result<()> {
        self.insert_nodes(nodes)
    }

    pub fn put_edge_batch(&self, edges: &[GraphEdge]) -> Result<()> {
        self.insert_edges(edges)
    }

    pub fn remove_nodes_by_id(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        for chunk in ids.chunks(200) {
            let batch: Vec<Vec<String>> = chunk.iter().map(|id| vec![id.clone()]).collect();
            let script = "?[id] <- $batch :rm node {id}";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(
                    batch
                        .into_iter()
                        .map(|v| {
                            serde_json::Value::Array(
                                v.into_iter().map(serde_json::Value::String).collect(),
                            )
                        })
                        .collect(),
                )),
            );
            self.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }
        Ok(())
    }

    pub fn remove_edges_for_source(&self, source_ids: &[String]) -> Result<()> {
        if source_ids.is_empty() {
            return Ok(());
        }
        for chunk in source_ids.chunks(200) {
            let batch: Vec<Vec<String>> = chunk.iter().map(|id| vec![id.clone()]).collect();
            let script = "source_input[source] <- $batch\n?[source, target, relation] := source_input[source], *edge{source, target, relation}\n:rm edge {source, target, relation}";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(
                    batch
                        .into_iter()
                        .map(|v| {
                            serde_json::Value::Array(
                                v.into_iter().map(serde_json::Value::String).collect(),
                            )
                        })
                        .collect(),
                )),
            );
            self.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }
        Ok(())
    }

    /// Remove all `snippet_embedding` rows for the given list of file paths.
    ///
    /// Called during incremental re-indexing to prune stale embeddings before
    /// inserting updated ones, ensuring consistency between the KG and the
    /// vector store.
    pub fn remove_snippets_for_files(&self, file_paths: &[String]) -> Result<()> {
        if file_paths.is_empty() {
            return Ok(());
        }
        // Check whether the relation exists to avoid errors on fresh repos.
        let relations = self.get_relations()?;
        if !relations.contains(&"snippet_embedding".to_string()) {
            return Ok(());
        }

        let mut list_vals = Vec::new();
        for fp in file_paths {
            list_vals.push(DataValue::Str(fp.clone().into()));
        }

        let mut params = std::collections::BTreeMap::new();
        params.insert("paths".into(), DataValue::List(Box::new(list_vals)));

        let script = "?[file_path, name, line_offset] := *snippet_embedding{file_path, name, line_offset}, file_path in $paths\n:rm snippet_embedding {file_path, name, line_offset}";
        self.run_script_with_params(script, params, ScriptMutability::Mutable)?;
        Ok(())
    }

    pub fn insert_nodes(&self, nodes: &[GraphNode]) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        for chunk in nodes.chunks(200) {
            let mut node_batch = Vec::new();
            for node in chunk {
                let metadata = node.metadata.as_ref().cloned().unwrap_or(json!({}));
                node_batch.push(json!([
                    node.id,
                    node.label,
                    node.category.to_string(),
                    node.risk_score,
                    metadata
                ]));
            }
            let script = "?[id, label, category, risk_score, metadata] <- $batch :put node";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(node_batch)),
            );
            self.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }
        Ok(())
    }

    pub fn insert_edges(&self, edges: &[GraphEdge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        for chunk in edges.chunks(200) {
            let mut edge_batch = Vec::new();
            for edge in chunk {
                edge_batch.push(json!([
                    edge.source,
                    edge.target,
                    edge.relation.to_string(),
                    edge.confidence,
                    edge.provenance_id,
                ]));
            }
            let script =
                "?[source, target, relation, confidence, provenance_id] <- $batch :put edge";
            let mut params = std::collections::BTreeMap::new();
            params.insert(
                "batch".to_string(),
                cozo::DataValue::from(serde_json::Value::Array(edge_batch)),
            );
            self.run_script_with_params(script, params, cozo::ScriptMutability::Mutable)?;
        }
        Ok(())
    }

    pub fn run_community_louvain(&self) -> Result<Vec<(String, i64)>> {
        let script = "
            edges[src, dst] := *edge{source: src, target: dst}
            ?[community_id, node] <~ CommunityDetectionLouvain(edges[src, dst], undirected: true)
        ";
        let res = self.run_script(script)?;
        let mut results = Vec::new();
        for row in res.rows {
            if let (Some(DataValue::List(list)), Some(DataValue::Str(node))) =
                (row.first(), row.get(1))
            {
                let comm_id = list
                    .first()
                    .and_then(|v| match v {
                        DataValue::Num(Num::Int(i)) => Some(*i),
                        _ => None,
                    })
                    .unwrap_or(0);
                results.push((node.to_string(), comm_id));
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remove_snippets_for_files() {
        let storage = CozoStorage::new_in_memory().unwrap();

        // Define relation
        storage.run_script(
            ":create snippet_embedding {file_path, name, line_offset => embedding: <F32; 1536>}",
        ).unwrap();

        // Insert dummy data
        let mut params = std::collections::BTreeMap::new();
        let batch = json!([
            ["foo.rs", "func", 0, vec![0.0f32; 1536]],
            ["bar.rs", "func2", 1, vec![0.0f32; 1536]]
        ]);
        params.insert("batch".into(), cozo::DataValue::from(batch));
        storage.run_script_with_params("?[file_path, name, line_offset, embedding] <- $batch\n:put snippet_embedding {file_path, name, line_offset => embedding}", params, cozo::ScriptMutability::Mutable).unwrap();

        // Remove
        storage
            .remove_snippets_for_files(&["foo.rs".to_string()])
            .unwrap();

        // Verify
        let res = storage
            .run_script("?[file_path] := *snippet_embedding{file_path}")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
        let first_row = res.rows.first().unwrap();
        assert_eq!(first_row.first().unwrap(), &DataValue::Str("bar.rs".into()));
    }

    #[test]
    fn setup_schema_without_bridge_tables_omits_turn_session_memory_decision() {
        let storage = CozoStorage::new_in_memory_bare().unwrap();
        storage
            .setup_schema_with_options(init::setup_schema::Options {
                include_bridge_tables: false,
            })
            .unwrap();

        let relations = storage.get_relations().unwrap();
        assert!(!relations.contains(&"Turn".to_string()));
        assert!(!relations.contains(&"Session".to_string()));
        assert!(!relations.contains(&"Memory".to_string()));
        assert!(!relations.contains(&"Decision".to_string()));

        assert!(relations.contains(&"node".to_string()));
        assert!(relations.contains(&"edge".to_string()));
    }

    #[test]
    fn setup_schema_with_bridge_tables_creates_turn_session_memory_decision() {
        let storage = CozoStorage::new_in_memory_bare().unwrap();
        storage
            .setup_schema_with_options(init::setup_schema::Options {
                include_bridge_tables: true,
            })
            .unwrap();

        let relations = storage.get_relations().unwrap();
        assert!(relations.contains(&"Turn".to_string()));
        assert!(relations.contains(&"Session".to_string()));
        assert!(relations.contains(&"Memory".to_string()));
        assert!(relations.contains(&"Decision".to_string()));
    }
}
