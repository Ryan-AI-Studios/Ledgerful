use crate::commands::verify::enumerate_invalid_ledger_entries;
use crate::config::model::GlobalRollupConfig;
use crate::ledger::db::LedgerDb;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Per-repo posture summary emitted by `ledger status --global`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepoPosture {
    pub repo_path: String,
    pub unsigned_entries: usize,
    pub pending_tx: usize,
    pub drift: usize,
    pub last_verify_result: Option<String>,
    pub last_verify_at: Option<String>,
}

/// Full JSON output shape for `ledger status --global --json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalPostureOutput {
    pub total_repos: usize,
    pub skipped_repos: usize,
    pub repos: Vec<RepoPosture>,
    pub warnings: Vec<String>,
}

/// Persistent cache record stored in `~/.ledgerful/rollup/cache.sqlite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedRepo {
    repo_path: String,
    db_path: String,
    unsigned_entries: usize,
    pending_tx: usize,
    drift: usize,
    last_verify_result: Option<String>,
    last_verify_at: Option<String>,
}

/// Run the global posture rollup for the configured roots.
///
/// `repo_filter` scopes to a single repo path when provided. `reindex` forces
/// a fresh walk even if the cache appears fresh. `json` controls the output
/// format. Returns `Ok(())` after printing the result.
pub fn execute_ledger_status_global(
    config: &GlobalRollupConfig,
    repo_filter: Option<&str>,
    reindex: bool,
    json: bool,
) -> Result<()> {
    if !config.enabled {
        println!("global rollup disabled — run `ledger status --global --opt-in` to re-enable");
        return Ok(());
    }

    let output = build_global_posture(config, repo_filter, reindex)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&output).into_diagnostic()?
        );
    } else {
        print_global_posture_text(&output);
    }

    Ok(())
}

/// Build the global posture value without printing it. Useful for tests and for
/// callers that want to consume the result programmatically.
pub fn build_global_posture(
    config: &GlobalRollupConfig,
    repo_filter: Option<&str>,
    reindex: bool,
) -> Result<GlobalPostureOutput> {
    let roots = resolve_roots(config)?;
    let cache_path = global_rollup_cache_path()?;
    ensure_parent(&cache_path)?;

    let mut warnings = Vec::new();
    let (repo_map, cached_postures, walk_warnings) =
        discover_repos(&roots, config.timeout_secs, &cache_path, reindex, config)?;
    warnings.extend(walk_warnings);

    let mut postures: Vec<RepoPosture> = Vec::new();
    let mut skipped = 0usize;

    // Build a lookup of cached postures by repo_path. Cached postures come from
    // the cache when all roots (or all non-stale roots) are fresh; they let us
    // skip reopening every per-repo DB on a cache hit. Partial hits re-query
    // only stale-root repos.
    let cached_by_repo: BTreeMap<&str, &RepoPosture> = cached_postures
        .as_ref()
        .map(|vec| {
            vec.iter()
                .map(|p| (p.repo_path.as_str(), p))
                .collect::<BTreeMap<&str, &RepoPosture>>()
        })
        .unwrap_or_default();

    for (repo_path, db_path) in repo_map.iter() {
        if let Some(filter) = repo_filter
            && !repo_filter_matches(repo_path.as_std_path(), filter)
        {
            continue;
        }

        // If this root is fresh in the cache, use the cached posture without
        // reopening the repo DB. Stale roots and any newly discovered repos
        // during a partial re-walk fall through to query_repo_posture.
        let repo_path_str = repo_path.as_str();
        if let Some(cached) = cached_by_repo.get(repo_path_str) {
            postures.push((*cached).clone());
            continue;
        }

        match query_repo_posture(db_path) {
            Ok(posture) => postures.push(posture),
            Err(e) => {
                let msg = format!("skipped {}: {}", repo_path, e);
                warn!("{}", msg);
                warnings.push(msg);
                skipped += 1;
            }
        }
    }

    // Worst-first: unsigned desc, pending desc, drift desc, last_verify_at asc.
    postures.sort_by(|a, b| {
        b.unsigned_entries
            .cmp(&a.unsigned_entries)
            .then_with(|| b.pending_tx.cmp(&a.pending_tx))
            .then_with(|| b.drift.cmp(&a.drift))
            .then_with(|| {
                // Treat None as "oldest" so repos with no verification float down.
                match (&a.last_verify_at, &b.last_verify_at) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(a_ts), Some(b_ts)) => a_ts.cmp(b_ts),
                }
            })
    });

    // Persist cache for subsequent fast paths. Cache is derived only.
    if let Err(e) = write_cache(&cache_path, &roots, &postures) {
        warn!("failed to write rollup cache: {}", e);
    }

    Ok(GlobalPostureOutput {
        total_repos: postures.len(),
        skipped_repos: skipped,
        repos: postures,
        warnings,
    })
}

fn print_global_posture_text(output: &GlobalPostureOutput) {
    println!("{}", "Ledgerful Global Posture".bold().underline());
    println!(
        "{} repo(s) queried, {} skipped",
        output.repos.len().to_string().cyan(),
        output.skipped_repos.to_string().yellow()
    );
    if !output.warnings.is_empty() {
        println!(
            "\n{} {}",
            "Warnings:".yellow().bold(),
            "(per-repo failures are non-fatal)".dimmed()
        );
        for w in &output.warnings {
            println!("  {} {}", "⚠".yellow(), w.dimmed());
        }
    }

    if output.repos.is_empty() {
        println!("\n  No Ledgerful repos discovered.");
    } else {
        let mut table = crate::output::table::build_table(vec![
            "Repo",
            "Unsigned",
            "Pending",
            "Drift",
            "Last Verify",
        ]);
        for p in &output.repos {
            let verify_cell = match (&p.last_verify_result, &p.last_verify_at) {
                (Some(result), Some(at)) => format!("{} {}", result, at.dimmed()),
                (Some(result), None) => result.clone(),
                (None, Some(at)) => format!("— {}", at.dimmed()),
                (None, None) => "—".to_string(),
            };
            table.add_row(vec![
                p.repo_path.cyan().to_string(),
                p.unsigned_entries.to_string().yellow().to_string(),
                p.pending_tx.to_string().yellow().to_string(),
                p.drift.to_string().red().to_string(),
                verify_cell,
            ]);
        }
        println!("\n{}", table);
    }
}

/// Gated `timings --global` entry point. Prints an honest message if the
/// per-repo `command_timings` table is not implemented (0043).
pub fn execute_timings_global(_config: &GlobalRollupConfig, _json: bool) -> Result<()> {
    // 0043 is not implemented, so the table cannot exist anywhere. We still
    // attempt a token discovery to keep the message honest, but the spec says
    // absent table → honest message, exit 0, no fabricated rows.
    println!("per-repo timing not enabled (see track 0043 — self-timing facility)");
    Ok(())
}

/// Returns true if the user's `--repo` filter should select `repo_path`.
/// Supports both bare repo names (`foo`) and absolute/relative paths.
fn repo_filter_matches(repo_path: &Path, filter: &str) -> bool {
    // Canonical filter path (if it exists and is absolute).
    let filter_normalized = normalize_filter(filter);
    let repo_normalized = normalize_path_for_match(repo_path);

    // Exact full-path match after normalization.
    if repo_normalized == filter_normalized {
        return true;
    }

    // Last-component match: `--repo foo` matches .../foo but not .../foobar.
    let repo_file_name = repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let filter_file_name = std::path::Path::new(&filter_normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&filter_normalized);

    repo_file_name == filter_file_name
}

/// Normalize a path string for matching: forward-slash form, no trailing slash.
fn normalize_path_for_match(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.trim_end_matches('/').to_string()
}

/// Normalize the filter the same way, but fall back to the literal input.
fn normalize_filter(filter: &str) -> String {
    let candidate = PathBuf::from(filter);
    match std::fs::canonicalize(&candidate) {
        Ok(canonical) => normalize_path_for_match(&canonical),
        Err(_) => filter.replace('\\', "/").trim_end_matches('/').to_string(),
    }
}

/// Placeholder for the non-global `ledgerful timings` surface. Track 0043 owns
/// the full implementation; this track only ships the gated `--global` slice.
pub fn execute_timings_not_implemented() -> Result<()> {
    println!("timings not implemented (see track 0043)");
    Ok(())
}

/// Resolve configured roots, expanding leading `~` to the user's home dir.
fn resolve_roots(config: &GlobalRollupConfig) -> Result<Vec<PathBuf>> {
    let home =
        dirs::home_dir().ok_or_else(|| miette::miette!("could not determine home directory"))?;
    let mut resolved = Vec::new();
    for root in &config.roots {
        let path = if let Some(s) = root.to_str()
            && s.starts_with("~/")
        {
            home.join(&s[2..])
        } else {
            root.clone()
        };
        let canonical = match std::fs::canonicalize(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "global rollup: root '{}' could not be resolved, skipping: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };
        resolved.push(canonical);
    }
    Ok(resolved)
}

/// Path to the derived rollup cache: `~/.ledgerful/rollup/cache.sqlite`.
///
/// Tests and power users may override this with `LEDGERFUL_ROLLUP_CACHE`.
fn global_rollup_cache_path() -> Result<PathBuf> {
    if let Some(env_path) = std::env::var_os("LEDGERFUL_ROLLUP_CACHE") {
        return Ok(PathBuf::from(env_path));
    }
    let config_dir = user_config_dir()?;
    Ok(config_dir.join("rollup").join("cache.sqlite"))
}

/// Return the Ledgerful user config directory (`~/.ledgerful`), respecting
/// `LEDGERFUL_CONFIG_HOME` for tests and relocated installs.
pub fn user_config_dir() -> Result<PathBuf> {
    if let Some(env_path) = std::env::var_os("LEDGERFUL_CONFIG_HOME") {
        return Ok(PathBuf::from(env_path));
    }
    let home =
        dirs::home_dir().ok_or_else(|| miette::miette!("could not determine home directory"))?;
    Ok(home.join(".ledgerful"))
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).into_diagnostic()?;
    }
    Ok(())
}

/// Discover per-repo `ledger.db` files under the given roots.
///
/// Returns a map of repo_root → db_path, an optional vec of cached postures
/// for roots that were still fresh (None when a full re-walk happened), plus
/// any walk warnings. Honors the timeout, skips hidden dirs except `.ledgerful`,
/// skips heavy trees, and swallows I/O/permission errors per-entry. When
/// `cached_postures` is `Some`, the caller can use those posture summaries for
/// fresh roots instead of reopening every repo DB.
fn discover_repos(
    roots: &[PathBuf],
    timeout_secs: u64,
    cache_path: &Path,
    reindex: bool,
    config: &GlobalRollupConfig,
) -> Result<DiscoveryResult> {
    // If cache is fresh and --reindex is not set, return cached map directly.
    if !reindex {
        match try_load_cache(roots, cache_path, config.staleness_secs) {
            Ok(Some((cached_map, cached_postures, stale_roots))) if stale_roots.is_empty() => {
                return Ok((cached_map, Some(cached_postures), Vec::new()));
            }
            Ok(Some((cached_map, cached_postures, stale_roots))) => {
                // Re-walk only stale roots, then merge with cached entries.
                let deadline = Instant::now() + Duration::from_secs(timeout_secs);
                let (walked, warnings) = walk_roots(&stale_roots, deadline, config)?;
                let mut merged = cached_map;
                // Remove stale-root entries that are being refreshed, then add new.
                merged.retain(|repo_path, _| {
                    stale_roots
                        .iter()
                        .all(|stale| !root_contains_repo(stale, repo_path.as_str()))
                });
                merged.extend(walked);
                // Drop cached postures that belong to stale roots; they will be
                // re-queried during posture assembly. Fresh-root cached postures
                // are preserved and returned so the cache hit path can avoid
                // reopening those DBs.
                let fresh_postures: Vec<RepoPosture> = cached_postures
                    .into_iter()
                    .filter(|p| {
                        stale_roots
                            .iter()
                            .all(|stale| !root_contains_repo(stale, &p.repo_path))
                    })
                    .collect();
                return Ok((merged, Some(fresh_postures), warnings));
            }
            Ok(None) => {}
            Err(e) => {
                warn!("rollup cache load failed, falling back to full walk: {}", e);
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let (map, warnings) = walk_roots(roots, deadline, config)?;
    Ok((map, None, warnings))
}

fn walk_roots(
    roots: &[PathBuf],
    deadline: Instant,
    config: &GlobalRollupConfig,
) -> Result<(BTreeMap<Utf8PathBuf, PathBuf>, Vec<String>)> {
    let mut map: BTreeMap<Utf8PathBuf, PathBuf> = BTreeMap::new();
    let mut warnings = Vec::new();

    for root in roots {
        if Instant::now() >= deadline {
            warnings.push(format!(
                "timeout reached; skipped remaining roots after {}",
                root.display()
            ));
            break;
        }

        let mut builder = WalkBuilder::new(root);
        builder
            .follow_links(false)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .ignore(false);
        if let Some(depth) = config.max_depth {
            builder.max_depth(Some(depth));
        }
        builder.filter_entry(|entry| {
            // Skip hidden dirs except .ledgerful; skip heavy/common junk trees.
            let path = entry.path();
            !should_prune_path(path)
        });

        let timeout_deadline = deadline;

        let walker = builder.build();
        for entry in walker {
            if Instant::now() >= timeout_deadline {
                warnings.push(format!("timeout reached while walking {}", root.display()));
                break;
            }

            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    let _depth = entry.depth();
                    if let Some(name) = path.file_name()
                        && name == "ledger.db"
                        && let Some(repo_root) = ledger_db_to_repo_root(path)
                        && map.insert(repo_root.clone(), path.to_path_buf()).is_some()
                    {
                        warnings.push(format!("duplicate repo path discovered: {}", repo_root));
                    }
                }
                Err(e) => {
                    // Swallow per-entry errors (PermissionDenied, I/O, etc.).
                    warnings.push(format!("{}: {}", root.display(), e));
                }
            }
        }
    }

    Ok((map, warnings))
}

fn should_prune_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == ".ledgerful" {
        return false;
    }
    if name.starts_with('.') {
        return true;
    }
    name == "node_modules" || name == ".git" || name == "target" || name == "vendor"
}

fn ledger_db_to_repo_root(path: &Path) -> Option<Utf8PathBuf> {
    let state_dir = path.parent()?;
    let ledgerful_dir = state_dir.parent()?;
    if ledgerful_dir.file_name() != Some(std::ffi::OsStr::new(".ledgerful")) {
        return None;
    }
    let repo_root = ledgerful_dir.parent()?;
    Utf8PathBuf::from_path_buf(repo_root.to_path_buf()).ok()
}

/// Query a single repo's posture. Opens the DB read-only, runs the posture
/// queries, and closes the connection. Any error is returned so the caller can
/// warn-and-skip.
fn query_repo_posture(db_path: &Path) -> Result<RepoPosture> {
    let storage = StorageManager::open_read_only_from_path(db_path)?;
    let repo_path = storage.root_path().to_string();
    let conn = storage.get_connection();
    let db = LedgerDb::new(conn);

    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette::miette!("failed to read ledger entries: {}", e))?;
    // The rollup counts all entries lacking a valid signature (both missing-sig
    // and invalid-sig) as `unsigned_entries` to surface trust risk, regardless of
    // whether the per-repo config enforces signing. `signing_required = true`
    // here means "count missing signatures as invalid" for the rollup view, not
    // "signing is enforced for this repo".
    let invalid = enumerate_invalid_ledger_entries(&entries, true);

    let pending = db
        .get_all_pending()
        .map_err(|e| miette::miette!("failed to read pending transactions: {}", e))?;
    let unaudited = db
        .get_all_unaudited()
        .map_err(|e| miette::miette!("failed to read unaudited drift: {}", e))?;

    let (last_result, last_at) = match storage.get_latest_verification_run() {
        Ok(Some((_, ts, pass))) => {
            let result = if pass {
                "PASS".to_string()
            } else {
                "FAIL".to_string()
            };
            (Some(result), Some(ts))
        }
        _ => (None, None),
    };

    storage.shutdown()?;

    Ok(RepoPosture {
        repo_path,
        unsigned_entries: invalid.len(),
        pending_tx: pending.len(),
        drift: unaudited.len(),
        last_verify_result: last_result,
        last_verify_at: last_at,
    })
}

/// Cache schema: a single table holding a JSON blob of discovered repos plus
/// root mtimes for staleness checks.
fn ensure_cache_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rollup_cache (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .into_diagnostic()?;
    Ok(())
}

/// Cache key for the discovery roots list.
const CACHE_KEY_ROOTS: &str = "roots";
/// Cache key prefix for per-repo posture records.
const CACHE_KEY_REPOS: &str = "repos";

fn root_mtime(root: &Path) -> Option<u64> {
    let ledgerful = root.join(".ledgerful");
    let target = if ledgerful.exists() {
        ledgerful
    } else {
        root.to_path_buf()
    };
    std::fs::metadata(&target)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()))
}

fn root_contains_repo(root: &Path, repo_path: &str) -> bool {
    let repo = Utf8PathBuf::from(repo_path);
    let root_utf8 = match Utf8PathBuf::from_path_buf(root.to_path_buf()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let root_str = root_utf8.as_str();
    let repo_str = repo.as_str();
    if !repo_str.starts_with(root_str) {
        return false;
    }
    if repo_str.len() == root_str.len() {
        return true;
    }
    // Treat both `/` and `\` as path separators (Windows + POSIX).
    let next = repo_str[root_str.len()..].chars().next();
    next == Some('/') || next == Some('\\')
}

/// Load the cache if it exists, is uncorrupted, and is not stale.
/// Returns Some((map, cached_postures, stale_roots)) if at least a partial
/// cached result is usable. `cached_postures` holds the posture summaries for
/// repos whose root is still fresh; `stale_roots` lists roots that need
/// re-walking. Returns None if the cache is missing, corrupted, or too stale to
/// trust.
///
/// Edge case: a repo recorded in the cache may no longer exist on disk (e.g.
/// the user deleted it since the last walk). On a fully-fresh cache it will
/// still be returned here because the staleness window — not an on-disk
/// re-walk — bounds cache validity. Callers should handle missing DBs during
/// posture assembly just as they handle any other per-repo failure.
type DiscoveredRepos = BTreeMap<Utf8PathBuf, PathBuf>;
/// Discovery result tuple: repo_root → db_path map, optional cached postures
/// for fresh roots, and walk warnings.
type DiscoveryResult = (DiscoveredRepos, Option<Vec<RepoPosture>>, Vec<String>);
/// Cache load tuple: repo map, cached postures for fresh roots, and stale roots.
type CacheLoadResult = (DiscoveredRepos, Vec<RepoPosture>, Vec<PathBuf>);

fn try_load_cache(
    roots: &[PathBuf],
    cache_path: &Path,
    staleness_secs: u64,
) -> Result<Option<CacheLoadResult>> {
    if !cache_path.exists() {
        return Ok(None);
    }

    let conn = rusqlite::Connection::open(cache_path).into_diagnostic()?;
    let integrity: String = match conn.query_row("PRAGMA integrity_check", [], |row| row.get(0)) {
        Ok(s) => s,
        Err(e) => {
            warn!("rollup cache integrity check failed: {}; re-walking", e);
            return Ok(None);
        }
    };
    if integrity != "ok" {
        warn!(
            "rollup cache integrity check returned non-ok: {}; re-walking",
            integrity
        );
        return Ok(None);
    }
    ensure_cache_schema(&conn)?;

    let cached_roots_json: Option<String> = conn
        .query_row(
            "SELECT value FROM rollup_cache WHERE key = ?1",
            [CACHE_KEY_ROOTS],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    let cached_roots: Vec<PathBuf> = match cached_roots_json {
        Some(json) => serde_json::from_str(&json).into_diagnostic()?,
        None => return Ok(None),
    };
    if cached_roots != roots {
        // Roots changed; full re-walk required.
        return Ok(None);
    }

    let cached_repos_json: Option<String> = conn
        .query_row(
            "SELECT value FROM rollup_cache WHERE key = ?1",
            [CACHE_KEY_REPOS],
            |row| row.get(0),
        )
        .optional()
        .into_diagnostic()?;
    let cached_repos: Vec<CachedRepo> = match cached_repos_json {
        Some(json) => serde_json::from_str(&json).into_diagnostic()?,
        None => return Ok(None),
    };

    let mut map = BTreeMap::new();
    let mut cached_postures = Vec::new();
    for repo in cached_repos {
        let repo_path = Utf8PathBuf::from(repo.repo_path.clone());
        map.insert(repo_path, PathBuf::from(repo.db_path));
        cached_postures.push(RepoPosture {
            repo_path: repo.repo_path,
            unsigned_entries: repo.unsigned_entries,
            pending_tx: repo.pending_tx,
            drift: repo.drift,
            last_verify_result: repo.last_verify_result,
            last_verify_at: repo.last_verify_at,
        });
    }

    // Staleness decision: a root is fresh if BOTH
    //   cache_mtime + staleness_secs >= now   (cache isn't too old), AND
    //   cache_mtime >= root_mtime               (root hasn't changed since cache).
    // Re-walk when either condition fails.
    let cache_mtime: u64 = std::fs::metadata(cache_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs()))
        .unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut stale_roots = Vec::new();
    for root in roots {
        let root_mtime_val = root_mtime(root).unwrap_or(0);
        let cache_fresh_by_age = cache_mtime.saturating_add(staleness_secs) >= now;
        let cache_fresh_by_root = cache_mtime >= root_mtime_val;
        if !cache_fresh_by_age || !cache_fresh_by_root {
            stale_roots.push(root.clone());
        }
    }

    Ok(Some((map, cached_postures, stale_roots)))
}

fn write_cache(cache_path: &Path, roots: &[PathBuf], postures: &[RepoPosture]) -> Result<()> {
    let conn = rusqlite::Connection::open(cache_path).into_diagnostic()?;
    ensure_cache_schema(&conn)?;

    let cached: Vec<CachedRepo> = postures
        .iter()
        .map(|p| CachedRepo {
            repo_path: p.repo_path.clone(),
            db_path: {
                let layout = Layout::new(Utf8Path::new(&p.repo_path));
                layout.state_subdir().join("ledger.db").to_string()
            },
            unsigned_entries: p.unsigned_entries,
            pending_tx: p.pending_tx,
            drift: p.drift,
            last_verify_result: p.last_verify_result.clone(),
            last_verify_at: p.last_verify_at.clone(),
        })
        .collect();

    let roots_json = serde_json::to_string(roots).into_diagnostic()?;
    let repos_json = serde_json::to_string(&cached).into_diagnostic()?;

    conn.execute(
        "INSERT OR REPLACE INTO rollup_cache (key, value) VALUES (?1, ?2)",
        [CACHE_KEY_ROOTS, &roots_json],
    )
    .into_diagnostic()?;
    conn.execute(
        "INSERT OR REPLACE INTO rollup_cache (key, value) VALUES (?1, ?2)",
        [CACHE_KEY_REPOS, &repos_json],
    )
    .into_diagnostic()?;

    Ok(())
}

/// Set the `[global_rollup] enabled` flag in the user config at
/// `~/.ledgerful/config.toml`, creating the file/table as needed.
pub fn set_global_rollup_enabled(enabled: bool) -> Result<()> {
    let config_dir = user_config_dir()?;
    std::fs::create_dir_all(&config_dir).into_diagnostic()?;
    let config_path = config_dir.join("config.toml");

    let mut doc = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).into_diagnostic()?;
        content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| miette::miette!("failed to parse user config: {}", e))?
    } else {
        toml_edit::DocumentMut::new()
    };

    let root = doc.as_table_mut();
    let rollup = root.entry("global_rollup").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    let table = rollup
        .as_table_mut()
        .ok_or_else(|| miette::miette!("global_rollup is not a table"))?;
    table.insert("enabled", toml_edit::value(enabled));

    std::fs::write(&config_path, doc.to_string()).into_diagnostic()?;
    Ok(())
}
