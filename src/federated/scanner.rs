use crate::federated::schema::FederatedSchema;
use crate::index::languages::{Language, parse_symbols};
use crate::index::references::extract_import_export;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::panic;
use std::time::SystemTime;
use tracing::{debug, info, warn};

pub const DEFAULT_SIBLING_LIMIT: usize = 20;

/// Stateful matching utility that caches compiled word-boundary regexes.
/// This prevents redundant regex compilation and string allocations when
/// checking the same public interface symbols against many files.
pub struct SymbolMatcher {
    cache: HashMap<String, Option<Regex>>,
}

impl SymbolMatcher {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    /// Check whether a symbol name appears as a whole-word match in the given content.
    /// Uses a cached word-boundary regex to avoid false positives.
    /// Falls back to exact substring match if the regex fails to compile.
    pub fn matches(&mut self, symbol: &str, content: &str) -> bool {
        if symbol.is_empty() {
            return false;
        }

        let re_opt = self.cache.entry(symbol.to_string()).or_insert_with(|| {
            // Escape any regex metacharacters in the symbol name.
            let escaped = regex::escape(symbol);

            // Use word boundary (\b) if the edge character is a word character,
            // otherwise use a non-word boundary (\B) to ensure we don't match
            // when adjacent to a word character.
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let start = if symbol.chars().next().is_some_and(is_word) {
                r"\b"
            } else {
                r"\B"
            };
            let end = if symbol.chars().last().is_some_and(is_word) {
                r"\b"
            } else {
                r"\B"
            };

            let pattern = format!("{}{}{}", start, escaped, end);
            match Regex::new(&pattern) {
                Ok(re) => Some(re),
                Err(_) => {
                    warn!(
                        "Failed to compile word-boundary regex for symbol '{}', falling back to substring match",
                        symbol
                    );
                    None
                }
            }
        });

        match re_opt {
            Some(re) => re.is_match(content),
            None => content.contains(symbol),
        }
    }
}

impl Default for SymbolMatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Check whether a symbol is imported or referenced via the file's import list.
/// This is a more precise match than word-boundary regex: if the symbol's module/crate
/// appears in the file's imports, it's a definitive dependency.
fn symbol_imported(symbol: &str, path: &Utf8Path, content: &str) -> bool {
    if let Ok(Some(import_export)) = extract_import_export(path.as_std_path(), content) {
        // Check if the symbol name or a module path containing it appears in imports.
        for import in &import_export.imported_from {
            if import.contains(symbol) {
                return true;
            }
        }
        for export in &import_export.exported_symbols {
            if export == symbol {
                return true;
            }
        }
    }
    false
}

pub struct FederatedScanner {
    root: Utf8PathBuf,
    sibling_limit: usize,
    /// TA31 R2: opt-in flag for auto-regenerating a stale/missing sibling
    /// `schema.json` by shelling out to `ledgerful federate export` against
    /// the sibling's own root. Defaults to `false` (see `new()`) — this is
    /// a deliberate safety boundary, not an oversight. `scan_siblings()` is
    /// also called synchronously from the `GET /api/projects` HTTP handler
    /// (`src/commands/web/server.rs`) and from `src/federated/refresh.rs`;
    /// spawning N blocking child processes (each doing a full tree-sitter
    /// parse + git history walk) inside either of those call paths would be
    /// a latency/DoS hazard. Only `execute_federate_scan` in
    /// `src/commands/federate.rs` opts in via `with_auto_sync`, gated by the
    /// `[federation] auto_sync_siblings` config flag.
    auto_sync: bool,
}

impl FederatedScanner {
    pub fn new(root: Utf8PathBuf) -> Self {
        Self {
            root,
            sibling_limit: DEFAULT_SIBLING_LIMIT,
            auto_sync: false,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.sibling_limit = limit;
        self
    }

    /// TA31 R2: opt into auto-regenerating stale/missing sibling
    /// `schema.json` files during `scan_siblings()`. See the `auto_sync`
    /// field doc for why this defaults to `false` and must stay
    /// caller-gated rather than always-on.
    pub fn with_auto_sync(mut self, enabled: bool) -> Self {
        self.auto_sync = enabled;
        self
    }

    /// Discovers sibling repositories and their schemas.
    ///
    /// Returns discovered schemas and a list of deterministic top-level
    /// warnings. Each discovered sibling carries its own
    /// `Vec<String>` of per-sibling validation warnings (TA31 R1): a
    /// sibling whose schema has only data-quality problems (empty
    /// `repo_name`/`entity`/`tx_id`/interface symbol/file — see
    /// `FederatedSchema::validation_issues()`) is still discovered, with
    /// those problems surfaced as warnings instead of a hard skip. A
    /// sibling whose schema has a *hard* error (path traversal, absolute
    /// path, or an incompatible `schema_version`) is still skipped
    /// entirely — that security/compat boundary is unchanged.
    #[allow(clippy::type_complexity)]
    pub fn scan_siblings(
        &self,
    ) -> Result<(
        Vec<(Utf8PathBuf, FederatedSchema, Vec<String>)>,
        Vec<String>,
    )> {
        let parent = match self.root.parent() {
            Some(p) => p,
            None => return Ok((Vec::new(), Vec::new())),
        };

        // Canonicalize parent for secure path comparison
        let canonical_parent = parent.canonicalize_utf8().into_diagnostic()?;

        let mut discovered = Vec::new();
        let mut warnings = Vec::new();
        let entries = fs::read_dir(parent).into_diagnostic()?;

        for entry in entries {
            if discovered.len() >= self.sibling_limit {
                warnings.push(format!(
                    "Reached sibling limit ({}). Some siblings may have been skipped.",
                    self.sibling_limit
                ));
                break;
            }

            let entry = entry.into_diagnostic()?;
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|_| miette::miette!("Invalid UTF-8 path: {:?}", entry.path()))?;

            // Security: Skip symlinks to prevent escapes
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).into_diagnostic(),
            };
            if metadata.is_symlink() {
                continue;
            }

            // Skip current repo
            let is_root = if let (Ok(p1), Ok(p2)) =
                (path.canonicalize_utf8(), self.root.canonicalize_utf8())
            {
                p1.as_str().to_lowercase() == p2.as_str().to_lowercase()
            } else {
                path.as_str().to_lowercase() == self.root.as_str().to_lowercase()
            };
            if is_root {
                continue;
            }

            if metadata.is_dir() {
                // Path Confinement Check
                let canonical_path = match path.canonicalize_utf8() {
                    Ok(p) => p,
                    Err(_) => {
                        warnings.push(format!("Failed to canonicalize path: {}", path));
                        continue;
                    }
                };

                // Verify the resolved path is exactly parent.join(sibling_name)
                // and resides exactly one level above the local repository root.
                if canonical_path.parent() != Some(&canonical_parent) {
                    warnings.push(format!(
                        "Security violation: Sibling path escapes discovery root: {}",
                        path
                    ));
                    continue;
                }

                // Task 1.1: Explicitly check for .ledgerful/ directory
                let cg_dir = path.join(".ledgerful");
                if !cg_dir.is_dir() {
                    continue;
                }

                // Check for schema in .ledgerful/state/schema.json (current)
                // or .ledgerful/schema.json (legacy fallback)
                let schema_path = cg_dir.join("state").join("schema.json");
                let legacy_path = cg_dir.join("schema.json");

                let final_path = if schema_path.exists() {
                    Some(schema_path.clone())
                } else if legacy_path.exists() {
                    Some(legacy_path.clone())
                } else {
                    None
                };

                // TA31 R2: when auto-sync is enabled (opt-in, see the
                // `auto_sync` field doc), check whether this sibling's
                // schema is missing or stale and, if so, synchronously
                // regenerate it via `ledgerful federate export` run against
                // the sibling's own root — never the current repo's root.
                // This runs ONE sibling at a time, inline in this loop
                // iteration, before the existing load+validate+discover
                // logic below — never concurrently (see `run_federate_export`
                // doc for the concurrency-bound rationale).
                if self.auto_sync {
                    let generated_at = final_path
                        .as_ref()
                        .and_then(|p| self.load_schema(p).ok())
                        .map(|s| s.generated_at)
                        .unwrap_or_default();
                    let commit_mtime = last_commit_mtime(&path.join(".git"));

                    if needs_sync(final_path.is_some(), &generated_at, commit_mtime) {
                        let db_path = path.join(".ledgerful").join("state").join("ledger.db");
                        if !db_path.exists() {
                            warn!(
                                "Skipping auto-sync for sibling at {}: no ledger.db found (run 'ledgerful init' there first)",
                                path
                            );
                        } else {
                            match run_federate_export(&path) {
                                Ok(()) => {
                                    info!("Auto-synced schema.json for sibling at {}", path);
                                }
                                Err(e) => {
                                    warn!("Auto-sync failed for sibling at {}: {:?}", path, e);
                                }
                            }
                        }
                    }
                }

                // Re-resolve: a successful auto-sync above may have just
                // created `schema.json` where none existed before.
                let final_path = if schema_path.exists() {
                    Some(schema_path)
                } else if legacy_path.exists() {
                    Some(legacy_path)
                } else {
                    None
                };

                if let Some(sp) = final_path {
                    match self.load_schema(&sp) {
                        Ok(schema) => {
                            let (hard_errors, sibling_warnings) = schema.validation_issues();
                            if !hard_errors.is_empty() {
                                // TA31 R1: security/compat violations (path
                                // traversal, absolute paths, incompatible
                                // schema_version) are still a hard skip.
                                // Task 1.4: Downgrade to DEBUG to reduce
                                // noise during discovery.
                                debug!("Invalid schema at {}: {}", path, hard_errors.join("; "));
                            } else {
                                // TA31 R1: data-quality issues (empty
                                // entity/repo_name/symbol/file/tx_id) no
                                // longer hard-skip the sibling — surface
                                // them as per-sibling warnings instead.
                                discovered.push((path, schema, sibling_warnings));
                            }
                        }
                        Err(e) => {
                            // Only warn if the schema file exists but is corrupted/unreadable
                            warnings.push(format!("Failed to load schema from {}: {}", path, e));
                            warn!("Failed to load schema from {}: {:?}", sp, e);
                        }
                    }
                }
            }
        }

        // Engineering standard: deterministic sorting by repo name
        discovered.sort_by(|a, b| a.1.repo_name.cmp(&b.1.repo_name));
        warnings.sort();

        Ok((discovered, warnings))
    }

    fn load_schema(&self, path: &Utf8Path) -> Result<FederatedSchema> {
        let content = fs::read_to_string(path).into_diagnostic()?;

        // JSON Safety: Wrap in catch_unwind to prevent panics from malformed JSON
        let result = panic::catch_unwind(|| serde_json::from_str::<FederatedSchema>(&content));

        match result {
            Ok(serde_result) => serde_result.into_diagnostic(),
            Err(_) => Err(miette::miette!("Panic occurred while parsing JSON schema")),
        }
    }

    pub fn discover_dependencies(
        &self,
        local_packet: &crate::impact::packet::ImpactPacket,
        _sibling_name: &str,
        sibling_schema: &FederatedSchema,
    ) -> Result<Vec<(String, String)>> {
        let mut edges = self.discover_dependencies_in_current_repo(sibling_schema)?;
        let mut matcher = SymbolMatcher::new();

        for interface in &sibling_schema.public_interfaces {
            let symbol_to_find = &interface.symbol;

            for change in &local_packet.changes {
                if let Some(local_symbols) = &change.symbols {
                    let Some(utf8_path) = Utf8Path::from_path(&change.path) else {
                        continue;
                    };
                    let full_path = self.root.join(utf8_path);
                    let file_content = match fs::read_to_string(&full_path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    // Use import-based matching first (definitive), then word-boundary
                    // regex (heuristic). This avoids false positives like "api"
                    // matching "map_item".
                    let matches_import = symbol_imported(symbol_to_find, utf8_path, &file_content);
                    let matches_word = matcher.matches(symbol_to_find, &file_content);
                    if matches_import || matches_word {
                        for local_symbol in local_symbols {
                            edges.push((local_symbol.name.clone(), symbol_to_find.clone()));
                        }
                    }
                }
            }
        }

        edges.sort();
        edges.dedup();
        Ok(edges)
    }

    pub fn discover_dependencies_in_current_repo(
        &self,
        sibling_schema: &FederatedSchema,
    ) -> Result<Vec<(String, String)>> {
        let mut edges = Vec::new();
        let mut matcher = SymbolMatcher::new();
        self.scan_dependency_dir(&self.root, sibling_schema, &mut edges, &mut matcher)?;
        edges.sort();
        edges.dedup();
        Ok(edges)
    }

    fn scan_dependency_dir(
        &self,
        dir: &Utf8Path,
        sibling_schema: &FederatedSchema,
        edges: &mut Vec<(String, String)>,
        matcher: &mut SymbolMatcher,
    ) -> Result<()> {
        for entry in fs::read_dir(dir).into_diagnostic()? {
            let entry = entry.into_diagnostic()?;
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|_| miette::miette!("Invalid UTF-8 path: {:?}", entry.path()))?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();

            if path.is_dir() {
                if matches!(file_name.as_ref(), ".git" | ".ledgerful" | "target") {
                    continue;
                }
                self.scan_dependency_dir(&path, sibling_schema, edges, matcher)?;
                continue;
            }

            let Some(extension) = path.extension() else {
                continue;
            };
            if Language::from_extension(extension).is_none() {
                continue;
            }

            let Ok(file_content) = fs::read_to_string(&path) else {
                continue;
            };
            let relative_path = path.strip_prefix(&self.root).unwrap_or(&path);
            let local_symbols =
                parse_symbols(relative_path.as_std_path(), &file_content).unwrap_or_default();

            let local_symbol_names = local_symbols
                .unwrap_or_default()
                .into_iter()
                .map(|symbol| symbol.name)
                .collect::<Vec<_>>();

            if local_symbol_names.is_empty() {
                continue;
            }

            for interface in &sibling_schema.public_interfaces {
                let symbol_to_find = &interface.symbol;
                let matches_import = symbol_imported(symbol_to_find, relative_path, &file_content);
                let matches_word = matcher.matches(symbol_to_find, &file_content);
                if matches_import || matches_word {
                    for local_symbol in &local_symbol_names {
                        edges.push((local_symbol.clone(), symbol_to_find.clone()));
                    }
                }
            }
        }

        Ok(())
    }
}

/// TA31 R2 staleness fast-path: resolves the mtime of the sibling's most
/// recent commit without spawning git machinery. `.git/HEAD` is usually a
/// symbolic ref (`ref: refs/heads/<branch>`) and does NOT change mtime on
/// every commit — only on checkout/branch-switch. The file that actually
/// changes on each commit is `.git/refs/heads/<branch>`, so when HEAD is
/// symbolic this resolves through to that ref file's mtime. Falls back to
/// stat-ing `.git/HEAD` itself when the ref file doesn't exist (e.g.
/// packed-refs) or HEAD is detached (a raw SHA, no `ref: ` prefix).
fn last_commit_mtime(sibling_git_dir: &Utf8Path) -> Option<SystemTime> {
    let head_path = sibling_git_dir.join("HEAD");
    let head_contents = fs::read_to_string(&head_path).ok()?;
    let head_contents = head_contents.trim();

    if let Some(ref_path) = head_contents.strip_prefix("ref: ") {
        let ref_file = sibling_git_dir.join(ref_path);
        if let Ok(metadata) = fs::metadata(&ref_file) {
            return metadata.modified().ok();
        }
        // Packed-refs case (ref file doesn't exist on disk): fall back to
        // HEAD's own mtime rather than parsing packed-refs — out of scope
        // for this cheap fast-path; a false "needs sync" here just costs an
        // extra (still bounded, sequential) export, not correctness.
    }

    fs::metadata(&head_path).ok()?.modified().ok()
}

/// TA31 R2 staleness fast-path: decides whether a sibling's `schema.json`
/// needs to be (re)generated, without touching git history.
///
/// Returns `true` when:
/// - `schema_exists` is `false` (no schema.json at all — the 12/14-siblings
///   case), OR
/// - `generated_at` is empty (legacy/unstamped schema.json predating TA31
///   R4 — we cannot prove freshness, so sync rather than implementing a
///   heavier real git-log fallback; that's out of scope/over-engineering
///   for this track), OR
/// - `commit_mtime` is `Some(t)` where `t` is strictly newer than
///   `generated_at` (parsed via `chrono::DateTime::parse_from_rfc3339`) —
///   the sibling has new commits since the schema was last exported.
///
/// Returns `false` when `commit_mtime` is older than or equal to
/// `generated_at` (no new commits since the last export), or when
/// `commit_mtime` is `None` (no git info available) and `generated_at`
/// is present and parses — nothing proves staleness, so don't sync.
///
/// Returns `true` when `generated_at` is non-empty but fails to parse as
/// RFC 3339 (a corrupt/foreign timestamp is treated the same as "can't
/// prove freshness, so sync" — matching the empty-string case, per
/// TA31-R2-001 resolution).
fn needs_sync(schema_exists: bool, generated_at: &str, commit_mtime: Option<SystemTime>) -> bool {
    if !schema_exists {
        return true;
    }
    if generated_at.trim().is_empty() {
        return true;
    }

    let Ok(generated_at_parsed) = chrono::DateTime::parse_from_rfc3339(generated_at) else {
        return true;
    };
    let generated_at_system: SystemTime = generated_at_parsed.into();

    match commit_mtime {
        Some(t) => t > generated_at_system,
        None => false,
    }
}

/// TA31 R2: regenerates `schema.json` for a sibling by shelling out to
/// `ledgerful federate export` with `current_dir` set to the sibling's own
/// root. This is the established pattern for "run the compiled binary
/// against a different directory" already used by
/// `src/commands/schedule.rs`'s `resolve_ledgerful_binary` +
/// `Command::new(..).current_dir(..)` (that helper is `pub(crate)`-free and
/// duplicated here rather than made `pub` across modules — several other
/// modules already duplicate the same `current_exe()` idiom, so this
/// follows existing convention rather than introducing a new one).
///
/// MUST be called synchronously, one sibling at a time, from a plain `for`
/// loop in `scan_siblings()` — never via threads, `tokio::spawn`, rayon, or
/// any concurrent iterator. Spawning N of these concurrently would each
/// open a SQLite DB, spin up Tree-Sitter parsers, and walk git history at
/// the same time, which can spike CPU/memory enough to be a DoS hazard on
/// the developer's own machine (the spec's explicit "Concurrency bound"
/// requirement).
fn run_federate_export(sibling_root: &Utf8Path) -> Result<()> {
    let current_exe = std::env::current_exe().into_diagnostic()?;
    let status = std::process::Command::new(current_exe)
        .args(["federate", "export"])
        .current_dir(sibling_root.as_std_path())
        .status()
        .into_diagnostic()?;

    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!(
            "ledgerful federate export failed for sibling at {} (exit status: {:?})",
            sibling_root,
            status.code()
        ))
    }
}

#[cfg(test)]
mod dependency_tests {
    use super::*;
    use crate::federated::schema::PublicInterface;
    use crate::index::symbols::SymbolKind;
    use tempfile::tempdir;

    #[test]
    fn discovers_dependencies_outside_latest_packet() {
        let tmp = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::write(
            root.join("main.rs"),
            "pub fn local_handler() { let _ = remote_api(); }",
        )
        .unwrap();

        let schema = FederatedSchema::new(
            "sibling".to_string(),
            vec![PublicInterface {
                symbol: "remote_api".to_string(),
                file: "src/lib.rs".to_string(),
                kind: SymbolKind::Function,
            }],
        );

        let scanner = FederatedScanner::new(root);
        let dependencies = scanner
            .discover_dependencies_in_current_repo(&schema)
            .unwrap();

        assert_eq!(
            dependencies,
            vec![("local_handler".to_string(), "remote_api".to_string())]
        );
    }

    #[test]
    fn no_false_positive_substring_match() {
        // "api" should NOT match "map_item" — the word boundary prevents it.
        let tmp = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::write(root.join("main.rs"), "pub fn map_item() { }").unwrap();

        let schema = FederatedSchema::new(
            "sibling".to_string(),
            vec![PublicInterface {
                symbol: "api".to_string(),
                file: "src/lib.rs".to_string(),
                kind: SymbolKind::Function,
            }],
        );

        let scanner = FederatedScanner::new(root);
        let dependencies = scanner
            .discover_dependencies_in_current_repo(&schema)
            .unwrap();

        assert!(
            dependencies.is_empty(),
            "Expected no dependencies, got {:?}",
            dependencies
        );
    }

    #[test]
    fn word_boundary_match_still_works() {
        // "handler" should match "let result = handler(request);" as a whole word.
        let tmp = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::write(
            root.join("main.rs"),
            "pub fn local_fn() { let result = handler(request); }",
        )
        .unwrap();

        let schema = FederatedSchema::new(
            "sibling".to_string(),
            vec![PublicInterface {
                symbol: "handler".to_string(),
                file: "src/lib.rs".to_string(),
                kind: SymbolKind::Function,
            }],
        );

        let scanner = FederatedScanner::new(root);
        let dependencies = scanner
            .discover_dependencies_in_current_repo(&schema)
            .unwrap();

        assert!(
            !dependencies.is_empty(),
            "Expected to find 'handler' as a whole-word match"
        );
    }

    #[test]
    fn symbol_matches_content_unit_tests() {
        let mut matcher = SymbolMatcher::new();
        // Exact word match
        assert!(matcher.matches("handler", "let result = handler(request);"));
        assert!(matcher.matches("api", "use crate::api;"));

        // False positives prevented: substring should NOT match
        assert!(!matcher.matches("api", "map_item"));
        assert!(!matcher.matches("api", "the_capabilities"));
        assert!(!matcher.matches("set", "upsetting"));

        // Should match identifiers at word boundaries
        assert!(matcher.matches("remote_api", "let x = remote_api();"));
        assert!(matcher.matches("RemoteApi", "use crate::RemoteApi;"));

        // Metacharacters should be escaped and matched correctly
        assert!(matcher.matches("api.v1", "let x = api.v1();"));
        assert!(!matcher.matches("api.v1", "api_v1"));
        assert!(matcher.matches("search(fn)", "call search(fn) now"));

        // Fallback behavior: manual insertion of None to simulate regex failure
        matcher.cache.insert("fallback_sym".to_string(), None);
        assert!(matcher.matches("fallback_sym", "this contains fallback_sym"));
        assert!(!matcher.matches("fallback_sym", "other content"));

        // Edge cases: empty content or symbols
        assert!(!matcher.matches("symbol", ""));
        assert!(!matcher.matches("", "content"));
    }
}

/// TA31 R1: `scan_siblings()` should surface data-quality problems (e.g.
/// an empty ledger `entity` — the AI-Brains real-world case) as
/// per-sibling warnings instead of hard-skipping the sibling, while
/// still hard-rejecting security violations (path traversal, absolute
/// paths) and incompatible `schema_version`s.
#[cfg(test)]
mod scan_siblings_tests {
    use super::*;
    use crate::federated::schema::{FederatedLedgerEntry, PublicInterface};
    use crate::index::symbols::SymbolKind;
    use crate::ledger::types::{Category, ChangeType, EntryType};
    use tempfile::tempdir;

    /// Writes `.ledgerful/state/schema.json` under `sibling_dir`,
    /// initializes a bare `.git` marker so the directory looks like a
    /// real sibling project, and returns nothing — the caller drives
    /// `FederatedScanner::scan_siblings()` against `sibling_dir`'s
    /// parent.
    fn write_sibling_schema(sibling_dir: &Utf8Path, schema: &FederatedSchema) {
        let cg_state_dir = sibling_dir.join(".ledgerful").join("state");
        fs::create_dir_all(&cg_state_dir).unwrap();
        let schema_json = serde_json::to_string_pretty(schema).unwrap();
        fs::write(cg_state_dir.join("schema.json"), schema_json).unwrap();
    }

    fn empty_entity_schema(repo_name: &str) -> FederatedSchema {
        FederatedSchema::new(
            repo_name.to_string(),
            vec![PublicInterface {
                symbol: "do_thing".to_string(),
                file: "src/lib.rs".to_string(),
                kind: SymbolKind::Function,
            }],
        )
        .with_ledger(vec![FederatedLedgerEntry {
            tx_id: "tx-ai-brains-1".to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: String::new(),
            change_type: ChangeType::Create,
            summary: "AI-Brains entry with no entity recorded".to_string(),
            reason: "legacy export".to_string(),
            is_breaking: false,
            committed_at: "2026-06-24T00:00:00Z".to_string(),
            author: String::new(),
        }])
    }

    fn path_traversal_schema(repo_name: &str) -> FederatedSchema {
        FederatedSchema::new(repo_name.to_string(), vec![]).with_ledger(vec![
            FederatedLedgerEntry {
                tx_id: "tx-malicious-1".to_string(),
                category: Category::Feature,
                entry_type: EntryType::Implementation,
                entity: "../outside.rs".to_string(),
                change_type: ChangeType::Create,
                summary: "malicious entry".to_string(),
                reason: "attempted path traversal".to_string(),
                is_breaking: false,
                committed_at: "2026-06-24T00:00:00Z".to_string(),
                author: String::new(),
            },
        ])
    }

    #[test]
    fn empty_entity_sibling_is_discovered_with_warning() {
        let workspace = tempdir().unwrap();
        let workspace_path = Utf8PathBuf::from_path_buf(workspace.path().to_path_buf()).unwrap();

        let local_root = workspace_path.join("local-repo");
        let sibling_root = workspace_path.join("ai-brains");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(&sibling_root).unwrap();
        // `scan_siblings` requires a `.ledgerful/` dir to even consider
        // the candidate, for both the local root (to detect "is_root") and
        // the sibling (the gate at line ~190).
        fs::create_dir_all(local_root.join(".ledgerful")).unwrap();

        write_sibling_schema(&sibling_root, &empty_entity_schema("ai-brains"));

        let scanner = FederatedScanner::new(local_root);
        let (discovered, _top_level_warnings) = scanner.scan_siblings().unwrap();

        assert_eq!(
            discovered.len(),
            1,
            "expected the empty-entity sibling to be discovered, not hard-skipped"
        );
        let (_path, schema, warnings) = &discovered[0];
        assert_eq!(schema.repo_name, "ai-brains");
        assert!(
            !warnings.is_empty(),
            "expected a non-empty per-sibling warning list for the empty entity"
        );
        assert!(
            warnings.iter().any(|w| w.contains("entity")),
            "expected a warning mentioning 'entity', got {:?}",
            warnings
        );
    }

    /// TA31 R1 regression lock-in: importing a discovered sibling's ledger
    /// entries via `import_federated_entries` must not panic when an
    /// entry has an empty `entity` — `normalize_relative_path(repo_root,
    /// "")` returns `Ok("")` rather than erroring, so the empty-entity
    /// path is safe end-to-end, not just at the scanner layer.
    #[test]
    fn empty_entity_sibling_imports_without_panic() {
        let workspace = tempdir().unwrap();
        let workspace_path = Utf8PathBuf::from_path_buf(workspace.path().to_path_buf()).unwrap();

        let local_root = workspace_path.join("local-repo");
        let sibling_root = workspace_path.join("ai-brains");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(&sibling_root).unwrap();
        fs::create_dir_all(local_root.join(".ledgerful")).unwrap();

        let schema = empty_entity_schema("ai-brains");
        write_sibling_schema(&sibling_root, &schema);

        let scanner = FederatedScanner::new(local_root.clone());
        let (discovered, _) = scanner.scan_siblings().unwrap();
        assert_eq!(discovered.len(), 1);
        let (_path, discovered_schema, _warnings) = &discovered[0];
        let entries = discovered_schema
            .ledger
            .as_ref()
            .expect("ledger entries must be present");

        let db_path = local_root.join(".ledgerful").join("ledger.db");
        let mut storage =
            crate::state::storage::StorageManager::init(db_path.as_std_path()).unwrap();

        // Must not panic.
        crate::ledger::federation::import_federated_entries(
            storage.get_connection_mut(),
            local_root.as_std_path(),
            &discovered_schema.repo_name,
            entries,
        )
        .expect("import_federated_entries must succeed even with an empty entity");
    }

    #[test]
    fn path_traversal_sibling_is_hard_skipped() {
        let workspace = tempdir().unwrap();
        let workspace_path = Utf8PathBuf::from_path_buf(workspace.path().to_path_buf()).unwrap();

        let local_root = workspace_path.join("local-repo");
        let sibling_root = workspace_path.join("malicious-sibling");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(&sibling_root).unwrap();
        fs::create_dir_all(local_root.join(".ledgerful")).unwrap();

        write_sibling_schema(&sibling_root, &path_traversal_schema("malicious-sibling"));

        let scanner = FederatedScanner::new(local_root);
        let (discovered, _top_level_warnings) = scanner.scan_siblings().unwrap();

        assert!(
            discovered.is_empty(),
            "a sibling with a path-traversal entity must still be hard-skipped (security \
             boundary), got {:?}",
            discovered
                .iter()
                .map(|(_, s, _)| s.repo_name.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// TA31 R2: unit tests for the staleness fast-path (`needs_sync`,
/// `last_commit_mtime`) and the auto-sync opt-in/backward-compat boundary
/// in `scan_siblings()`. These deliberately do NOT exercise
/// `run_federate_export` (that would spawn a real subprocess); the
/// subprocess path is covered by the `__slow` integration test in
/// `tests/integration/cli_federate.rs`.
#[cfg(test)]
mod auto_sync_tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn rfc3339(t: SystemTime) -> String {
        chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
    }

    // ------------------------------------------------------------------
    // needs_sync
    // ------------------------------------------------------------------

    #[test]
    fn needs_sync_true_when_schema_missing() {
        assert!(needs_sync(false, "", None));
        // Even with a (nonsensical) populated generated_at, a missing
        // schema file always needs sync.
        let now = SystemTime::now();
        assert!(needs_sync(false, &rfc3339(now), Some(now)));
    }

    #[test]
    fn needs_sync_true_when_generated_at_empty() {
        // Legacy/unstamped schema.json (pre-TA31 R4): can't prove
        // freshness, so sync.
        assert!(needs_sync(true, "", None));
        assert!(needs_sync(true, "   ", Some(SystemTime::now())));
    }

    #[test]
    fn needs_sync_true_when_generated_at_unparseable() {
        assert!(needs_sync(
            true,
            "not-a-real-timestamp",
            Some(SystemTime::now())
        ));
    }

    #[test]
    fn needs_sync_true_when_commit_mtime_newer_than_generated_at() {
        let generated_at = SystemTime::now() - Duration::from_secs(3600);
        let commit_mtime = SystemTime::now();
        assert!(needs_sync(true, &rfc3339(generated_at), Some(commit_mtime)));
    }

    #[test]
    fn needs_sync_false_when_commit_mtime_older_than_generated_at() {
        let generated_at = SystemTime::now();
        let commit_mtime = SystemTime::now() - Duration::from_secs(3600);
        assert!(!needs_sync(
            true,
            &rfc3339(generated_at),
            Some(commit_mtime)
        ));
    }

    #[test]
    fn needs_sync_false_when_commit_mtime_equal_to_generated_at() {
        let t = SystemTime::now();
        assert!(!needs_sync(true, &rfc3339(t), Some(t)));
    }

    #[test]
    fn needs_sync_false_when_commit_mtime_is_none_and_generated_at_present() {
        // No git HEAD info available (e.g. .git missing or unreadable) but
        // the schema is stamped: nothing proves staleness, so don't sync.
        assert!(!needs_sync(true, &rfc3339(SystemTime::now()), None));
    }

    // ------------------------------------------------------------------
    // last_commit_mtime
    // ------------------------------------------------------------------

    #[test]
    fn last_commit_mtime_resolves_through_symbolic_head() {
        let tmp = tempdir().unwrap();
        let git_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
            .unwrap()
            .join(".git");
        let refs_dir = git_dir.join("refs").join("heads");
        fs::create_dir_all(&refs_dir).unwrap();

        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(refs_dir.join("main"), "deadbeef\n").unwrap();

        let branch_mtime = fs::metadata(refs_dir.join("main"))
            .unwrap()
            .modified()
            .unwrap();
        let resolved = last_commit_mtime(&git_dir).expect("expected a resolved mtime");

        assert_eq!(
            resolved, branch_mtime,
            "expected last_commit_mtime to resolve through the symbolic ref to the branch \
             ref file's mtime, not HEAD's own mtime"
        );
    }

    #[test]
    fn last_commit_mtime_falls_back_to_head_for_detached_sha() {
        let tmp = tempdir().unwrap();
        let git_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
            .unwrap()
            .join(".git");
        fs::create_dir_all(&git_dir).unwrap();

        // Detached HEAD: raw SHA, no "ref: " prefix.
        fs::write(
            git_dir.join("HEAD"),
            "1234567890abcdef1234567890abcdef12345678\n",
        )
        .unwrap();

        let head_mtime = fs::metadata(git_dir.join("HEAD"))
            .unwrap()
            .modified()
            .unwrap();
        let resolved = last_commit_mtime(&git_dir).expect("expected a resolved mtime");

        assert_eq!(
            resolved, head_mtime,
            "expected last_commit_mtime to fall back to HEAD's own mtime for a detached/raw-SHA \
             HEAD"
        );
    }

    #[test]
    fn last_commit_mtime_falls_back_to_head_when_ref_file_missing() {
        // Symbolic HEAD pointing at a ref that doesn't exist on disk (e.g.
        // packed-refs case): fall back to HEAD's own mtime.
        let tmp = tempdir().unwrap();
        let git_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
            .unwrap()
            .join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // Deliberately do NOT create refs/heads/main.

        let head_mtime = fs::metadata(git_dir.join("HEAD"))
            .unwrap()
            .modified()
            .unwrap();
        let resolved = last_commit_mtime(&git_dir).expect("expected a resolved mtime");

        assert_eq!(resolved, head_mtime);
    }

    #[test]
    fn last_commit_mtime_none_when_git_dir_missing() {
        let tmp = tempdir().unwrap();
        let git_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf())
            .unwrap()
            .join(".git");
        // Deliberately do not create the .git directory at all.
        assert_eq!(last_commit_mtime(&git_dir), None);
    }

    // ------------------------------------------------------------------
    // scan_siblings auto-sync opt-in / backward-compat boundary
    // ------------------------------------------------------------------

    /// TA31 R2 regression guard: with `auto_sync` left at its default
    /// (`false`), a sibling with a `.ledgerful/` dir but no schema.json
    /// must still be silently skipped exactly as before TA31 — no
    /// subprocess is ever spawned, and no schema.json appears on disk
    /// after the scan.
    #[test]
    fn auto_sync_disabled_by_default_skips_missing_schema_exactly_as_before() {
        let workspace = tempdir().unwrap();
        let workspace_path = Utf8PathBuf::from_path_buf(workspace.path().to_path_buf()).unwrap();

        let local_root = workspace_path.join("local-repo");
        let sibling_root = workspace_path.join("sibling-no-schema");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(local_root.join(".ledgerful")).unwrap();
        // The sibling has a `.ledgerful/` dir (already initialized) but
        // deliberately no schema.json anywhere under it.
        fs::create_dir_all(sibling_root.join(".ledgerful")).unwrap();

        let scanner = FederatedScanner::new(local_root);
        // Deliberately do NOT call `.with_auto_sync(true)` — exercising the
        // default.
        let (discovered, _warnings) = scanner.scan_siblings().unwrap();

        assert!(
            discovered.is_empty(),
            "a sibling with no schema.json must still be silently skipped when auto_sync is \
             disabled (the default), got {:?}",
            discovered
                .iter()
                .map(|(_, s, _)| s.repo_name.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            !sibling_root
                .join(".ledgerful")
                .join("state")
                .join("schema.json")
                .exists(),
            "no schema.json should have been generated when auto_sync is disabled"
        );
    }

    /// Sanity check that `with_auto_sync(false)` (explicit, not just
    /// default) behaves identically — same skip, no schema.json created.
    #[test]
    fn auto_sync_explicitly_false_skips_missing_schema() {
        let workspace = tempdir().unwrap();
        let workspace_path = Utf8PathBuf::from_path_buf(workspace.path().to_path_buf()).unwrap();

        let local_root = workspace_path.join("local-repo");
        let sibling_root = workspace_path.join("sibling-no-schema");
        fs::create_dir_all(&local_root).unwrap();
        fs::create_dir_all(local_root.join(".ledgerful")).unwrap();
        fs::create_dir_all(sibling_root.join(".ledgerful")).unwrap();

        let scanner = FederatedScanner::new(local_root).with_auto_sync(false);
        let (discovered, _warnings) = scanner.scan_siblings().unwrap();

        assert!(discovered.is_empty());
        assert!(
            !sibling_root
                .join(".ledgerful")
                .join("state")
                .join("schema.json")
                .exists()
        );
    }
}
