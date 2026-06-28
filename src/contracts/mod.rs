pub mod index;
pub mod matcher;
pub mod parser;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AffectedContract {
    pub endpoint_id: String,
    pub path: String,
    pub method: String,
    pub summary: String,
    pub similarity: f32,
    pub spec_file: String,
}

impl Eq for AffectedContract {}

impl PartialOrd for AffectedContract {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AffectedContract {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .similarity
            .partial_cmp(&self.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.path.cmp(&other.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affected_contract_serialization_roundtrip() {
        let contract = AffectedContract {
            endpoint_id: "api/openapi.json::GET::/pets".to_string(),
            path: "/pets".to_string(),
            method: "GET".to_string(),
            summary: "List all pets".to_string(),
            similarity: 0.85,
            spec_file: "api/openapi.json".to_string(),
        };

        let json = serde_json::to_string(&contract).unwrap();
        let parsed: AffectedContract = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.endpoint_id, "api/openapi.json::GET::/pets");
        assert_eq!(parsed.path, "/pets");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.summary, "List all pets");
        assert!((parsed.similarity - 0.85).abs() < 1e-6);
        assert_eq!(parsed.spec_file, "api/openapi.json");
    }

    #[test]
    fn affected_contract_sort_by_similarity_descending() {
        let mut contracts = [
            AffectedContract {
                endpoint_id: "a".to_string(),
                path: "/pets".to_string(),
                method: "GET".to_string(),
                summary: "".to_string(),
                similarity: 0.5,
                spec_file: "api.yaml".to_string(),
            },
            AffectedContract {
                endpoint_id: "b".to_string(),
                path: "/users".to_string(),
                method: "POST".to_string(),
                summary: "".to_string(),
                similarity: 0.9,
                spec_file: "api.yaml".to_string(),
            },
            AffectedContract {
                endpoint_id: "c".to_string(),
                path: "/items".to_string(),
                method: "GET".to_string(),
                summary: "".to_string(),
                similarity: 0.5,
                spec_file: "api.yaml".to_string(),
            },
        ];

        contracts.sort();

        assert!((contracts[0].similarity - 0.9).abs() < 1e-6);
        assert!((contracts[1].similarity - 0.5).abs() < 1e-6);
        assert!((contracts[2].similarity - 0.5).abs() < 1e-6);
        // For ties, sort by path ascending
        assert_eq!(contracts[1].path, "/items");
        assert_eq!(contracts[2].path, "/pets");
    }
}
