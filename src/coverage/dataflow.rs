use crate::impact::packet::{CallChain, ChangedFile, DataFlowMatch, DataModel, RiskLevel};
use crate::util::path::normalize_relative_path;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

static SQL_TABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:from|join|into|update|insert\s+into)\s+[`"']?(\w+)[`"']?"#).unwrap()
});

fn extract_sql_table_names(content: &str) -> HashSet<String> {
    SQL_TABLE_RE
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_lowercase()))
        .collect()
}

/// Compute data-flow coupling matches between call chains and changed files.
pub fn compute_data_flow_coupling(
    call_chains: &[CallChain],
    changed_files: &[ChangedFile],
    data_models: &[DataModel],
    _min_change_pct: f64, // Usually 20%
    repo_root: &std::path::Path,
) -> Vec<DataFlowMatch> {
    let mut matches = Vec::new();

    let changed_paths: HashSet<String> = changed_files
        .iter()
        .map(|f| {
            normalize_relative_path(repo_root, &f.path.to_string_lossy())
                .unwrap_or_else(|_| f.path.to_string_lossy().to_string())
        })
        .collect();

    let model_names: HashSet<String> = data_models
        .iter()
        .map(|m| m.model_name.to_lowercase())
        .collect();

    // Cache: file_path -> SQL table names found in that file (for fallback resolution)
    let mut sql_cache: HashMap<PathBuf, HashSet<String>> = HashMap::new();

    for chain in call_chains {
        if chain.nodes.len() < 2 {
            continue;
        }

        let mut changed_nodes = Vec::new();
        let mut has_data_model = false;

        for node in &chain.nodes {
            let path_str = normalize_relative_path(repo_root, &node.file_path.to_string_lossy())
                .unwrap_or_else(|_| node.file_path.to_string_lossy().to_string());
            if changed_paths.contains(&path_str) {
                changed_nodes.push(node.symbol.clone());
            }

            if node.is_data_model || model_names.contains(&node.symbol.to_lowercase()) {
                has_data_model = true;
            } else if !model_names.is_empty() {
                // Fallback: scan file for SQL table names referencing known models
                let full_path = repo_root.join(&node.file_path);
                let tables = sql_cache.entry(full_path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(&full_path)
                        .map(|content| extract_sql_table_names(&content))
                        .unwrap_or_default()
                });
                if tables.iter().any(|t| model_names.contains(t)) {
                    has_data_model = true;
                }
            }
        }

        let change_pct = changed_nodes.len() as f64 / chain.nodes.len() as f64;

        // Requirement: >= threshold change AND at least one data model in chain
        if change_pct >= _min_change_pct && has_data_model {
            let risk = if chain.nodes.len() > 5 || changed_nodes.len() >= 3 {
                RiskLevel::High
            } else {
                RiskLevel::Medium
            };

            let chain_label = chain
                .nodes
                .iter()
                .map(|n| n.symbol.clone())
                .collect::<Vec<_>>()
                .join(" -> ");

            matches.push(DataFlowMatch {
                chain_label,
                changed_nodes,
                total_nodes: chain.nodes.len(),
                change_pct,
                risk,
            });
        }
    }

    // Sort by change_pct descending (as per spec/determinism)
    matches.sort_by(|a, b| {
        b.change_pct
            .partial_cmp(&a.change_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chain_label.cmp(&b.chain_label))
    });

    matches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{CallChainNode, FileAnalysisStatus};
    use std::path::{Path, PathBuf};

    #[test]
    fn test_compute_data_flow_coupling_basic() {
        let chain = CallChain {
            nodes: vec![
                CallChainNode {
                    symbol: "get_user".to_string(),
                    file_path: PathBuf::from("src/api.rs"),
                    is_data_model: false,
                    is_external: false,
                },
                CallChainNode {
                    symbol: "User".to_string(),
                    file_path: PathBuf::from("src/models.rs"),
                    is_data_model: true,
                    is_external: false,
                },
            ],
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/api.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.2, Path::new("."));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].change_pct, 0.5);
        assert_eq!(matches[0].risk, RiskLevel::Medium);
    }

    #[test]
    fn test_compute_data_flow_coupling_threshold() {
        let mut nodes = Vec::new();
        for i in 0..10 {
            nodes.push(CallChainNode {
                symbol: format!("fn{}", i),
                file_path: PathBuf::from(format!("src/f{}.rs", i)),
                is_data_model: i == 9,
                is_external: false,
            });
        }
        let chain = CallChain {
            nodes,
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/f0.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        // 1/10 = 10%, threshold is 20%
        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.2, Path::new("."));
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_compute_data_flow_coupling_high_confidence() {
        let chain = CallChain {
            nodes: vec![
                CallChainNode {
                    symbol: "get_user".to_string(),
                    file_path: PathBuf::from("src/api.rs"),
                    is_data_model: false,
                    is_external: false,
                },
                CallChainNode {
                    symbol: "User".to_string(),
                    file_path: PathBuf::from("src/models.rs"),
                    is_data_model: true,
                    is_external: false,
                },
            ],
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/api.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        // 1/2 = 50%, threshold is 20%
        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.2, Path::new("."));
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].chain_label, "get_user -> User");
        assert!((matches[0].change_pct - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_data_flow_coupling_threshold_edge() {
        let chain = CallChain {
            nodes: vec![
                CallChainNode {
                    symbol: "s1".to_string(),
                    file_path: PathBuf::from("f1.rs"),
                    is_data_model: false,
                    is_external: false,
                },
                CallChainNode {
                    symbol: "s2".to_string(),
                    file_path: PathBuf::from("f2.rs"),
                    is_data_model: true,
                    is_external: false,
                },
            ],
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("f1.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        // 1/2 = 50%
        // threshold 0.6 -> no match
        let matches = compute_data_flow_coupling(
            std::slice::from_ref(&chain),
            &changed_files,
            &[],
            0.6,
            Path::new("."),
        );
        assert_eq!(matches.len(), 0);

        // threshold 0.4 -> match
        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.4, Path::new("."));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_sql_table_name_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("src/handler.rs");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            &file_path,
            r#"fn get_users() {
    let rows = db.query("SELECT * FROM users WHERE active = true");
}
"#,
        )
        .unwrap();

        let chain = CallChain {
            nodes: vec![
                CallChainNode {
                    symbol: "get_users".to_string(),
                    file_path: PathBuf::from("src/handler.rs"),
                    is_data_model: false,
                    is_external: false,
                },
                CallChainNode {
                    symbol: "process".to_string(),
                    file_path: PathBuf::from("src/other.rs"),
                    is_data_model: false,
                    is_external: false,
                },
            ],
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/handler.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        let data_models = vec![DataModel {
            model_name: "users".to_string(),
            model_kind: "table".to_string(),
            confidence: 1.0,
            evidence: Some("sql".to_string()),
        }];

        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &data_models, 0.2, dir.path());

        assert_eq!(matches.len(), 1, "SQL fallback should detect 'users' table");
        assert_eq!(matches[0].chain_label, "get_users -> process");
    }

    #[test]
    fn test_three_of_ten_changed_match() {
        let mut nodes = Vec::new();
        for i in 0..10 {
            nodes.push(CallChainNode {
                symbol: format!("fn{}", i),
                file_path: PathBuf::from(format!("src/f{}.rs", i)),
                is_data_model: i == 9,
                is_external: false,
            });
        }
        let chain = CallChain {
            nodes,
            has_cycle: false,
        };

        let changed_files = vec![
            ChangedFile {
                path: PathBuf::from("src/f0.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            },
            ChangedFile {
                path: PathBuf::from("src/f1.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            },
            ChangedFile {
                path: PathBuf::from("src/f2.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: true,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            },
        ];

        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.2, Path::new("."));
        assert_eq!(matches.len(), 1);
        assert!((matches[0].change_pct - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_chain_depth_one_skipped() {
        let chain = CallChain {
            nodes: vec![CallChainNode {
                symbol: "handler".to_string(),
                file_path: PathBuf::from("src/handler.rs"),
                is_data_model: false,
                is_external: false,
            }],
            has_cycle: false,
        };

        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/handler.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];

        let matches =
            compute_data_flow_coupling(&[chain], &changed_files, &[], 0.2, Path::new("."));
        assert_eq!(matches.len(), 0);
    }
}
