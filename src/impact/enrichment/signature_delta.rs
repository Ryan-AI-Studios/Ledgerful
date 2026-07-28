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
use std::collections::HashMap;
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
///
/// When a match key is **ambiguous** (multiple previous or current symbols share
/// it), skip emission for that key — never invent a cross-paired false positive.
fn diff_signatures(
    file_path: &str,
    previous: &[Symbol],
    current: &[Symbol],
) -> Vec<SignatureDelta> {
    let mut prev_by_key: HashMap<String, Vec<&Symbol>> = HashMap::new();
    for p in previous {
        prev_by_key.entry(symbol_match_key(p)).or_default().push(p);
    }
    let mut curr_by_key: HashMap<String, Vec<&Symbol>> = HashMap::new();
    for c in current {
        curr_by_key.entry(symbol_match_key(c)).or_default().push(c);
    }

    let mut deltas = Vec::new();
    // Deterministic iteration over keys present on the current side.
    let mut keys: Vec<&String> = curr_by_key.keys().collect();
    keys.sort();
    for key in keys {
        let Some(curr_syms) = curr_by_key.get(key.as_str()) else {
            continue;
        };
        let Some(prev_syms) = prev_by_key.get(key.as_str()) else {
            // Added symbol — not a signature change.
            continue;
        };
        // Ambiguous multi-match: refuse to guess pairing.
        if curr_syms.len() != 1 || prev_syms.len() != 1 {
            continue;
        }
        let curr = curr_syms[0];
        let prev = prev_syms[0];
        let Some(curr_sig) = symbol_signature(curr) else {
            continue;
        };
        let Some(prev_sig) = symbol_signature(prev) else {
            continue;
        };
        if let Some(class) = classify_signature_change(&prev_sig, &curr_sig) {
            deltas.push(SignatureDelta {
                file_path: file_path.to_string(),
                symbol_name: key.clone(),
                previous_signature: prev_sig.text,
                current_signature: curr_sig.text,
                change_class: class.as_str().to_string(),
            });
        }
    }

    deltas
}

/// Stable match key: always `Kind:qualified_or_name` so kind is never dropped
/// when `qualified_name` is set (R1-05).
fn symbol_match_key(sym: &Symbol) -> String {
    let name = sym.qualified_name.as_deref().unwrap_or(sym.name.as_str());
    format!("{:?}:{}", sym.kind, name)
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
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn sym(name: &str, parts: SymbolSignatureParts) -> Symbol {
        sym_with(name, SymbolKind::Function, None, parts)
    }

    fn sym_with(
        name: &str,
        kind: SymbolKind,
        qualified_name: Option<&str>,
        parts: SymbolSignatureParts,
    ) -> Symbol {
        let sig = build_symbol_signature(&parts);
        let mut metadata = BTreeMap::new();
        write_signature_metadata(&mut metadata, &sig);
        Symbol {
            name: name.to_string(),
            kind,
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: qualified_name.map(str::to_string),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata,
        }
    }

    fn enrichment_ctx() -> (StorageManager, crate::config::model::Config) {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        (storage, config)
    }

    fn git(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command")
    }

    fn init_git_repo(dir: &std::path::Path) {
        assert!(git(dir, &["init", "-b", "main"]).status.success());
        assert!(
            git(dir, &["config", "user.email", "test@example.com"])
                .status
                .success()
        );
        assert!(git(dir, &["config", "user.name", "test"]).status.success());
    }

    fn changed_file(
        path: &str,
        status: &str,
        old_path: Option<&str>,
        symbols: Option<Vec<Symbol>>,
    ) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: status.to_string(),
            old_path: old_path.map(PathBuf::from),
            is_staged: false,
            symbols,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
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
    fn two_impl_new_only_changed_one_emits_one_delta() {
        // R1-01: two `fn new()` methods must not cross-pair. Only Bar.new shape changes.
        let prev_src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn new() -> Self { Foo } }
            impl Bar { fn new() -> Self { Bar } }
        "#;
        let curr_src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn new() -> Self { Foo } }
            impl Bar { fn new(x: u32) -> Self { let _ = x; Bar } }
        "#;
        let prev = crate::index::languages::rust::extract_symbols(prev_src)
            .unwrap()
            .unwrap();
        let curr = crate::index::languages::rust::extract_symbols(curr_src)
            .unwrap()
            .unwrap();
        let deltas = diff_signatures("src/lib.rs", &prev, &curr);
        assert_eq!(
            deltas.len(),
            1,
            "expected only Bar.new delta, got {deltas:?}"
        );
        assert!(
            deltas[0].symbol_name.contains("Bar.new"),
            "delta must name Bar.new, got {}",
            deltas[0].symbol_name
        );
        assert_eq!(deltas[0].change_class, "shape");
        // Foo.new must not appear as a false-positive shape change.
        assert!(
            !deltas.iter().any(|d| d.symbol_name.contains("Foo.new")),
            "Foo.new must not false-positive: {deltas:?}"
        );
    }

    #[test]
    fn ambiguous_match_key_skips_emission() {
        // Safety net when qualified_name is still missing and names collide.
        let parts_u32 = SymbolSignatureParts {
            name: "new".into(),
            modifiers: vec![],
            params: vec![SignatureParam {
                name: Some("a".into()),
                type_text: Some("u32".into()),
            }],
            return_type: None,
        };
        let parts_u64 = SymbolSignatureParts {
            name: "new".into(),
            modifiers: vec![],
            params: vec![SignatureParam {
                name: Some("a".into()),
                type_text: Some("u64".into()),
            }],
            return_type: None,
        };
        let prev = vec![sym("new", parts_u32.clone()), sym("new", parts_u32)];
        let curr = vec![sym("new", parts_u64.clone()), sym("new", parts_u64)];
        // Both sides have two Function:new keys → ambiguous → no deltas.
        assert!(diff_signatures("src/a.rs", &prev, &curr).is_empty());
    }

    #[test]
    fn match_key_includes_kind_with_qualified_name() {
        let s = sym_with(
            "read",
            SymbolKind::Method,
            Some("Reader.read"),
            SymbolSignatureParts {
                name: "read".into(),
                modifiers: vec![],
                params: vec![],
                return_type: None,
            },
        );
        assert_eq!(symbol_match_key(&s), "Method:Reader.read");
    }

    #[test]
    fn enrich_no_delta_without_changes() {
        let (storage, config) = enrichment_ctx();
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
        let (storage, config) = enrichment_ctx();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from(r"C:\dev\ledgerful"),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![changed_file(
                "this_file_does_not_exist_in_head_0088.rs",
                "Added",
                None,
                Some(vec![sym(
                    "foo",
                    SymbolSignatureParts {
                        name: "foo".into(),
                        modifiers: vec![],
                        params: vec![],
                        return_type: None,
                    },
                )]),
            )],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(packet.signature_deltas.is_empty());
    }

    #[test]
    fn enrich_unborn_head_yields_no_delta() {
        // Fresh repo with no commits → HEAD unborn → read_head_blob None.
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();

        let (storage, config) = enrichment_ctx();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: dir.path().to_path_buf(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![changed_file(
                "lib.rs",
                "Added",
                None,
                Some(vec![sym(
                    "foo",
                    SymbolSignatureParts {
                        name: "foo".into(),
                        modifiers: vec![],
                        params: vec![],
                        return_type: None,
                    },
                )]),
            )],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(
            packet.signature_deltas.is_empty(),
            "unborn HEAD must not invent signature deltas"
        );
    }

    #[test]
    fn enrich_deleted_file_yields_no_delta() {
        // Deleted files carry no current symbols → never a signature-changed false positive.
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join("gone.rs"), "fn foo(a: u32) {}\n").unwrap();
        assert!(git(dir.path(), &["add", "."]).status.success());
        assert!(git(dir.path(), &["commit", "-m", "init"]).status.success());
        fs::remove_file(dir.path().join("gone.rs")).unwrap();

        let (storage, config) = enrichment_ctx();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: dir.path().to_path_buf(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        // Packet path for Deleted: symbols None (map_snapshot_to_packet behavior).
        let mut packet = ImpactPacket {
            changes: vec![changed_file("gone.rs", "Deleted", None, None)],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(
            packet.signature_deltas.is_empty(),
            "deleted file must not emit signature deltas"
        );
    }

    #[test]
    fn enrich_renamed_file_uses_old_path_for_head() {
        // Rename: HEAD blob is at old_path; current symbols at new path.
        // Same signature → no delta (rename is not a shape change).
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let src = "pub fn greet(name: String) -> bool { true }\n";
        fs::write(dir.path().join("old.rs"), src).unwrap();
        assert!(git(dir.path(), &["add", "."]).status.success());
        assert!(git(dir.path(), &["commit", "-m", "init"]).status.success());
        // Simulate rename in worktree (git mv not required for gix HEAD lookup).
        fs::write(dir.path().join("new.rs"), src).unwrap();
        fs::remove_file(dir.path().join("old.rs")).unwrap();

        let curr_syms = crate::index::languages::rust::extract_symbols(src)
            .unwrap()
            .unwrap();
        let (storage, config) = enrichment_ctx();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: dir.path().to_path_buf(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![changed_file(
                "new.rs",
                "Renamed",
                Some("old.rs"),
                Some(curr_syms),
            )],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(
            packet.signature_deltas.is_empty(),
            "pure rename with identical signatures must not emit deltas: {:?}",
            packet.signature_deltas
        );
    }

    #[test]
    fn enrich_unparseable_previous_yields_no_delta() {
        // HEAD content present but extractor path returns no usable symbols for the
        // head path extension (or empty extract) → no false-positive shape delta.
        // Using a non-language extension at old_path while current has symbols.
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        // Blob at HEAD for "data.bin" — not a language we extract signatures from.
        fs::write(dir.path().join("data.bin"), b"not source code\n").unwrap();
        assert!(git(dir.path(), &["add", "."]).status.success());
        assert!(git(dir.path(), &["commit", "-m", "init"]).status.success());

        let (storage, config) = enrichment_ctx();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: dir.path().to_path_buf(),
            warnings: Arc::new(Mutex::new(Vec::new())),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(120),
        };
        let mut packet = ImpactPacket {
            changes: vec![changed_file(
                "data.bin",
                "Modified",
                None,
                Some(vec![sym(
                    "foo",
                    SymbolSignatureParts {
                        name: "foo".into(),
                        modifiers: vec![],
                        params: vec![],
                        return_type: None,
                    },
                )]),
            )],
            ..Default::default()
        };
        SignatureDeltaProvider
            .enrich(&context, &mut packet)
            .unwrap();
        assert!(
            packet.signature_deltas.is_empty(),
            "unparseable/unsupported previous content must not invent deltas"
        );
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
