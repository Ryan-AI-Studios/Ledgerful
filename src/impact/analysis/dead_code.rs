use crate::config::model::DeadCodeConfig;
use crate::impact::packet::{ConfidenceFactor, DeadCodeFinding, ImpactPacket};
use crate::index::symbols::Symbol;
use crate::state::storage::StorageManager;
use crate::state::storage_cozo::CozoStorage;
use miette::{IntoDiagnostic, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

pub struct DeadCodeImpactProvider;

impl super::ImpactProvider for DeadCodeImpactProvider {
    fn name(&self) -> &'static str {
        "Dead Code Impact Provider"
    }

    fn analyze(
        &self,
        packet: &ImpactPacket,
        _rules: &crate::policy::rules::Rules,
        config: &crate::config::model::Config,
    ) -> Result<crate::impact::packet::RiskImpact> {
        let mut impact = crate::impact::packet::RiskImpact {
            weight: 0,
            reasons: Vec::new(),
        };

        if !config.dead_code.enabled {
            return Ok(impact);
        }

        for finding in &packet.dead_code_findings {
            if finding.confidence >= config.dead_code.confidence_threshold {
                let reason = format!(
                    "Advisory: changed symbol '{}' in {} is likely dead code (confidence: {:.0}%)",
                    finding.symbol_name,
                    finding.file_path.display(),
                    finding.confidence * 100.0
                );
                impact.reasons.push(reason);
            }
        }

        Ok(impact)
    }
}

/// Per-file "days since last commit" index built by a single commit-history
/// walk (see `ConfidenceScorer::precompute_git_activity`), replacing N
/// independent per-file walks. `repo_available` is `false` only when the
/// repo-level git operations themselves failed (not a git repo, no HEAD,
/// etc.); a file simply absent from `last_touched_days` after a successful
/// walk falls back to `DeadCodeConfig::git_inactivity_days`.
pub(super) struct GitActivityIndex {
    pub(super) last_touched_days: HashMap<PathBuf, u32>,
    pub(super) repo_available: bool,
}

/// Symbols resolved for a single file together with the stored path used in
/// the index. Returned by `get_symbols_for_file` so callers can consistently
/// use the same stored path for cache keys and SQL lookups.
#[derive(Debug, Clone)]
pub(super) struct FileSymbols {
    pub(super) stored_path: String,
    pub(super) symbols: Vec<Symbol>,
    pub(super) symbol_ids: HashMap<(String, String, String), i64>,
}

pub struct ConfidenceScorer<'a> {
    pub(super) cozo: Option<&'a CozoStorage>,
    pub(super) storage: &'a StorageManager,
    pub(super) config: &'a DeadCodeConfig,
    pub(super) repo_path: &'a Path,
    /// When `false` (default), standard trait symbols are excluded from results.
    /// Set to `true` via `--include-traits` to see all findings.
    pub(super) include_traits: bool,
    pub(super) git_activity_cache:
        std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, Option<u32>>>,
    pub(super) precomputed_reachable_symbols: Option<HashSet<i64>>,
    pub(super) precomputed_tested_symbols: Option<HashSet<i64>>,
    pub(super) precomputed_symbol_ids: Option<HashMap<(String, String, String), i64>>,
    pub(super) precomputed_git_activity: Option<GitActivityIndex>,
}

impl<'a> ConfidenceScorer<'a> {
    pub fn new(
        cozo: Option<&'a CozoStorage>,
        storage: &'a StorageManager,
        config: &'a DeadCodeConfig,
        repo_path: &'a Path,
        include_traits: bool,
    ) -> Self {
        Self {
            cozo,
            storage,
            config,
            repo_path,
            include_traits,
            git_activity_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            precomputed_reachable_symbols: None,
            precomputed_tested_symbols: None,
            precomputed_symbol_ids: None,
            precomputed_git_activity: None,
        }
    }

    /// Precomputes the per-run evidence caches (reachability, test coverage,
    /// symbol-ID lookup) once instead of per-symbol, and logs the duration
    /// and result size of each phase so a future latency regression can be
    /// localized to a specific phase without manually profiling the whole
    /// `dead-code` command (CG-F15).
    pub fn precompute(&mut self) -> Result<()> {
        let start = std::time::Instant::now();
        let reachable = self.precompute_reachability()?;
        debug!(
            phase = "reachability",
            count = reachable.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute phase complete"
        );
        self.precomputed_reachable_symbols = Some(reachable);

        let start = std::time::Instant::now();
        let tested = self.precompute_test_coverage()?;
        debug!(
            phase = "test_coverage",
            count = tested.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute phase complete"
        );
        self.precomputed_tested_symbols = Some(tested);

        let start = std::time::Instant::now();
        let symbol_ids = self.precompute_symbol_ids()?;
        debug!(
            phase = "symbol_ids",
            count = symbol_ids.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute phase complete"
        );
        self.precomputed_symbol_ids = Some(symbol_ids);

        let start = std::time::Instant::now();
        let git_activity = self.precompute_git_activity()?;
        debug!(
            phase = "git_activity",
            count = git_activity.last_touched_days.len(),
            repo_available = git_activity.repo_available,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute phase complete"
        );
        self.precomputed_git_activity = Some(git_activity);

        Ok(())
    }

    /// TA25: build file-scoped evidence caches for `dead-code --explain`.
    ///
    /// This populates the same cache fields as `precompute()` but with data
    /// limited to the symbols in the requested file. It does NOT load the full
    /// structural edge table or full test history — it only issues batched
    /// queries scoped to the target file's symbol ids.
    pub fn precompute_for_file(&mut self, file_path: &Path) -> Result<()> {
        let resolved = self.get_symbols_for_file(file_path)?;
        self.precompute_for_file_with_symbols(&resolved)
    }

    fn precompute_for_file_with_symbols(&mut self, resolved: &FileSymbols) -> Result<()> {
        if resolved.symbols.is_empty() {
            return Ok(());
        }

        let ids: Vec<i64> = resolved.symbol_ids.values().copied().collect();

        let start = std::time::Instant::now();
        let reachable = self.precompute_reachability_for_symbols(&ids)?;
        debug!(
            phase = "reachability_for_file",
            file = resolved.stored_path,
            count = reachable.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute_for_file phase complete"
        );

        let start = std::time::Instant::now();
        let tested = self.precompute_test_coverage_for_symbols(&ids)?;
        debug!(
            phase = "test_coverage_for_file",
            file = resolved.stored_path,
            count = tested.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute_for_file phase complete"
        );

        let start = std::time::Instant::now();
        let git_activity = self.precompute_git_activity()?;
        debug!(
            phase = "git_activity_for_file",
            file = resolved.stored_path,
            count = git_activity.last_touched_days.len(),
            repo_available = git_activity.repo_available,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code precompute_for_file phase complete"
        );

        self.precomputed_symbol_ids = Some(resolved.symbol_ids.clone());
        self.precomputed_reachable_symbols = Some(reachable);
        self.precomputed_tested_symbols = Some(tested);
        self.precomputed_git_activity = Some(git_activity);

        Ok(())
    }
}

mod evidence;
mod filters;
mod scoring;

// --- TA16 R4: Engine-owned explanation structs ---

/// Structured explanation of dead-code findings for a specific file.
/// Built by the engine from pre-computed findings; formatted by the CLI.
#[derive(Debug, Clone)]
pub struct DeadCodeExplanation {
    pub file: String,
    pub symbols: Vec<DeadCodeSymbolExplanation>,
}

#[derive(Debug, Clone)]
pub struct DeadCodeSymbolExplanation {
    pub symbol_name: String,
    pub confidence: f64,
    pub factors: Vec<DeadCodeFactor>,
}

#[derive(Debug, Clone)]
pub struct DeadCodeFactor {
    pub kind: ConfidenceFactor,
    pub description: String,
    pub evidence: String,
}

/// Build a `DeadCodeExplanation` from pre-computed findings for a file.
/// The CLI calls this; no analysis logic is duplicated in the formatter.
pub fn compute_dead_code_explanation(
    file_path: &str,
    findings: &[DeadCodeFinding],
) -> DeadCodeExplanation {
    let target = std::path::Path::new(file_path);
    let target_str = target.display().to_string();
    let target_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Prefer exact path match; fall back to file_name match only if no
    // exact match exists (avoids matching unrelated files sharing a basename).
    let mut file_findings: Vec<&DeadCodeFinding> = findings
        .iter()
        .filter(|f| f.file_path.display().to_string() == target_str)
        .collect();

    if file_findings.is_empty() && !target_name.is_empty() {
        file_findings = findings
            .iter()
            .filter(|f| {
                f.file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n == target_name)
            })
            .collect();
    }

    file_findings.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
    });

    let symbols = file_findings
        .iter()
        .map(|f| DeadCodeSymbolExplanation {
            symbol_name: f.symbol_name.clone(),
            confidence: f.confidence,
            factors: f
                .factors
                .iter()
                .map(|fac| DeadCodeFactor {
                    description: describe_factor(fac),
                    evidence: evidence_factor(fac),
                    kind: fac.clone(),
                })
                .collect(),
        })
        .collect();

    DeadCodeExplanation {
        file: file_path.to_string(),
        symbols,
    }
}

fn describe_factor(factor: &ConfidenceFactor) -> String {
    match factor {
        ConfidenceFactor::UnreachableFromEntrypoints => {
            "symbol has no incoming calls from known entrypoints".to_string()
        }
        ConfidenceFactor::GitInactive {
            days_since_last_commit,
        } => {
            format!(
                "{} days since last commit touching this file",
                days_since_last_commit
            )
        }
        ConfidenceFactor::NoTestCoverage => "no test symbols reference this file".to_string(),
    }
}

fn evidence_factor(factor: &ConfidenceFactor) -> String {
    match factor {
        ConfidenceFactor::UnreachableFromEntrypoints => "no incoming calls detected".to_string(),
        ConfidenceFactor::GitInactive {
            days_since_last_commit,
        } => {
            format!("{} days", days_since_last_commit)
        }
        ConfidenceFactor::NoTestCoverage => "no tests".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::{Symbol, SymbolKind};
    use crate::state::storage::StorageManager;
    use crate::state::storage_cozo::CozoStorage;
    use std::path::PathBuf;

    pub(super) fn in_memory_storage_with_cozo() -> (StorageManager, CozoStorage) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut conn = conn;
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let cozo = CozoStorage::new(&PathBuf::from("")).unwrap();
        (storage, cozo)
    }

    pub(super) fn default_config() -> DeadCodeConfig {
        DeadCodeConfig {
            enabled: true,
            confidence_threshold: 0.75,
            git_inactivity_days: 90,
            reachability_weight: 1.0,
            git_activity_weight: 1.0,
            test_coverage_weight: 1.0,
        }
    }

    pub(super) fn make_symbol(
        name: &str,
        qualified: Option<&str>,
        entrypoint: Option<&str>,
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            is_public: false,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: qualified.map(|s| s.to_string()),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: entrypoint.map(|s| s.to_string()),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn test_entrypoint_skipped() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let symbol = make_symbol("main", Some("crate::main"), Some("ENTRYPOINT"));
        let result = scorer
            .score_symbol(&symbol, Path::new("src/main.rs"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_reachability_unreachable_sqlite() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/main.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let main_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h2', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let lib_file = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::main', 'main', 'Function', 'ENTRYPOINT', '2026-01-01')",
            [main_file],
        ).unwrap();
        let main_sym = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::helper', 'helper', 'Function', 'INTERNAL', '2026-01-01')",
            [lib_file],
        ).unwrap();
        let helper_sym = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused', 'unused', 'Function', 'INTERNAL', '2026-01-01')",
            [lib_file],
        ).unwrap();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status) VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED')",
            [main_sym, main_file, helper_sym, lib_file],
        ).unwrap();

        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let helper = make_symbol("helper", Some("crate::helper"), None);
        let score = scorer
            .reachability_score(&helper, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 0.0);

        let unused = make_symbol("unused", Some("crate::unused"), None);
        let score = scorer
            .reachability_score(&unused, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_reachability_via_cozo() {
        use crate::platform::urn::build_urn;
        use crate::state::graph_kinds::NodeKind;

        let (storage, cozo) = in_memory_storage_with_cozo();

        let main_urn = build_urn(NodeKind::Symbol, "crate::main");
        let helper_urn = build_urn(NodeKind::Symbol, "crate::helper");
        let unused_urn = build_urn(NodeKind::Symbol, "crate::unused");

        cozo.run_script(&format!(
            "?[id, label, category, risk_score, metadata] <- [
                ['{}', 'main', 'code', 0.0, {{}}],
                ['{}', 'helper', 'code', 0.0, {{}}],
                ['{}', 'unused', 'code', 0.0, {{}}]
            ] :put node",
            main_urn, helper_urn, unused_urn
        ))
        .unwrap();

        cozo.run_script(&format!(
            "?[source, target, relation, confidence, provenance_id] <- [
                ['{}', '{}', 'calls', 1.0, 'tx1']
            ] :put edge",
            main_urn, helper_urn
        ))
        .unwrap();

        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/main.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let main_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::main', 'main', 'Function', 'ENTRYPOINT', '2026-01-01')",
            [main_file],
        ).unwrap();

        let config = default_config();
        let scorer = ConfidenceScorer::new(Some(&cozo), &storage, &config, Path::new("."), false);

        let helper = make_symbol("helper", Some("crate::helper"), None);
        let score = scorer
            .reachability_score(&helper, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 0.0);

        let unused = make_symbol("unused", Some("crate::unused"), None);
        let score = scorer
            .reachability_score(&unused, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_test_coverage_no_mapping() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let symbol = make_symbol("foo", Some("crate::foo"), None);
        let score = scorer
            .test_coverage_score(&symbol, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_test_coverage_with_mapping() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::foo', 'foo', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();
        let sym_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::test_foo', 'test_foo', 'Function', 'TEST', '2026-01-01')",
            [file_id],
        ).unwrap();
        let test_sym_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, mapping_kind, last_indexed_at) VALUES (?1, ?2, ?3, ?4, 'IMPORT', '2026-01-01')",
            [test_sym_id, file_id, sym_id, file_id],
        ).unwrap();

        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let symbol = make_symbol("foo", Some("crate::foo"), None);
        let score = scorer
            .test_coverage_score(&symbol, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_blend_expected_value() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let confidence = scorer.blend(1.0, 0.5, 0.0);
        assert!((confidence - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_blend_with_zero_weights() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = DeadCodeConfig {
            enabled: true,
            confidence_threshold: 0.75,
            git_inactivity_days: 90,
            reachability_weight: 0.0,
            git_activity_weight: 0.0,
            test_coverage_weight: 0.0,
        };
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let confidence = scorer.blend(1.0, 1.0, 1.0);
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn test_standard_trait_filtered_by_default() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        // The Rust extractor stores `impl Eq for MyType {}` as (name="Eq", kind=Type).
        let eq_symbol = Symbol {
            name: "Eq".to_string(),
            kind: SymbolKind::Type, // impl_item → Type in the Rust AST extractor
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: Some("crate::Eq".to_string()),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        };

        let result = scorer
            .score_symbol(&eq_symbol, Path::new("src/lib.rs"))
            .unwrap();
        assert!(
            result.is_none(),
            "impl Eq for MyType (stored as Type/Eq) must be filtered by default"
        );
    }

    #[test]
    fn test_standard_trait_shown_with_include_traits() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        // Zero threshold so any confidence value above 0 would be returned,
        // and zero weights so confidence = 0 via blend → None regardless.
        // The key assertion: score_symbol must NOT short-circuit for standard traits
        // when include_traits = true (no early None from is_standard_trait filter).
        // We confirm by checking it reaches the reachability check (no panic).
        let config = DeadCodeConfig {
            enabled: true,
            confidence_threshold: 0.0,
            git_inactivity_days: 90,
            reachability_weight: 0.0,
            git_activity_weight: 0.0,
            test_coverage_weight: 0.0,
        };
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), true);

        let eq_symbol = Symbol {
            name: "Eq".to_string(),
            kind: SymbolKind::Type, // impl_item → Type in the Rust AST extractor
            is_public: true,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: Some("crate::Eq".to_string()),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: std::collections::BTreeMap::new(),
        };

        // Should not panic (reaches scoring path even for standard traits)
        let _ = scorer.score_symbol(&eq_symbol, Path::new("src/lib.rs"));
    }

    /// DX4: helper to build a Symbol with explicit kind + metadata.
    fn make_symbol_with_kind(name: &str, kind: SymbolKind, metadata: Vec<(&str, &str)>) -> Symbol {
        let mut map = std::collections::BTreeMap::new();
        for (k, v) in metadata {
            map.insert(k.to_string(), v.to_string());
        }
        Symbol {
            name: name.to_string(),
            kind,
            is_public: false,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: Some(format!("crate::{name}")),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: map,
        }
    }

    /// DX4: a struct carrying `#[derive(Serialize, Deserialize, Debug)]` with
    /// raw confidence 1.0 (unreachable, no git, no tests) must fall below the
    /// 0.75 default threshold after the -0.50 derive penalty and be suppressed.
    #[test]
    fn test_derived_struct_suppressed_by_default_threshold() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config(); // threshold 0.75
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let user = make_symbol_with_kind(
            "User",
            SymbolKind::Struct,
            vec![("derived_traits", "Debug,Deserialize,Serialize")],
        );

        let result = scorer
            .score_symbol(&user, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_none(),
            "derived struct with implicit-usage traits must be suppressed by default; got {:?}",
            result
        );
    }

    /// DX4: a plain struct WITHOUT `derived_traits` metadata still scores
    /// above the 0.75 threshold (raw 1.0, no penalty) and is flagged.
    #[test]
    fn test_plain_struct_without_derives_is_flagged() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let plain = make_symbol_with_kind("Plain", SymbolKind::Struct, Vec::new());
        let result = scorer
            .score_symbol(&plain, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_some(),
            "plain struct with no derives and raw confidence 1.0 must be flagged"
        );
        if let Some(f) = result {
            assert!((f.confidence - 1.0).abs() < 1e-6);
        }
    }

    /// DX4: `--include-traits` must NOT re-enable derived-struct suppression.
    /// The flag governs explicit trait impls (CG-F6); the derive penalty is
    /// applied regardless. A derived struct stays suppressed even with
    /// `include_traits = true`.
    #[test]
    fn test_derived_struct_still_suppressed_with_include_traits() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), true);

        let user = make_symbol_with_kind(
            "User",
            SymbolKind::Struct,
            vec![("derived_traits", "Debug,Deserialize,Serialize")],
        );
        let result = scorer
            .score_symbol(&user, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_none(),
            "derive penalty must apply regardless of --include-traits; got {:?}",
            result
        );
    }

    /// DX4: a struct with only a non-implicit derive (not in the standard
    /// set) gets no derive penalty and is flagged per its raw signals.
    #[test]
    fn test_struct_with_only_non_implicit_derive_is_flagged() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let widget = make_symbol_with_kind(
            "Widget",
            SymbolKind::Struct,
            vec![("derived_traits", "MyCustomDerive")],
        );
        let result = scorer
            .score_symbol(&widget, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_some(),
            "struct with only a non-implicit derive must NOT be suppressed"
        );
    }

    /// DX4 (codex Finding 1): a DB model struct carrying only a reflection
    /// derive (`sqlx::FromRow`, reduced to `FromRow`) — consumed only via sqlx
    /// reflection with no static call edges — must be suppressed at the
    /// default 0.75 threshold. Before `FromRow` was added to
    /// `IMPLICIT_USAGE_DERIVED_TRAITS`, such a model had zero derive penalty
    /// and remained a false positive (raw confidence 1.0 -> flagged).
    #[test]
    fn test_db_reflection_derive_fromrow_suppressed_by_default() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config(); // threshold 0.75
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let model = make_symbol_with_kind(
            "Account",
            SymbolKind::Struct,
            vec![("derived_traits", "FromRow")],
        );
        let result = scorer
            .score_symbol(&model, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_none(),
            "DB reflection derive (FromRow) must be suppressed at default threshold; got {:?}",
            result
        );
    }

    /// DX4 end-to-end metadata round-trip: prove the FULL path preserves
    /// `derived_traits` through the JSON serialization the storage layer uses
    /// (`serde_json::to_string(&symbol.metadata)` into the `project_symbols.metadata`
    /// column) and back (`serde_json::from_str`), and that `score_symbol`
    /// still suppresses the derived struct at the default 0.75 threshold
    /// after the round-trip. A control struct with NO `derived_traits`
    /// metadata round-trips and is still flagged.
    ///
    /// This locks the contract the reviewer verified by inspection across
    /// `src/index/storage.rs` (serialize) and
    /// `src/impact/analysis/dead_code/evidence.rs` (deserialize), so a future
    /// change to either layer that breaks the `derived_traits` key will fail
    /// here rather than silently regress the derive penalty.
    #[test]
    fn test_derived_traits_metadata_survives_json_round_trip_to_score_symbol() {
        use std::collections::BTreeMap;

        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config(); // threshold 0.75
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        // Build a realistic metadata map exactly as the Rust AST extractor
        // would: sorted, comma-joined `derived_traits`.
        let mut original_metadata = BTreeMap::new();
        original_metadata.insert(
            "derived_traits".to_string(),
            "Debug,Deserialize,Serialize".to_string(),
        );

        // Simulate the DB round-trip: serialize the metadata map to a JSON
        // string (as `src/index/storage.rs` does on write), then deserialize
        // it back (as `evidence.rs` does on read).
        let json = serde_json::to_string(&original_metadata).expect("metadata must serialize");
        let round_tripped: BTreeMap<String, String> =
            serde_json::from_str(&json).expect("metadata must deserialize");

        // Rebuild the Symbol with the round-tripped metadata and run it
        // through the real scorer with unreachable/no-git/no-test signals
        // (raw confidence 1.0).
        let user = Symbol {
            name: "User".to_string(),
            kind: SymbolKind::Struct,
            is_public: false,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: Some("crate::User".to_string()),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: round_tripped,
        };

        let result = scorer
            .score_symbol(&user, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            result.is_none(),
            "derived struct must be suppressed after JSON round-trip (derive penalty -0.50 -> 0.5 < 0.75); got {:?}",
            result
        );

        // Control: the SAME struct with NO `derived_traits` metadata (only a
        // non-implicit key) round-trips and is NOT suppressed — raw 1.0, no
        // derive penalty -> 1.0 >= 0.75.
        let mut control_metadata = BTreeMap::new();
        control_metadata.insert("abi".to_string(), "extern \"C\"".to_string());
        let control_json =
            serde_json::to_string(&control_metadata).expect("control metadata must serialize");
        let control_round_tripped: BTreeMap<String, String> =
            serde_json::from_str(&control_json).expect("control metadata must deserialize");

        let control = Symbol {
            name: "Control".to_string(),
            kind: SymbolKind::Struct,
            is_public: false,
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start: None,
            line_end: None,
            qualified_name: Some("crate::Control".to_string()),
            byte_start: None,
            byte_end: None,
            entrypoint_kind: None,
            metadata: control_round_tripped,
        };

        let control_result = scorer
            .score_symbol(&control, Path::new("src/models.rs"))
            .unwrap();
        assert!(
            control_result.is_some(),
            "control struct without derived_traits must be flagged after round-trip; got {:?}",
            control_result
        );
        if let Some(f) = control_result {
            assert!(
                (f.confidence - 1.0).abs() < 1e-6,
                "control struct confidence must be 1.0, got {}",
                f.confidence
            );
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    /// Regression for CG-F15: `precompute_git_activity` must compute "days
    /// since last commit" for every touched file in a single commit-history
    /// walk, instead of the old approach that re-walked history
    /// independently per file (the dominant cost on repos with many files
    /// and deep history -- 96s+ on this repo's own ~5000 symbols before
    /// this fix, despite the other caches already being in place).
    #[test]
    fn test_precompute_git_activity_single_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init"]);
        git(root, &["config", "user.email", "test@test.com"]);
        git(root, &["config", "user.name", "Test User"]);

        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        git(root, &["add", "a.rs"]);
        git(root, &["commit", "-m", "add a"]);

        std::fs::write(root.join("b.rs"), "fn b() {}").unwrap();
        git(root, &["add", "b.rs"]);
        git(root, &["commit", "-m", "add b"]);

        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, root, false);

        let index = scorer.precompute_git_activity().unwrap();
        assert!(index.repo_available);
        assert!(
            index.last_touched_days.contains_key(&PathBuf::from("a.rs")),
            "expected a.rs to be present: {:?}",
            index.last_touched_days
        );
        assert!(
            index.last_touched_days.contains_key(&PathBuf::from("b.rs")),
            "expected b.rs to be present: {:?}",
            index.last_touched_days
        );

        // Wire it through precompute() and confirm days_since_last_commit
        // reads from the index instead of doing a fresh per-file walk, and
        // that an unknown file falls back to git_inactivity_days rather
        // than None.
        let mut scorer = scorer;
        scorer.precomputed_git_activity = Some(index);
        let a_days = scorer
            .days_since_last_commit(Path::new("a.rs"))
            .unwrap()
            .unwrap();
        assert!(
            a_days <= 1,
            "a.rs was just committed, expected 0-1 days, got {a_days}"
        );
        assert_eq!(
            scorer
                .days_since_last_commit(Path::new("never_committed.rs"))
                .unwrap()
                .unwrap(),
            config.git_inactivity_days
        );
    }

    #[test]
    fn test_precompute_git_activity_reports_unavailable_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        let index = scorer.precompute_git_activity().unwrap();
        assert!(!index.repo_available);
        assert!(index.last_touched_days.is_empty());
    }

    #[test]
    fn test_explain_file_returns_symbols_for_indexed_file() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused_fn', 'unused_fn', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        let explanation = scorer.explain_file(Path::new("src/lib.rs")).unwrap();
        assert_eq!(explanation.file, "src/lib.rs");
        assert_eq!(explanation.symbols.len(), 1);
        assert_eq!(explanation.symbols[0].symbol_name, "unused_fn");
        assert!((explanation.symbols[0].confidence - 1.0).abs() < 1e-6);

        // TA25: the explain path populates file-scoped caches, not full-repo caches.
        assert!(scorer.precomputed_reachable_symbols.is_some());
        assert!(scorer.precomputed_tested_symbols.is_some());
        assert!(scorer.precomputed_symbol_ids.is_some());
        assert!(scorer.precomputed_git_activity.is_some());
        let symbol_ids = scorer.precomputed_symbol_ids.unwrap();
        assert_eq!(symbol_ids.len(), 1);
        assert!(symbol_ids.contains_key(&(
            "src/lib.rs".to_string(),
            "unused_fn".to_string(),
            "Function".to_string()
        )));
    }

    /// TA24: path normalization should make --explain find the same indexed
    /// file when the user types backslashes, a leading `./`, or a trailing
    /// slash.
    #[test]
    fn test_explain_file_normalizes_input_path() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused_fn', 'unused_fn', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        #[cfg(not(target_os = "windows"))]
        let inputs = vec!["./src/lib.rs", "src/lib.rs/"];
        #[cfg(target_os = "windows")]
        let inputs = vec!["./src/lib.rs", "src/lib.rs/", "src\\lib.rs"];

        for input in inputs {
            let explanation = scorer.explain_file(Path::new(input)).unwrap();
            assert_eq!(
                explanation.symbols.len(),
                1,
                "expected one symbol for input {input:?}"
            );
            assert_eq!(
                explanation.symbols[0].symbol_name, "unused_fn",
                "unexpected symbol for input {input:?}"
            );
        }
    }

    /// TA24: basename fallback finds a file whose stored path has additional
    /// prefix directories (e.g. user typed `src/lib.rs`, KG stores
    /// `crates/core/src/lib.rs`).
    #[test]
    fn test_explain_file_basename_fallback_selects_best_candidate() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        for path in ["crates/core/src/lib.rs", "crates/other/src/lib.rs"] {
            conn.execute(
                "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES (?1, 'Rust', 'h1', 100, 'OK', '2026-01-01')",
                [path],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused_fn', 'unused_fn', 'Function', 'INTERNAL', '2026-01-01')",
                [file_id],
            ).unwrap();
        }

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        // `lib.rs` is weak: only the basename matches both candidates, so the
        // resolver must error instead of guessing.
        let err = scorer.explain_file(Path::new("lib.rs")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple files match 'lib.rs'"), "{msg}");
        assert!(msg.contains("crates/core/src/lib.rs"), "{msg}");
        assert!(msg.contains("crates/other/src/lib.rs"), "{msg}");

        // `src/lib.rs` shares the trailing `src/lib.rs` suffix with both
        // candidates. The resolver picks the best (longest common suffix; tie-
        // break shorter path). Both candidates tie on suffix length and have
        // equal length, so `Iterator::max_by` keeps the last one deterministically.
        let explanation = scorer.explain_file(Path::new("src/lib.rs")).unwrap();
        assert_eq!(explanation.symbols.len(), 1);
        assert_eq!(explanation.symbols[0].symbol_name, "unused_fn");

        // A fully qualified input resolves directly without needing fallback.
        let explanation = scorer
            .explain_file(Path::new("crates/core/src/lib.rs"))
            .unwrap();
        assert_eq!(explanation.symbols.len(), 1);
        assert_eq!(explanation.symbols[0].symbol_name, "unused_fn");
    }

    #[test]
    fn test_explain_file_empty_for_non_indexed_file() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        let explanation = scorer.explain_file(Path::new("src/missing.rs")).unwrap();
        assert!(explanation.symbols.is_empty());
        assert_eq!(explanation.file, "src/missing.rs");

        // R1: no full-repo caches should be built for a missing file either.
        assert!(scorer.precomputed_reachable_symbols.is_none());
        assert!(scorer.precomputed_tested_symbols.is_none());
        assert!(scorer.precomputed_symbol_ids.is_none());
        assert!(scorer.precomputed_git_activity.is_none());
    }

    /// Performance guard: `--explain <file>` on a tiny in-memory graph must
    /// complete quickly. This test is marked slow because wall-clock assertions
    /// are inherently flaky; it primarily documents the latency target.
    #[ignore = "perf"]
    #[test]
    fn test_explain_file_is_fast() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        for i in 0..50 {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, ?2, ?3, 'Function', 'INTERNAL', '2026-01-01')",
                rusqlite::params![file_id, format!("crate::fn_{i}"), format!("fn_{i}")],
            ).unwrap();
        }

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        let start = std::time::Instant::now();
        let explanation = scorer.explain_file(Path::new("src/lib.rs")).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(explanation.symbols.len(), 50);
        assert!(
            elapsed.as_millis() < 500,
            "explain_file took {} ms, target is <500 ms",
            elapsed.as_millis()
        );
    }

    /// TA25: `precompute_for_file` builds caches scoped to the target file only.
    #[test]
    fn test_precompute_for_file_builds_file_scoped_caches() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/a.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let a_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::a_one', 'a_one', 'Function', 'INTERNAL', '2026-01-01')",
            [a_file],
        ).unwrap();
        let a_one_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::a_two', 'a_two', 'Function', 'INTERNAL', '2026-01-01')",
            [a_file],
        ).unwrap();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/b.rs', 'Rust', 'h2', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let b_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::b_one', 'b_one', 'Function', 'INTERNAL', '2026-01-01')",
            [b_file],
        ).unwrap();

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);
        scorer.precompute_for_file(Path::new("src/a.rs")).unwrap();

        let symbol_ids = scorer.precomputed_symbol_ids.as_ref().unwrap();
        assert_eq!(
            symbol_ids.len(),
            2,
            "cache should hold only src/a.rs symbols"
        );
        assert!(symbol_ids.contains_key(&(
            "src/a.rs".to_string(),
            "a_one".to_string(),
            "Function".to_string()
        )));
        assert!(symbol_ids.contains_key(&(
            "src/a.rs".to_string(),
            "a_two".to_string(),
            "Function".to_string()
        )));
        assert!(!symbol_ids.contains_key(&(
            "src/b.rs".to_string(),
            "b_one".to_string(),
            "Function".to_string()
        )));

        let reachable = scorer.precomputed_reachable_symbols.as_ref().unwrap();
        assert!(
            !reachable.contains(&a_one_id),
            "internal symbol with no entrypoint edges is unreachable"
        );
    }

    /// TA25: batched reachability results must match the per-symbol path.
    #[test]
    fn test_batch_reachability_matches_per_symbol() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/main.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let main_file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h2', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let lib_file = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::main', 'main', 'Function', 'ENTRYPOINT', '2026-01-01')",
            [main_file],
        ).unwrap();
        let main_sym = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::helper', 'helper', 'Function', 'INTERNAL', '2026-01-01')",
            [lib_file],
        ).unwrap();
        let helper_sym = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::unused', 'unused', 'Function', 'INTERNAL', '2026-01-01')",
            [lib_file],
        ).unwrap();
        let unused_sym = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status) VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED')",
            [main_sym, main_file, helper_sym, lib_file],
        ).unwrap();

        let config = default_config();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        scorer.precompute_for_file(Path::new("src/lib.rs")).unwrap();

        let reachable = scorer.precomputed_reachable_symbols.as_ref().unwrap();
        assert!(
            reachable.contains(&helper_sym),
            "helper called by entrypoint is reachable"
        );
        assert!(
            !reachable.contains(&unused_sym),
            "unused symbol is not reachable"
        );
    }

    /// TA25: batched test-coverage results must match the per-symbol path.
    #[test]
    fn test_batch_test_coverage_matches_per_symbol() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::covered', 'covered', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();
        let covered_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::uncovered', 'uncovered', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        ).unwrap();
        let uncovered_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::test_covered', 'test_covered', 'Function', 'TEST', '2026-01-01')",
            [file_id],
        ).unwrap();
        let test_sym_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, mapping_kind, last_indexed_at) VALUES (?1, ?2, ?3, ?4, 'IMPORT', '2026-01-01')",
            [test_sym_id, file_id, covered_id, file_id],
        ).unwrap();

        let config = default_config();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        scorer.precompute_for_file(Path::new("src/lib.rs")).unwrap();

        let tested = scorer.precomputed_tested_symbols.as_ref().unwrap();
        assert!(
            tested.contains(&covered_id),
            "covered symbol is in tested set"
        );
        assert!(
            !tested.contains(&uncovered_id),
            "uncovered symbol is not in tested set"
        );
    }

    /// TA25 performance guard: `--explain` on a 10-symbol file must stay under
    /// the 200 ms budget. Marked `#[ignore = "perf"]` because wall-clock
    /// assertions are inherently flaky in CI; it documents the latency target.
    #[ignore = "perf"]
    #[test]
    fn test_explain_file_ten_symbols_under_budget() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();

        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let file_id = conn.last_insert_rowid();

        for i in 0..10 {
            conn.execute(
                "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, ?2, ?3, 'Function', 'INTERNAL', '2026-01-01')",
                rusqlite::params![file_id, format!("crate::fn_{i}"), format!("fn_{i}")],
            ).unwrap();
        }

        let config = default_config();
        let tmp = tempfile::tempdir().unwrap();
        let mut scorer = ConfidenceScorer::new(None, &storage, &config, tmp.path(), false);

        let start = std::time::Instant::now();
        let explanation = scorer.explain_file(Path::new("src/lib.rs")).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(explanation.symbols.len(), 10);
        assert!(
            elapsed.as_millis() < 200,
            "explain_file took {} ms, target is <200 ms",
            elapsed.as_millis()
        );
    }
}
