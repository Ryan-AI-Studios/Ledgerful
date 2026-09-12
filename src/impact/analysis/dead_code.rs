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
    /// When `false` (CLI default), `is_test_path` files are omitted.
    pub(super) include_tests: bool,
    /// When `false` (CLI default), vendored trees are omitted.
    pub(super) include_vendor: bool,
    pub(super) git_activity_cache:
        std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, Option<u32>>>,
    pub(super) omitted_test_paths: std::cell::RefCell<HashSet<String>>,
    pub(super) omitted_vendor_paths: std::cell::RefCell<HashSet<String>>,
    pub(super) extension_has_edges: std::cell::RefCell<Option<HashSet<String>>>,
    pub(super) test_mapping_nonempty: std::cell::RefCell<Option<bool>>,
    pub(super) entrypoints_present: std::cell::RefCell<Option<bool>>,
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
        _include_traits: bool,
    ) -> Self {
        Self {
            cozo,
            storage,
            config,
            repo_path,
            include_tests: false,
            include_vendor: false,
            git_activity_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            omitted_test_paths: std::cell::RefCell::new(HashSet::new()),
            omitted_vendor_paths: std::cell::RefCell::new(HashSet::new()),
            extension_has_edges: std::cell::RefCell::new(None),
            test_mapping_nonempty: std::cell::RefCell::new(None),
            entrypoints_present: std::cell::RefCell::new(None),
            precomputed_reachable_symbols: None,
            precomputed_tested_symbols: None,
            precomputed_symbol_ids: None,
            precomputed_git_activity: None,
        }
    }

    /// CLI/MCP path-omit override. Defaults from [`Self::new`] omit test and
    /// vendor paths (CLI + `scan --impact` enrichment). MCP passes `(true, true)`.
    pub fn with_path_include(mut self, include_tests: bool, include_vendor: bool) -> Self {
        self.include_tests = include_tests;
        self.include_vendor = include_vendor;
        self
    }

    pub fn omitted_test_path_count(&self) -> usize {
        self.omitted_test_paths.borrow().len()
    }

    pub fn omitted_vendor_path_count(&self) -> usize {
        self.omitted_vendor_paths.borrow().len()
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
        assert_eq!(score, Some(0.0));

        let unused = make_symbol("unused", Some("crate::unused"), None);
        let score = scorer
            .reachability_score(&unused, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, Some(1.0));
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
        let main_sym = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h2', 100, 'OK', '2026-01-01')",
            [],
        ).unwrap();
        let lib_file = conn.last_insert_rowid();
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
        let scorer = ConfidenceScorer::new(Some(&cozo), &storage, &config, Path::new("."), false);

        let helper = make_symbol("helper", Some("crate::helper"), None);
        let score = scorer
            .reachability_score(&helper, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, Some(0.0));

        let unused = make_symbol("unused", Some("crate::unused"), None);
        let score = scorer
            .reachability_score(&unused, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, Some(1.0));
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
        assert_eq!(score, None);
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
        assert_eq!(score, Some(0.0));
    }

    #[test]
    fn test_blend_expected_value() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);

        let confidence = scorer.blend(Some(1.0), 0.5, Some(0.0));
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
        let confidence = scorer.blend(Some(1.0), 1.0, Some(1.0));
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn score_symbol_skips_non_callable_kinds() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        for kind in [
            SymbolKind::Type,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Module,
        ] {
            let symbol = Symbol {
                name: "Eq".to_string(),
                kind: kind.clone(),
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
            assert!(
                scorer
                    .score_symbol(&symbol, Path::new("src/lib.rs"))
                    .unwrap()
                    .is_none(),
                "{kind:?} is not Function/Method"
            );
        }
    }

    fn seed_rs_edge_and_mapping(conn: &rusqlite::Connection) {
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/entry.rs', 'Rust', 'hedge', 50, 'OK', '2026-01-01')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::entry_main', 'entry_main', 'Function', 'ENTRYPOINT', '2026-01-01')",
            [file_id],
        )
        .unwrap();
        let main_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::live_helper', 'live_helper', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        )
        .unwrap();
        let helper_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id, call_kind, resolution_status) VALUES (?1, ?2, ?3, ?4, 'DIRECT', 'RESOLVED')",
            [main_id, file_id, helper_id, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO test_mapping (test_symbol_id, test_file_id, tested_symbol_id, tested_file_id, mapping_kind, last_indexed_at) VALUES (?1, ?2, ?3, ?4, 'IMPORT', '2026-01-01')",
            [main_id, file_id, helper_id, file_id],
        )
        .unwrap();
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
        seed_rs_edge_and_mapping(conn);

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
        seed_rs_edge_and_mapping(conn);

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
        seed_rs_edge_and_mapping(conn);

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
        seed_rs_edge_and_mapping(conn);

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
        seed_rs_edge_and_mapping(conn);

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

    #[test]
    fn score_symbol_skips_bin_main() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_rs_edge_and_mapping(conn);
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let main_fn = make_symbol("main", Some("main"), None);
        for path in [
            "mcp-server/bin/x.js",
            "mcp-server\\bin\\x.js",
            "src/main.rs",
        ] {
            assert!(
                scorer
                    .score_symbol(&main_fn, Path::new(path))
                    .unwrap()
                    .is_none(),
                "expected skip for {path}"
            );
        }
    }

    #[test]
    fn score_symbol_keeps_isolated_function_when_language_has_edges() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_rs_edge_and_mapping(conn);
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/gen_1.rs', 'Rust', 'hiso', 40, 'OK', '2026-01-01')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::isolated', 'isolated', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        )
        .unwrap();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let isolated = make_symbol("isolated", Some("crate::isolated"), None);
        let finding = scorer
            .score_symbol(&isolated, Path::new("src/gen_1.rs"))
            .unwrap()
            .expect("isolated Function must remain a finding");
        assert!(finding.factors.iter().any(|f| matches!(
            f,
            crate::impact::packet::ConfidenceFactor::UnreachableFromEntrypoints
        )));
    }

    #[test]
    fn reachability_missing_symbol_id_is_unknown() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_rs_edge_and_mapping(conn);
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let ghost = make_symbol("ghost", Some("crate::ghost"), None);
        let score = scorer
            .reachability_score(&ghost, Path::new("src/missing.rs"))
            .unwrap();
        assert_eq!(score, None);
        let finding = scorer
            .score_symbol(&ghost, Path::new("src/missing.rs"))
            .unwrap();
        if let Some(f) = finding {
            assert!(!f.factors.iter().any(|fac| matches!(
                fac,
                crate::impact::packet::ConfidenceFactor::UnreachableFromEntrypoints
            )));
        }
        let none_blend = scorer.blend(None, 1.0, None);
        assert!((none_blend - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn reachability_unknown_when_extension_has_no_edges() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_rs_edge_and_mapping(conn);
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('pkg/a.js', 'JavaScript', 'hjs', 20, 'OK', '2026-01-01')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'cacheRoot', 'cacheRoot', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        )
        .unwrap();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let js_fn = make_symbol("cacheRoot", Some("cacheRoot"), None);
        assert_eq!(
            scorer
                .reachability_score(&js_fn, Path::new("pkg/a.js"))
                .unwrap(),
            None
        );
    }

    #[test]
    fn no_test_coverage_unknown_when_mapping_table_empty() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'Rust', 'h1', 100, 'OK', '2026-01-01')",
            [],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, 'crate::foo', 'foo', 'Function', 'INTERNAL', '2026-01-01')",
            [file_id],
        )
        .unwrap();
        let foo_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO embeddings (entity_id, entity_type, model, vector, created_at) VALUES (?1, 'symbol', 'test', x'00', '2026-01-01')",
            [foo_id.to_string()],
        )
        .ok();
        let config = default_config();
        let scorer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let symbol = make_symbol("foo", Some("crate::foo"), None);
        let score = scorer
            .test_coverage_score(&symbol, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(score, None);
        if let Some(finding) = scorer
            .score_symbol(&symbol, Path::new("src/lib.rs"))
            .unwrap()
        {
            assert!(!finding
                .factors
                .iter()
                .any(|f| matches!(f, crate::impact::packet::ConfidenceFactor::NoTestCoverage)));
        }
    }

    fn seed_isolated_fn(conn: &rusqlite::Connection, path: &str, name: &str) {
        seed_rs_edge_and_mapping(conn);
        conn.execute(
            "INSERT INTO project_files (file_path, language, content_hash, file_size, parse_status, last_indexed_at) VALUES (?1, 'Rust', 'hiso', 40, 'OK', '2026-01-01')",
            [path],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_symbols (file_id, qualified_name, symbol_name, symbol_kind, entrypoint_kind, last_indexed_at) VALUES (?1, ?2, ?2, 'Function', 'INTERNAL', '2026-01-01')",
            rusqlite::params![file_id, name],
        )
        .unwrap();
    }

    #[test]
    fn dead_code_include_tests_restores_test_path() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_isolated_fn(conn, "tests/foo.rs", "test_only");
        let config = default_config();
        let default = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let included = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false)
            .with_path_include(true, false);
        let symbol = make_symbol("test_only", Some("test_only"), None);
        assert!(default
            .score_symbol(&symbol, Path::new("tests/foo.rs"))
            .unwrap()
            .is_none());
        assert!(included
            .score_symbol(&symbol, Path::new("tests/foo.rs"))
            .unwrap()
            .is_some());
        let mut explainer = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let explanation = explainer.explain_file(Path::new("tests/foo.rs")).unwrap();
        assert!(explanation
            .symbols
            .iter()
            .any(|s| s.symbol_name == "test_only"));
    }

    #[test]
    fn dead_code_include_vendor_restores_vendor_path() {
        let (storage, _cozo) = in_memory_storage_with_cozo();
        let conn = storage.get_connection();
        seed_isolated_fn(conn, "vendor/x.rs", "vendored");
        let config = default_config();
        let default = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false);
        let included = ConfidenceScorer::new(None, &storage, &config, Path::new("."), false)
            .with_path_include(false, true);
        let symbol = make_symbol("vendored", Some("vendored"), None);
        assert!(default
            .score_symbol(&symbol, Path::new("vendor/x.rs"))
            .unwrap()
            .is_none());
        assert!(included
            .score_symbol(&symbol, Path::new("vendor/x.rs"))
            .unwrap()
            .is_some());
    }
}
