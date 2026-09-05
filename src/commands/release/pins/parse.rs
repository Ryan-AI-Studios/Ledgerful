use super::types::{AdvisoryInput, HomebrewPin, LatestPins, LocalPins, McpPin, ScoopPin};
use serde_json::Value;
use std::path::Path;

pub(super) fn strip_leading_v(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

pub(super) fn normalize_digest(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix("sha256:")
        .or_else(|| s.strip_prefix("SHA256:"))
        .unwrap_or(s)
        .trim();
    s.to_ascii_lowercase()
}

fn is_archive_asset_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".sha256") || n.ends_with(".bundle") {
        return false;
    }
    n.ends_with(".tar.gz") || n.ends_with(".zip")
}

fn archive_name_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    let name = path.rsplit('/').next().filter(|s| !s.is_empty())?;
    Some(name.to_string())
}

fn ruby_string_assign(line: &str, key: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Pair each `url` with the following `sha256`. Do not eval Ruby.
pub(crate) fn parse_homebrew_formula(body: &str) -> HomebrewPin {
    let mut version = None;
    let mut hashes = std::collections::BTreeMap::new();
    let mut pending_url: Option<String> = None;
    for line in body.lines() {
        if let Some(v) = ruby_string_assign(line, "version") {
            version = Some(v);
        }
        if let Some(url) = ruby_string_assign(line, "url") {
            pending_url = Some(url);
            continue;
        }
        if let Some(sha) = ruby_string_assign(line, "sha256")
            && let Some(url) = pending_url.take()
            && let Some(name) = archive_name_from_url(&url)
        {
            hashes.insert(name, normalize_digest(&sha));
        }
    }
    HomebrewPin { version, hashes }
}

pub(crate) fn parse_scoop_manifest(body: &str) -> Option<ScoopPin> {
    let v: Value = serde_json::from_str(body).ok()?;
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let arch = v.get("architecture").and_then(|a| a.get("64bit"));
    let url = arch
        .and_then(|a| a.get("url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let hash = arch
        .and_then(|a| a.get("hash"))
        .and_then(Value::as_str)
        .map(normalize_digest)
        .filter(|s| !s.is_empty());
    if version.is_none() && url.is_none() && hash.is_none() {
        return None;
    }
    Some(ScoopPin { version, url, hash })
}

pub(crate) fn parse_mcp_package(body: &str) -> Option<McpPin> {
    let v: Value = serde_json::from_str(body).ok()?;
    parse_mcp_value(&v)
}

fn parse_mcp_value(v: &Value) -> Option<McpPin> {
    let version = v
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let ledgerful_engine_tag = v
        .get("ledgerfulEngineTag")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if version.is_none() && ledgerful_engine_tag.is_none() {
        return None;
    }
    Some(McpPin {
        version,
        ledgerful_engine_tag,
    })
}

/// npm `/latest` document. Missing `ledgerfulEngineTag` → `None` (unverified).
pub(crate) fn parse_npm_document(v: &Value) -> Option<McpPin> {
    let pin = parse_mcp_value(v)?;
    pin.ledgerful_engine_tag.as_ref()?;
    Some(pin)
}

/// Release JSON → tag + archive digests. Never reads `target_commitish`.
pub(crate) fn parse_release_pins(value: &Value) -> Option<LatestPins> {
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let version = strip_leading_v(&tag).to_string();
    let mut archives = std::collections::BTreeMap::new();
    if let Some(assets) = value.get("assets").and_then(Value::as_array) {
        for asset in assets {
            let Some(name) = asset.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !is_archive_asset_name(name) {
                continue;
            }
            let Some(digest) = asset.get("digest").and_then(Value::as_str) else {
                continue;
            };
            let hex = normalize_digest(digest);
            if hex.is_empty() {
                continue;
            }
            archives.insert(name.to_string(), hex);
        }
    }
    Some(LatestPins {
        tag,
        sha: None,
        version,
        archives,
    })
}

pub(super) fn parse_commit_sha(value: &Value) -> Option<String> {
    let sha = value.get("sha").and_then(Value::as_str).map(str::trim)?;
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha.to_ascii_lowercase())
    } else {
        None
    }
}

fn extract_object_field(source: &str, object: &str, field: &str) -> Option<String> {
    let obj_key = format!("{object}:");
    let start = source.find(&obj_key)?;
    let end = (start + 4000).min(source.len());
    let window = &source[start..end];
    let field_key = format!("{field}:");
    let fstart = window.find(&field_key)?;
    let after = window[fstart + field_key.len()..].trim_start();
    let after = after.strip_prefix('"')?;
    let qend = after.find('"')?;
    let value = after[..qend].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn extract_launch_facts(source: &str) -> (Option<String>, Option<String>) {
    (
        extract_object_field(source, "release", "tag"),
        extract_object_field(source, "mcpPackage", "engineTag"),
    )
}

pub(super) fn read_locals(root: &Path) -> LocalPins {
    let homebrew = std::fs::read_to_string(root.join("packaging/homebrew/ledgerful.rb"))
        .ok()
        .map(|body| parse_homebrew_formula(&body));
    let scoop = std::fs::read_to_string(root.join("packaging/scoop/ledgerful.json"))
        .ok()
        .and_then(|body| parse_scoop_manifest(&body));
    let mcp = std::fs::read_to_string(root.join("mcp-server/package.json"))
        .ok()
        .and_then(|body| parse_mcp_package(&body));
    LocalPins {
        homebrew,
        scoop,
        mcp,
    }
}

pub(super) fn read_advisory(engine_root: &Path) -> Option<AdvisoryInput> {
    let parent = engine_root.parent()?;
    let path = parent
        .join("ledgerful-web")
        .join("src")
        .join("lib")
        .join("content")
        .join("launch-facts.ts");
    if !path.is_file() {
        return None;
    }
    let source = std::fs::read_to_string(&path).ok()?;
    let (release_tag, mcp_engine_tag) = extract_launch_facts(&source);
    Some(AdvisoryInput {
        launch_facts_path: path.to_string_lossy().into_owned(),
        release_tag,
        mcp_engine_tag,
    })
}
