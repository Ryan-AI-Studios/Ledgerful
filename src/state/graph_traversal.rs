use crate::state::graph_kinds::EdgeKind;
use crate::state::storage_cozo::CozoStorage;
use miette::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TraversalResult {
    pub target_id: String,
    pub relation: String,
    pub hops: usize,
}

pub struct GraphTraversal<'a> {
    storage: &'a CozoStorage,
}

impl<'a> GraphTraversal<'a> {
    pub fn new(storage: &'a CozoStorage) -> Self {
        Self { storage }
    }

    pub fn get_related_entities(
        &self,
        seed_id: &str,
        relation_kinds: Option<&[EdgeKind]>,
        max_hops: usize,
    ) -> Result<Vec<TraversalResult>> {
        let relations_filter = if let Some(kinds) = relation_kinds {
            let kinds_str: Vec<String> = kinds.iter().map(|k| format!("'{}'", k)).collect();
            format!("relations[rel] <- [{}]", kinds_str.join(", "))
        } else {
            "".to_string()
        };

        let filter_clause = if relation_kinds.is_some() {
            ", relations[rel]"
        } else {
            ""
        };

        let mut all_results = Vec::new();

        for hop in 1..=max_hops {
            let script = if hop == 1 {
                format!(
                    "{}\n?[target, relation, hops] := *edge{{source: '{}', target: target, relation: relation}}{}, hops = 1",
                    relations_filter, seed_id, filter_clause
                )
            } else {
                // For multi-hop, we use Datalog reachability
                // This is a simplified version. For more hops, we'd want a recursive rule.
                format!(
                    "{}\nreachable[t, r, h] := *edge{{source: '{}', target: t, relation: r}}, h = 1\n\
                     reachable[t, r, h] := reachable[m, _, h_prev], *edge{{source: m, target: t, relation: r}}, h = h_prev + 1, h <= {}\n\
                     ?[target, relation, hops] := reachable[target, relation, hops]{}, hops == {}",
                    relations_filter, seed_id, max_hops, filter_clause, hop
                )
            };

            let res = self.storage.run_script(&script)?;
            for row in res.rows {
                if let (
                    Some(cozo::DataValue::Str(target)),
                    Some(cozo::DataValue::Str(rel)),
                    Some(cozo::DataValue::Num(cozo::Num::Int(h))),
                ) = (row.first(), row.get(1), row.get(2))
                {
                    all_results.push(TraversalResult {
                        target_id: target.to_string(),
                        relation: rel.to_string(),
                        hops: *h as usize,
                    });
                }
            }
        }

        Ok(all_results)
    }
}
