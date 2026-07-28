//! Signature-delta enrichment: compare function signatures between HEAD and the
//! working tree for changed files.
//!
//! Modelled on [`RuntimeUsageProvider`](super::runtime_usage::RuntimeUsageProvider)
//! (read previous content → re-run the same extractor → emit deltas) but uses the
//! `gix` HEAD-blob helper — **never** `Command::new("git")` (0088 §2.3).

use crate::git::read_head_blob;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, SignatureDelta};
use crate::index::languages;
use crate::index::signature::{
    METADATA_SIGNATURE, METADATA_SIGNATURE_SHAPE, SymbolSignature, classify_signature_change,
};
use crate::index::symbols::Symbol;
use miette::Result;
use std::path::Path;

pub struct SignatureDeltaProvider;

impl EnrichmentProvider for SignatureDeltaProvider {
    fn name(&self) -> &'static str {
        "Signature Delta Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let mut deltas = Vec::new();

        for change in &packet.changes {
            // One HEAD read per changed file. For renames, content lives at old_path.
            let head_path = change.old_path.as_deref().unwrap_or(change.path.as_path());

            let Some(prev_content) = read_head_blob(&context.project_root, head_path) else {
                // No HEAD / added file / unreadable → no delta (never a false positive).
                continue;
            };

            let prev_symbols = match extract_symbols_for_path(head_path, &prev_content) {
                Ok(syms) => syms,
                Err(_) => {
                    // Unparseable previous content → no delta.
                    continue;
                }
            };

            let curr_symbols = change.symbols.clone().unwrap_or_default();
            deltas.extend(diff_signatures(
                &change.path.to_string_lossy(),
                &prev_symbols,
                &curr_symbols,
            ));
        }

        deltas.sort_unstable();
        packet.signature_deltas = deltas;
        Ok(())
    }
}

fn extract_symbols_for_path(path: &Path, content: &str) -> Result<Vec<Symbol>> {
    match languages::parse_symbols(path, content)? {
        Some(syms) => Ok(syms),
        None => Ok(Vec::new()),
    }
}

/// Match symbols by `(kind, qualified_name || name)`. Unmatched = add/delete, not
/// a signature change.
fn diff_signatures(
    file_path: &str,
    previous: &[Symbol],
    current: &[Symbol],
) -> Vec<SignatureDelta> {
    let mut deltas = Vec::new();

    for curr in current {
        let Some(curr_sig) = symbol_signature(curr) else {
            continue;
        };
        let key = symbol_match_key(curr);
        let Some(prev) = previous.iter().find(|p| symbol_match_key(p) == key) else {
            // Added symbol — not a signature change.
            continue;
        };
        let Some(prev_sig) = symbol_signature(prev) else {
            continue;
        };
        if let Some(class) = classify_signature_change(&prev_sig, &curr_sig) {
            deltas.push(SignatureDelta {
                file_path: file_path.to_string(),
                symbol_name: key,
                previous_signature: prev_sig.text,
                current_signature: curr_sig.text,
                change_class: class.as_str().to_string(),
            });
        }
    }

    deltas
}

fn symbol_match_key(sym: &Symbol) -> String {
    sym.qualified_name
        .clone()
        .unwrap_or_else(|| format!("{:?}:{}", sym.kind, sym.name))
}

fn symbol_signature(sym: &Symbol) -> Option<SymbolSignature> {
    let text = sym.metadata.get(METADATA_SIGNATURE)?.clone();
    let shape = sym.metadata.get(METADATA_SIGNATURE_SHAPE)?.clone();
    Some(SymbolSignature { text, shape })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::index::signature::{
        SignatureParam, SymbolSignatureParts, build_symbol_signature, write_signature_metadata,
    };
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn sym(name: &str, parts: SymbolSignatureParts) -> Symbol {
        let sig = build_symbol_signature(&parts);
        let mut metadata = BTreeMap::new();
        write_signature_metadata(&mut metadata, &sig);
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: None,
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata,
        }
    }

    #[test]
    fn shape_change_emits_delta() {
        let prev = vec![sym(
            "foo",
            SymbolSignatureParts {
                name: "foo".into(),
                modifiers: vec![],
                params: vec![SignatureParam {
                    name: Some("a".into()),
                    type_text: Some("u32".into()),
                }],
                return_type: None,
            },
        )];
        let curr = vec![sym(
            "foo",
            SymbolSignatureParts {
                name: "foo".into(),
                modifiers: vec![],
                params: vec![SignatureParam {
                    name: Some("a".into()),
                    type_text: Some("u64".into()),
                }],
                return_type: None,
            },
        )];
        let deltas = diff_signatures("src/a.rs", &prev, &curr);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].change_class, "shape");
    }

    #[test]
    fn rename_only_is_cosmetic() {
        let prev = vec![sym(
            "foo",
            SymbolSignatureParts {
                name: "foo".into(),
                modifiers: vec![],
                params: vec![SignatureParam {
                    name: Some("a".into()),
                    type_text: Some("u32".into()),
                }],
                return_type: None,
            },
        )];
        let curr = vec![sym(
            "foo",
            SymbolSignatureParts {
                name: "foo".into(),
                modifiers: vec![],
                params: vec![SignatureParam {
                    name: Some("x".into()),
                    type_text: Some("u32".into()),
                }],
                return_type: None,
            },
        )];
        let deltas = diff_signatures("src/a.rs", &prev, &curr);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].change_class, "cosmetic");
    }

    #[test]
    fn body_only_no_delta() {
        let prev = vec![sym(
            "foo",
            SymbolSignatureParts {
                name: "foo".into(),
                modifiers: vec![],
                params: vec![],
                return_type: None,
            },
        )];
        let curr = prev.clone();
        let deltas = diff_signatures("src/a.rs", &prev, &curr);
        assert!(deltas.is_empty());
    }

    #[test]
    fn unmatched_symbol_is_not_signature_change() {
        let prev = vec![sym(
            "old",
            SymbolSignatureParts {
                name: "old".into(),
                modifiers: vec![],
                params: vec![],
                return_type: None,
            },
        )];
        let curr = vec![sym(
            "new",
            SymbolSignatureParts {
                name: "new".into(),
                modifiers: vec![],
                params: vec![],
                return_type: None,
            },
        )];
        assert!(diff_signatures("src/a.rs", &prev, &curr).is_empty());
    }

    #[test]
    fn enrich_no_delta_without_changes() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from(r"C:\dev\ledgerful"),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket::default();
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(packet.signature_deltas.is_empty());
    }

    #[test]
    fn enrich_added_file_no_head_yields_no_delta() {
        // File absent from HEAD → read_head_blob returns None → no delta.
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from(r"C:\dev\ledgerful"),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("this_file_does_not_exist_in_head_0088.rs"),
                status: "Added".to_string(),
                old_path: None,
                is_staged: false,
                symbols: Some(vec![sym(
                    "foo",
                    SymbolSignatureParts {
                        name: "foo".into(),
                        modifiers: vec![],
                        params: vec![],
                        return_type: None,
                    },
                )]),
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(packet.signature_deltas.is_empty());
    }

    #[test]
    fn deltas_are_sorted() {
        let prev = vec![
            sym(
                "b",
                SymbolSignatureParts {
                    name: "b".into(),
                    modifiers: vec![],
                    params: vec![SignatureParam {
                        name: Some("a".into()),
                        type_text: Some("u32".into()),
                    }],
                    return_type: None,
                },
            ),
            sym(
                "a",
                SymbolSignatureParts {
                    name: "a".into(),
                    modifiers: vec![],
                    params: vec![SignatureParam {
                        name: Some("a".into()),
                        type_text: Some("u32".into()),
                    }],
                    return_type: None,
                },
            ),
        ];
        let curr = vec![
            sym(
                "b",
                SymbolSignatureParts {
                    name: "b".into(),
                    modifiers: vec![],
                    params: vec![SignatureParam {
                        name: Some("a".into()),
                        type_text: Some("u64".into()),
                    }],
                    return_type: None,
                },
            ),
            sym(
                "a",
                SymbolSignatureParts {
                    name: "a".into(),
                    modifiers: vec![],
                    params: vec![SignatureParam {
                        name: Some("a".into()),
                        type_text: Some("u64".into()),
                    }],
                    return_type: None,
                },
            ),
        ];
        let mut deltas = diff_signatures("f.rs", &prev, &curr);
        deltas.sort_unstable();
        assert!(deltas[0].symbol_name <= deltas[1].symbol_name);
    }
}
