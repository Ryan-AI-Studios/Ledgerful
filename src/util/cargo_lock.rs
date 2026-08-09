//! Shared `Cargo.lock` parse types and helpers.
//!
//! Used by:
//! - `dependencies list` (live direct-deps resolution, H1 root-array join)
//! - `phase_cargo_dependencies` graph ingest (all lock packages into Cozo)
//!
//! Preserves the weakly-typed fallback path and whitespace dep-string split
//! used historically in `graph_loader` (track 0153 L6).

use miette::{IntoDiagnostic, Result};
use serde::Deserialize;

/// Top-level `Cargo.lock` document (`version` field ignored when present).
#[derive(Deserialize, Clone, Debug)]
pub struct CargoLockFile {
    #[serde(rename = "package")]
    pub packages: Vec<CargoLockPackage>,
}

/// One `[[package]]` entry from `Cargo.lock`.
#[derive(Deserialize, Clone, Debug)]
pub struct CargoLockPackage {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub dependencies: Option<Vec<String>>,
}

/// Parse `Cargo.lock` content into package rows.
///
/// Attempts strongly typed deserialization first; on failure falls back to a
/// weakly typed `toml` → JSON walk so partial/older lock shapes still yield
/// packages (same discipline as the former `graph_loader` path).
pub fn parse_cargo_lock(content: &str) -> Result<Vec<CargoLockPackage>> {
    // Attempt strongly typed deserialization first
    let typed_lock: Option<CargoLockFile> = toml::from_str(content).ok();

    if let Some(lock) = typed_lock {
        return Ok(lock.packages);
    }

    tracing::warn!(
        "Cargo.lock: Failed to parse with strongly typed schema; falling back to weakly typed parsing."
    );
    let value: serde_json::Value = toml::from_str(content).into_diagnostic()?;
    let packages = value.get("package").and_then(|p| p.as_array());
    if let Some(pkgs) = packages {
        Ok(pkgs
            .iter()
            .map(|p| CargoLockPackage {
                name: p
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string(),
                version: p
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0.0.0")
                    .to_string(),
                source: p.get("source").and_then(|s| s.as_str()).map(String::from),
                dependencies: p
                    .get("dependencies")
                    .and_then(|d| d.as_array())
                    .map(|deps| {
                        deps.iter()
                            .filter_map(|d| d.as_str().map(String::from))
                            .collect()
                    }),
            })
            .collect())
    } else {
        Ok(Vec::new())
    }
}

/// Parse a Cargo.lock dependency string (`"crate"`, `"crate 1.2.3"`, or
/// source-bearing forms) into `(name, optional_version)`.
///
/// Matches `graph_loader` discipline: `parts[0]` = name, `parts[1]` = version
/// when present (further tokens such as registry source are ignored here).
pub fn parse_lock_dep_string(dep_str: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = dep_str.split_whitespace().collect();
    if parts.is_empty() {
        return (String::new(), None);
    }
    let name = parts[0].to_string();
    let version = parts.get(1).map(|s| s.to_string());
    (name, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typed_lock_packages() {
        let content = r#"
version = 3

[[package]]
name = "foo"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "bar 2.0.0",
 "baz",
]

[[package]]
name = "bar"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "baz"
version = "0.1.0"
"#;
        let pkgs = parse_cargo_lock(content).expect("parse");
        assert_eq!(pkgs.len(), 3);
        assert_eq!(pkgs[0].name, "foo");
        assert_eq!(pkgs[0].version, "1.0.0");
        assert!(pkgs[0].source.is_some());
        let deps = pkgs[0].dependencies.as_ref().expect("deps");
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn parse_dep_string_name_and_version() {
        let (name, ver) = parse_lock_dep_string("owo-colors 4.3.0");
        assert_eq!(name, "owo-colors");
        assert_eq!(ver.as_deref(), Some("4.3.0"));

        let (name, ver) = parse_lock_dep_string("serde");
        assert_eq!(name, "serde");
        assert!(ver.is_none());

        let (name, ver) = parse_lock_dep_string(
            "foo 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)",
        );
        assert_eq!(name, "foo");
        assert_eq!(ver.as_deref(), Some("1.0.0"));
    }
}
