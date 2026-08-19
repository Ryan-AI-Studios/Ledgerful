//! `dependencies list` / `dependencies audit` CLI surfaces.
//!
//! Default **list** reads live `Cargo.toml` + `Cargo.lock` (no Cozo): declared
//! direct deps with locked versions from the root package's own lock
//! `dependencies` array (H1). Full lock is `--all`. Audit still populates Cozo.

use crate::commands::helpers::{get_layout, get_layout_or_cwd_if_not_git};
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use crate::util::cargo_lock::{CargoLockPackage, parse_cargo_lock, parse_lock_dep_string};
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

const ECOSYSTEM: &str = "rust/cargo";

#[derive(Args, Debug)]
#[command(
    after_help = "Default when omitted: list.\nFlags such as --json require an explicit subcommand (e.g. `dependencies list --json`)."
)]
pub struct DependenciesArgs {
    #[command(subcommand)]
    pub command: Option<DependencySubcommands>,
}

impl DependenciesArgs {
    /// Resolve bare `dependencies` to read-only list with default flags.
    pub fn command_or_default(self) -> DependencySubcommands {
        self.command.unwrap_or(DependencySubcommands::List {
            json: false,
            verbose: false,
            all: false,
        })
    }
}

#[derive(Subcommand, Debug)]
pub enum DependencySubcommands {
    /// List project dependencies (default: direct from Cargo.toml + Cargo.lock)
    List {
        /// Output as JSON (schemaVersion envelope)
        #[arg(long)]
        json: bool,
        /// Richer direct columns (Req, Source) — not a synonym of --all
        #[arg(short, long)]
        verbose: bool,
        /// List all packages from Cargo.lock (full lock dump)
        #[arg(short = 'a', long)]
        all: bool,
    },
    /// Audit dependencies for known vulnerabilities (requires OSV-Scanner JSON)
    Audit {
        /// Path to OSV-Scanner JSON output
        #[arg(short, long)]
        input: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ---------------------------------------------------------------------------
// Resolve types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DepKind {
    Normal,
    Build,
    Dev,
}

impl DepKind {
    fn as_str(self) -> &'static str {
        match self {
            DepKind::Normal => "normal",
            DepKind::Build => "build",
            DepKind::Dev => "dev",
        }
    }
}

#[derive(Debug, Clone)]
struct DeclaredDep {
    name: String,
    kind: DepKind,
    optional: bool,
    req: Option<String>,
    target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RootInfo {
    name: String,
    version: String,
    source: String,
}

#[derive(Debug, Clone, Serialize)]
struct ListedPackage {
    name: String,
    /// Locked version; null when not selected / no lock.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    ecosystem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    req: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DependenciesListEnvelope {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    mode: String,
    ecosystem: String,
    root: RootInfo,
    #[serde(rename = "directCount")]
    direct_count: usize,
    #[serde(rename = "lockPackageCount")]
    lock_package_count: usize,
    packages: Vec<ListedPackage>,
}

#[derive(Debug)]
struct ResolvedList {
    root: RootInfo,
    direct: Vec<ListedPackage>,
    lock_packages: Vec<ListedPackage>,
    lock_package_count: usize,
    /// True when Cargo.lock was missing (locked versions unavailable).
    lock_missing: bool,
}

// ---------------------------------------------------------------------------
// Manifest + lock resolve
// ---------------------------------------------------------------------------

fn read_manifest_value(root: &Path) -> Result<toml::Value> {
    let toml_path = root.join("Cargo.toml");
    if !toml_path.exists() {
        return Err(miette::miette!(
            "Cargo.toml not found at work root ({}). `dependencies list` requires a package-at-root Cargo.toml; it does not fall back to the knowledge graph.",
            root.display()
        ));
    }
    let content = std::fs::read_to_string(&toml_path).into_diagnostic()?;
    toml::from_str(&content).into_diagnostic()
}

/// Root package identity from live `[package]`.
///
/// Name is required. Version is taken only when `[package].version` is a
/// string; workspace inheritance (`version.workspace = true`) leaves version
/// unresolved so the lock fallback in [`resolve_root_info`] can fill it.
fn root_identity_from_manifest(manifest: &toml::Value) -> Result<(String, Option<String>)> {
    let package = manifest.get("package").ok_or_else(|| {
        miette::miette!(
            "Cargo.toml has no [package] table (virtual workspace root). `dependencies list` scopes to the package at the work root only; run from a package directory or add a [package] section."
        )
    })?;
    let name = package
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| miette::miette!("Cargo.toml [package] is missing `name`"))?
        .to_string();
    // Only a plain string version counts; table form (`version.workspace = true`)
    // or a missing key leaves version unresolved for lock fallback.
    let version = package
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok((name, version))
}

/// Resolve root package version from `Cargo.lock` when the manifest did not
/// provide a string version (B1.7 lock fallback).
///
/// Preference: unique name match; if multiple, first entry with no `source`
/// (workspace / path package).
fn root_version_from_lock(packages: &[CargoLockPackage], root_name: &str) -> Option<String> {
    let matches: Vec<&CargoLockPackage> = packages.iter().filter(|p| p.name == root_name).collect();
    match matches.as_slice() {
        [] => None,
        [only] => Some(only.version.clone()),
        many => many
            .iter()
            .find(|p| p.source.is_none())
            .map(|p| p.version.clone()),
    }
}

/// Build root envelope: version from manifest string when present (`source:
/// "manifest"`), else from lock (`source: "lock"`). Errors clearly when
/// version cannot be resolved from either source.
fn resolve_root_info(
    manifest: &toml::Value,
    packages: &[CargoLockPackage],
    lock_missing: bool,
) -> Result<RootInfo> {
    let (name, manifest_version) = root_identity_from_manifest(manifest)?;
    if let Some(version) = manifest_version {
        return Ok(RootInfo {
            name,
            version,
            source: "manifest".to_string(),
        });
    }

    if !lock_missing && let Some(version) = root_version_from_lock(packages, &name) {
        return Ok(RootInfo {
            name,
            version,
            source: "lock".to_string(),
        });
    }

    Err(miette::miette!(
        "Could not resolve root package version for `{name}`: Cargo.toml [package].version is missing or not a string (e.g. `version.workspace = true`) and no matching package was found in Cargo.lock. Add a string version under [package], generate/update Cargo.lock, or run from a package directory that has both."
    ))
}

fn extract_req_and_optional(val: &toml::Value) -> (Option<String>, bool) {
    match val {
        toml::Value::String(s) => (Some(s.clone()), false),
        toml::Value::Table(t) => {
            let req = t
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let optional = t.get("optional").and_then(|v| v.as_bool()).unwrap_or(false);
            (req, optional)
        }
        _ => (None, false),
    }
}

fn collect_dep_table(
    table: Option<&toml::Value>,
    kind: DepKind,
    target: Option<&str>,
    out: &mut Vec<DeclaredDep>,
) {
    let Some(toml::Value::Table(map)) = table else {
        return;
    };
    for (name, val) in map {
        // Skip non-package keys that can appear under dep tables in rare cases;
        // real crates always use string or inline-table values.
        if !matches!(val, toml::Value::String(_) | toml::Value::Table(_)) {
            continue;
        }
        let (req, optional) = extract_req_and_optional(val);
        out.push(DeclaredDep {
            name: name.clone(),
            kind,
            optional,
            req,
            target: target.map(|s| s.to_string()),
        });
    }
}

fn collect_declared_deps(manifest: &toml::Value) -> Vec<DeclaredDep> {
    let mut out = Vec::new();

    collect_dep_table(
        manifest.get("dependencies"),
        DepKind::Normal,
        None,
        &mut out,
    );
    collect_dep_table(
        manifest.get("dev-dependencies"),
        DepKind::Dev,
        None,
        &mut out,
    );
    collect_dep_table(
        manifest.get("build-dependencies"),
        DepKind::Build,
        None,
        &mut out,
    );

    if let Some(toml::Value::Table(targets)) = manifest.get("target") {
        // Deterministic target key order
        let mut target_keys: Vec<&String> = targets.keys().collect();
        target_keys.sort();
        for key in target_keys {
            let Some(target_val) = targets.get(key) else {
                continue;
            };
            collect_dep_table(
                target_val.get("dependencies"),
                DepKind::Normal,
                Some(key.as_str()),
                &mut out,
            );
            collect_dep_table(
                target_val.get("dev-dependencies"),
                DepKind::Dev,
                Some(key.as_str()),
                &mut out,
            );
            collect_dep_table(
                target_val.get("build-dependencies"),
                DepKind::Build,
                Some(key.as_str()),
                &mut out,
            );
        }
    }

    // Sort: kind order normal → build → dev, then name, then target
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.target.cmp(&b.target))
    });
    out
}

/// Build `name → resolved version` from the root package's lock `dependencies` array.
/// Never name-joins all multi-version lock rows (H1).
fn root_deps_version_map(
    packages: &[CargoLockPackage],
    root_name: &str,
    root_version: &str,
) -> HashMap<String, String> {
    let root_pkg = packages
        .iter()
        .find(|p| p.name == root_name && p.version == root_version)
        .or_else(|| {
            let matches: Vec<_> = packages.iter().filter(|p| p.name == root_name).collect();
            if matches.len() == 1 {
                Some(matches[0])
            } else {
                None
            }
        });

    let Some(root_pkg) = root_pkg else {
        return HashMap::new();
    };
    let Some(deps) = root_pkg.dependencies.as_ref() else {
        return HashMap::new();
    };

    // Index lock packages by name for bare-name resolve
    let mut by_name: HashMap<&str, Vec<&CargoLockPackage>> = HashMap::new();
    for p in packages {
        by_name.entry(p.name.as_str()).or_default().push(p);
    }

    let mut map = HashMap::new();
    for dep_str in deps {
        let (name, ver_opt) = parse_lock_dep_string(dep_str);
        if name.is_empty() {
            continue;
        }
        if let Some(ver) = ver_opt {
            map.insert(name, ver);
        } else if let Some(cands) = by_name.get(name.as_str())
            && cands.len() == 1
        {
            map.insert(name, cands[0].version.clone());
            // Multi-version bare name: leave unresolved (honest null later)
        }
    }
    map
}

fn find_lock_source(packages: &[CargoLockPackage], name: &str, version: &str) -> Option<String> {
    packages
        .iter()
        .find(|p| p.name == name && p.version == version)
        .and_then(|p| p.source.clone())
}

fn resolve_list(root: &Path) -> Result<ResolvedList> {
    let manifest = read_manifest_value(root)?;
    let declared = collect_declared_deps(&manifest);

    let lock_path = root.join("Cargo.lock");
    let (packages, lock_missing) = if lock_path.exists() {
        let content = std::fs::read_to_string(&lock_path).into_diagnostic()?;
        (parse_cargo_lock(&content)?, false)
    } else {
        (Vec::new(), true)
    };

    let root_info = resolve_root_info(&manifest, &packages, lock_missing)?;

    let version_map = if lock_missing {
        HashMap::new()
    } else {
        root_deps_version_map(&packages, &root_info.name, &root_info.version)
    };

    let mut direct: Vec<ListedPackage> = declared
        .into_iter()
        .map(|d| {
            let locked = version_map.get(&d.name).cloned();
            let source = locked
                .as_ref()
                .and_then(|v| find_lock_source(&packages, &d.name, v));
            ListedPackage {
                name: d.name,
                version: locked,
                kind: Some(d.kind.as_str().to_string()),
                ecosystem: ECOSYSTEM.to_string(),
                source,
                optional: Some(d.optional),
                req: d.req,
                target: d.target,
            }
        })
        .collect();

    // Deterministic (already sorted via declared; re-sort for safety)
    direct.sort_by(|a, b| {
        let ka = kind_rank(a.kind.as_deref());
        let kb = kind_rank(b.kind.as_deref());
        ka.cmp(&kb)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.target.cmp(&b.target))
    });

    let mut lock_packages: Vec<ListedPackage> = packages
        .iter()
        .map(|p| ListedPackage {
            name: p.name.clone(),
            version: Some(p.version.clone()),
            kind: None,
            ecosystem: ECOSYSTEM.to_string(),
            source: p.source.clone(),
            optional: None,
            req: None,
            target: None,
        })
        .collect();
    lock_packages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));

    let lock_package_count = lock_packages.len();

    Ok(ResolvedList {
        root: root_info,
        direct,
        lock_packages,
        lock_package_count,
        lock_missing,
    })
}

fn kind_rank(kind: Option<&str>) -> u8 {
    match kind {
        Some("normal") => 0,
        Some("build") => 1,
        Some("dev") => 2,
        _ => 3,
    }
}

fn version_display(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("-")
}

fn source_display(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

pub fn execute_dependencies(args: DependenciesArgs) -> Result<()> {
    match args.command_or_default() {
        DependencySubcommands::List { json, verbose, all } => execute_list(json, verbose, all),
        DependencySubcommands::Audit { input, json } => execute_audit(input, json),
    }
}

fn execute_list(json: bool, verbose: bool, all: bool) -> Result<()> {
    // L2: non-git cwd with valid cargo project still works; no Cozo required.
    let layout = get_layout_or_cwd_if_not_git()?;
    let resolved = resolve_list(layout.root.as_std_path())?;

    let mode = if all { "all" } else { "direct" };
    let packages: Vec<ListedPackage> = if all {
        resolved.lock_packages.clone()
    } else {
        resolved.direct.clone()
    };

    if json {
        let envelope = DependenciesListEnvelope {
            schema_version: 1,
            mode: mode.to_string(),
            ecosystem: ECOSYSTEM.to_string(),
            root: resolved.root.clone(),
            direct_count: resolved.direct.len(),
            lock_package_count: resolved.lock_package_count,
            packages,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).into_diagnostic()?
        );
        return Ok(());
    }

    // Human output
    if all {
        println!(
            "{}",
            "Project Dependencies (all lock packages, from Cargo.lock)"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
        );
        let mut table = Table::new();
        table.set_header(vec!["Package", "Version", "Ecosystem", "Source"]);
        for dep in &packages {
            table.add_row(vec![
                dep.name.clone(),
                version_display(&dep.version).to_string(),
                dep.ecosystem.clone(),
                source_display(&dep.source).to_string(),
            ]);
        }
        println!("{}", table);
        println!(
            "\nLock packages total: {} · Direct declared: {}",
            resolved.lock_package_count,
            resolved.direct.len()
        );
    } else {
        let header = if resolved.lock_missing {
            "Project Dependencies (direct, from Cargo.toml; no Cargo.lock)"
        } else {
            "Project Dependencies (direct, from Cargo.toml + Cargo.lock)"
        };
        println!(
            "{}",
            header.if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().green()))
        );

        let mut table = Table::new();
        if verbose {
            table.set_header(vec![
                "Package",
                "Locked",
                "Kind",
                "Ecosystem",
                "Req",
                "Source",
            ]);
            for dep in &packages {
                table.add_row(vec![
                    dep.name.clone(),
                    version_display(&dep.version).to_string(),
                    dep.kind.clone().unwrap_or_else(|| "-".to_string()),
                    dep.ecosystem.clone(),
                    dep.req.as_deref().unwrap_or("-").to_string(),
                    source_display(&dep.source).to_string(),
                ]);
            }
        } else {
            table.set_header(vec!["Package", "Locked", "Kind", "Ecosystem"]);
            for dep in &packages {
                table.add_row(vec![
                    dep.name.clone(),
                    version_display(&dep.version).to_string(),
                    dep.kind.clone().unwrap_or_else(|| "-".to_string()),
                    dep.ecosystem.clone(),
                ]);
            }
        }
        println!("{}", table);

        if resolved.lock_missing {
            println!(
                "\nNote: Cargo.lock not found — locked versions shown as \"-\". Direct: {} · run `cargo generate-lockfile` to resolve.",
                resolved.direct.len()
            );
        } else {
            println!(
                "\nDirect: {} · Lock packages total: {} (use --all)",
                resolved.direct.len(),
                resolved.lock_package_count
            );
        }
        println!(
            "Root: {} {} (from {})",
            resolved.root.name, resolved.root.version, resolved.root.source
        );
    }

    Ok(())
}

fn execute_audit(input: String, json: bool) -> Result<()> {
    // Audit still requires git layout + Cozo write (populate KG).
    let layout = get_layout()?;

    let path = Path::new(&input);
    if !path.exists() {
        return Err(miette::miette!("Input file not found: {}", input));
    }

    let result = crate::index::advisories::OsvImporter::import_from_json(path)?;

    // Open writeable storage to populate KG
    let storage = StorageManager::init_with_layout(&layout)?;
    if let Some(cozo) = storage.cozo() {
        crate::index::advisories::OsvImporter::populate_kg(cozo, &result, "audit-tx")?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).into_diagnostic()?
        );
    } else {
        println!(
            "{}",
            "Security Advisory Audit (OSV)"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().red()))
        );
        let mut table = Table::new();
        table.set_header(vec!["Package", "Version", "Vulnerability", "Summary"]);

        for src_res in &result.results {
            for pkg_res in &src_res.packages {
                if let Some(vulns) = &pkg_res.vulnerabilities {
                    for vuln in vulns {
                        table.add_row(vec![
                            pkg_res.package.name.clone(),
                            pkg_res.package.version.clone(),
                            vuln.id
                                .if_supports_color(Stream::Stdout, |s| s.red())
                                .to_string(),
                            vuln.summary.as_deref().unwrap_or("-").to_string(),
                        ]);
                    }
                }
            }
        }
        println!("{}", table);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path, toml: &str, lock: Option<&str>) {
        std::fs::write(dir.join("Cargo.toml"), toml).expect("toml");
        if let Some(lock) = lock {
            std::fs::write(dir.join("Cargo.lock"), lock).expect("lock");
        }
    }

    #[test]
    fn h1_root_array_selects_new_version_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
multi = "2"
"#,
            Some(
                r#"
version = 3

[[package]]
name = "demo"
version = "0.1.0"
dependencies = [
 "multi 2.0.0",
]

[[package]]
name = "multi"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "multi"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
            ),
        );

        let resolved = resolve_list(root).expect("resolve");
        let multi: Vec<_> = resolved
            .direct
            .iter()
            .filter(|p| p.name == "multi")
            .collect();
        assert_eq!(multi.len(), 1);
        assert_eq!(multi[0].version.as_deref(), Some("2.0.0"));
        // Full lock still has both
        assert_eq!(resolved.lock_package_count, 3);
    }

    #[test]
    fn dual_kind_emits_one_row_per_kind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
shared = "1"

[dev-dependencies]
shared = "1"
"#,
            Some(
                r#"
version = 3

[[package]]
name = "demo"
version = "0.1.0"
dependencies = [
 "shared 1.2.3",
]

[[package]]
name = "shared"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
            ),
        );

        let resolved = resolve_list(root).expect("resolve");
        let rows: Vec<_> = resolved
            .direct
            .iter()
            .filter(|p| p.name == "shared")
            .collect();
        assert_eq!(rows.len(), 2);
        let kinds: Vec<_> = rows.iter().map(|r| r.kind.as_deref()).collect();
        assert!(kinds.contains(&Some("normal")));
        assert!(kinds.contains(&Some("dev")));
        for r in rows {
            assert_eq!(r.version.as_deref(), Some("1.2.3"));
        }
    }

    #[test]
    fn no_lock_honest_null_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
only-me = "1.0"
"#,
            None,
        );

        let resolved = resolve_list(root).expect("resolve");
        assert!(resolved.lock_missing);
        assert_eq!(resolved.direct.len(), 1);
        assert_eq!(resolved.direct[0].name, "only-me");
        assert!(resolved.direct[0].version.is_none());
        assert_eq!(resolved.root.version, "0.1.0");
    }

    #[test]
    fn missing_package_table_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[workspace]
members = ["a"]
"#,
            None,
        );
        let err = resolve_list(root).expect_err("virtual workspace");
        let msg = format!("{err}");
        assert!(
            msg.contains("[package]") || msg.contains("virtual"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn target_deps_collected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
base = "1"

[target.'cfg(unix)'.dependencies]
unix-only = "1"
"#,
            Some(
                r#"
version = 3

[[package]]
name = "demo"
version = "0.1.0"
dependencies = [
 "base 1.0.0",
 "unix-only 1.0.0",
]

[[package]]
name = "base"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "unix-only"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
            ),
        );

        let resolved = resolve_list(root).expect("resolve");
        assert_eq!(resolved.direct.len(), 2);
        let unix = resolved
            .direct
            .iter()
            .find(|p| p.name == "unix-only")
            .expect("unix-only");
        assert!(unix.target.as_deref().is_some_and(|t| t.contains("unix")));
        assert_eq!(unix.version.as_deref(), Some("1.0.0"));
    }

    /// B1.7: `version.workspace = true` leaves manifest version unresolved;
    /// lock root package supplies version and `root.source == "lock"`.
    #[test]
    fn workspace_version_falls_back_to_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version.workspace = true

[dependencies]
foo = "1"
"#,
            Some(
                r#"
version = 3

[[package]]
name = "demo"
version = "9.9.9"
dependencies = [
 "foo 1.0.0",
]

[[package]]
name = "foo"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
            ),
        );

        let resolved = resolve_list(root).expect("resolve");
        assert_eq!(resolved.root.name, "demo");
        assert_eq!(resolved.root.version, "9.9.9");
        assert_eq!(resolved.root.source, "lock");
        assert_eq!(resolved.direct.len(), 1);
        assert_eq!(resolved.direct[0].name, "foo");
        assert_eq!(resolved.direct[0].version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn workspace_version_without_lock_errors_clearly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version.workspace = true

[dependencies]
foo = "1"
"#,
            None,
        );

        let err = resolve_list(root).expect_err("no string version and no lock");
        let msg = format!("{err}");
        assert!(
            msg.contains("version") && (msg.contains("lock") || msg.contains("Cargo.lock")),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn manifest_string_version_keeps_manifest_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_fixture(
            root,
            r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
foo = "1"
"#,
            Some(
                r#"
version = 3

[[package]]
name = "demo"
version = "0.1.0"
dependencies = [
 "foo 1.0.0",
]

[[package]]
name = "foo"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
            ),
        );

        let resolved = resolve_list(root).expect("resolve");
        assert_eq!(resolved.root.version, "0.1.0");
        assert_eq!(resolved.root.source, "manifest");
    }
}
