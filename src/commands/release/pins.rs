//! Parsers, classifier, and fetch for `ledgerful release pins` (0201).
//!
//! Own Latest fetch (`tag_name` + archive `assets[].digest`; optional peel).
//! Do not call SHA-required `fetch_github_latest` as the only Latest source.

use crate::commands::doctor::{is_ledgerful_engine_worktree, shorten_sha_for_display};
use crate::output::table::{Table, apply_table_style, resolve_table_style};
use crate::util::network::network_disabled_from_env;
use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub(crate) const GITHUB_OWNER_REPO: &str = "Ryan-AI-Studios/Ledgerful";
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";
pub(crate) const HOMEBREW_TAP_REPO: &str = "Ryan-AI-Studios/homebrew-tap";
pub(crate) const HOMEBREW_TAP_PATH: &str = "ledgerful.rb";
pub(crate) const SCOOP_BUCKET_REPO: &str = "Ryan-AI-Studios/scoop-bucket";
pub(crate) const SCOOP_BUCKET_PATH: &str = "ledgerful.json";
pub(crate) const NPM_LATEST_URL: &str = "https://registry.npmjs.org/@ledgerful/mcp-server/latest";

const GITHUB_API_VERSION: &str = "2022-11-28";
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);
const KIND: &str = "releasePins";
const SCHEMA_VERSION: u32 = 1;

const ARCHIVE_DARWIN_ARM: &str = "ledgerful-aarch64-apple-darwin.tar.gz";
const ARCHIVE_DARWIN_X64: &str = "ledgerful-x86_64-apple-darwin.tar.gz";
const ARCHIVE_LINUX_X64: &str = "ledgerful-x86_64-unknown-linux-gnu.tar.gz";
const ARCHIVE_WINDOWS: &str = "ledgerful-x86_64-pc-windows-msvc.zip";
const HOMEBREW_ARCHIVES: &[&str] = &[ARCHIVE_DARWIN_ARM, ARCHIVE_DARWIN_X64, ARCHIVE_LINUX_X64];

const ID_PACKAGING_HOMEBREW: &str = "packaging.homebrew";
const ID_PACKAGING_SCOOP: &str = "packaging.scoop";
const ID_MCP_INTREE: &str = "mcp.inTree";
const ID_REMOTE_TAP: &str = "remote.homebrew-tap";
const ID_REMOTE_BUCKET: &str = "remote.scoop-bucket";
const ID_MCP_NPM: &str = "mcp.npm";

/// Pin keys from GitHub Latest: tag + archive digests. SHA is optional (peel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatestPins {
    pub tag: String,
    pub sha: Option<String>,
    pub version: String,
    pub archives: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PinStatus {
    Skipped,
    Unverified,
    Drift,
    Match,
}

impl PinStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
            Self::Drift => "drift",
            Self::Match => "match",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct Surface {
    pub id: String,
    pub status: PinStatus,
    pub local: Value,
    pub expected: Value,
    pub remote: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LatestJson {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Advisory {
    pub launch_facts_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_engine_tag: Option<String>,
    pub status: PinStatus,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleasePinsEnvelope {
    pub schema_version: u32,
    pub kind: &'static str,
    pub status: PinStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<LatestJson>,
    pub surfaces: Vec<Surface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advisory: Option<Advisory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HomebrewPin {
    pub version: Option<String>,
    pub hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ScoopPin {
    pub version: Option<String>,
    pub url: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct McpPin {
    pub version: Option<String>,
    pub ledgerful_engine_tag: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalPins {
    pub homebrew: Option<HomebrewPin>,
    pub scoop: Option<ScoopPin>,
    pub mcp: Option<McpPin>,
}

#[derive(Debug, Clone)]
pub(crate) enum RemoteFact<T> {
    Unverified,
    Value(T),
}

#[derive(Debug, Clone)]
pub(crate) struct RemotePins {
    pub homebrew_tap: RemoteFact<HomebrewPin>,
    pub scoop_bucket: RemoteFact<ScoopPin>,
    pub npm: RemoteFact<McpPin>,
}

impl RemotePins {
    fn unverified() -> Self {
        Self {
            homebrew_tap: RemoteFact::Unverified,
            scoop_bucket: RemoteFact::Unverified,
            npm: RemoteFact::Unverified,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AdvisoryInput {
    pub launch_facts_path: String,
    pub release_tag: Option<String>,
    pub mcp_engine_tag: Option<String>,
}

pub(crate) struct ClassifyPinsInput<'a> {
    pub is_engine: bool,
    pub latest: Option<&'a LatestPins>,
    pub fetch_error: bool,
    pub locals: &'a LocalPins,
    pub remotes: &'a RemotePins,
    pub advisory: Option<AdvisoryInput>,
}

#[derive(Debug, Clone)]
pub(crate) struct PinFetchEndpoints {
    pub github_api_base: String,
    pub npm_latest_url: String,
}

impl PinFetchEndpoints {
    pub(crate) fn production() -> Self {
        Self {
            github_api_base: GITHUB_API_BASE.to_string(),
            npm_latest_url: NPM_LATEST_URL.to_string(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum PinFetchError {
    NetworkDisabled,
    Http { status: Option<u16>, detail: String },
    InvalidBody(&'static str),
}

impl std::fmt::Display for PinFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkDisabled => write!(f, "LEDGERFUL_NO_NETWORK"),
            Self::Http { status, detail } => match status {
                Some(code) => write!(f, "HTTP {code}: {detail}"),
                None => write!(f, "transport: {detail}"),
            },
            Self::InvalidBody(msg) => write!(f, "invalid body: {msg}"),
        }
    }
}

pub(crate) struct PinFetchBundle {
    pub latest: Result<LatestPins, PinFetchError>,
    pub tap: Result<String, PinFetchError>,
    pub bucket: Result<String, PinFetchError>,
    pub npm: Result<Value, PinFetchError>,
}

pub(crate) fn exit_code_for(status: PinStatus) -> i32 {
    match status {
        PinStatus::Match => 0,
        PinStatus::Drift => 1,
        PinStatus::Skipped | PinStatus::Unverified => 2,
    }
}

fn user_agent() -> String {
    format!("ledgerful/{}", env!("CARGO_PKG_VERSION"))
}

fn strip_leading_v(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

fn version_eq(a: &str, b: &str) -> bool {
    strip_leading_v(a.trim()) == strip_leading_v(b.trim())
}

fn normalize_digest(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix("sha256:")
        .or_else(|| s.strip_prefix("SHA256:"))
        .unwrap_or(s)
        .trim();
    s.to_ascii_lowercase()
}

fn hash_eq(a: &str, b: &str) -> bool {
    let a = normalize_digest(a);
    let b = normalize_digest(b);
    !a.is_empty() && a == b
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
    let mut hashes = BTreeMap::new();
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
    let mut archives = BTreeMap::new();
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

fn parse_commit_sha(value: &Value) -> Option<String> {
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

fn homebrew_json(pin: &HomebrewPin) -> Value {
    let hashes: BTreeMap<&str, &str> = pin
        .hashes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    json!({
        "version": pin.version,
        "hashes": hashes,
    })
}

fn expected_homebrew_json(latest: &LatestPins) -> Value {
    let mut hashes = BTreeMap::new();
    for name in HOMEBREW_ARCHIVES {
        if let Some(h) = latest.archives.get(*name) {
            hashes.insert(*name, h.as_str());
        }
    }
    json!({
        "version": latest.version,
        "hashes": hashes,
    })
}

fn scoop_json(pin: &ScoopPin) -> Value {
    json!({
        "version": pin.version,
        "url": pin.url,
        "hash": pin.hash,
    })
}

fn expected_scoop_url(tag: &str) -> String {
    format!("https://github.com/{GITHUB_OWNER_REPO}/releases/download/{tag}/{ARCHIVE_WINDOWS}")
}

fn expected_scoop_json(latest: &LatestPins) -> Value {
    json!({
        "version": latest.version,
        "url": expected_scoop_url(&latest.tag),
        "hash": latest.archives.get(ARCHIVE_WINDOWS),
    })
}

fn mcp_json(pin: &McpPin) -> Value {
    json!({
        "version": pin.version,
        "ledgerfulEngineTag": pin.ledgerful_engine_tag,
    })
}

fn homebrew_expected_complete(latest: &LatestPins) -> bool {
    HOMEBREW_ARCHIVES
        .iter()
        .all(|name| latest.archives.contains_key(*name))
}

fn homebrew_matches(local: &HomebrewPin, latest: &LatestPins) -> bool {
    let Some(ver) = local.version.as_deref() else {
        return false;
    };
    if !version_eq(ver, &latest.version) {
        return false;
    }
    HOMEBREW_ARCHIVES.iter().all(|name| {
        match (
            local.hashes.get(*name).map(String::as_str),
            latest.archives.get(*name).map(String::as_str),
        ) {
            (Some(a), Some(b)) => hash_eq(a, b),
            _ => false,
        }
    })
}

fn scoop_matches(local: &ScoopPin, latest: &LatestPins) -> bool {
    let Some(ver) = local.version.as_deref() else {
        return false;
    };
    if !version_eq(ver, &latest.version) {
        return false;
    }
    let Some(expected_hash) = latest.archives.get(ARCHIVE_WINDOWS) else {
        return false;
    };
    let Some(local_hash) = local.hash.as_deref() else {
        return false;
    };
    if !hash_eq(local_hash, expected_hash) {
        return false;
    }
    match local.url.as_deref() {
        Some(url) => url == expected_scoop_url(&latest.tag),
        None => false,
    }
}

fn mcp_tag_matches(pin: &McpPin, latest: &LatestPins) -> bool {
    pin.ledgerful_engine_tag
        .as_deref()
        .is_some_and(|t| t == latest.tag)
}

fn surface(id: &str, status: PinStatus, local: Value, expected: Value, remote: Value) -> Surface {
    Surface {
        id: id.to_string(),
        status,
        local,
        expected,
        remote,
    }
}

fn classify_packaging_homebrew(
    latest: Option<&LatestPins>,
    local: Option<&HomebrewPin>,
) -> Surface {
    let local_json = local.map(homebrew_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_homebrew_json(latest);
    let Some(local) = local else {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    if !homebrew_expected_complete(latest) {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Unverified,
            homebrew_json(local),
            expected,
            Value::Null,
        );
    }
    let status = if homebrew_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_PACKAGING_HOMEBREW,
        status,
        homebrew_json(local),
        expected,
        Value::Null,
    )
}

fn classify_packaging_scoop(latest: Option<&LatestPins>, local: Option<&ScoopPin>) -> Surface {
    let local_json = local.map(scoop_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_scoop_json(latest);
    let Some(local) = local else {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    if !latest.archives.contains_key(ARCHIVE_WINDOWS) {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Unverified,
            scoop_json(local),
            expected,
            Value::Null,
        );
    }
    let status = if scoop_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_PACKAGING_SCOOP,
        status,
        scoop_json(local),
        expected,
        Value::Null,
    )
}

fn classify_mcp_intree(latest: Option<&LatestPins>, local: Option<&McpPin>) -> Surface {
    let local_json = local.map(mcp_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_MCP_INTREE,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = json!({ "ledgerfulEngineTag": latest.tag });
    let Some(local) = local else {
        return surface(
            ID_MCP_INTREE,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    let status = if mcp_tag_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_MCP_INTREE,
        status,
        mcp_json(local),
        expected,
        Value::Null,
    )
}

fn classify_remote_homebrew(
    latest: Option<&LatestPins>,
    remote: &RemoteFact<HomebrewPin>,
) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_REMOTE_TAP,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_homebrew_json(latest);
    match remote {
        RemoteFact::Unverified => surface(
            ID_REMOTE_TAP,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            if !homebrew_expected_complete(latest) {
                return surface(
                    ID_REMOTE_TAP,
                    PinStatus::Unverified,
                    Value::Null,
                    expected,
                    homebrew_json(pin),
                );
            }
            let status = if homebrew_matches(pin, latest) {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(
                ID_REMOTE_TAP,
                status,
                Value::Null,
                expected,
                homebrew_json(pin),
            )
        }
    }
}

fn classify_remote_scoop(latest: Option<&LatestPins>, remote: &RemoteFact<ScoopPin>) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_REMOTE_BUCKET,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_scoop_json(latest);
    match remote {
        RemoteFact::Unverified => surface(
            ID_REMOTE_BUCKET,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            if !latest.archives.contains_key(ARCHIVE_WINDOWS) {
                return surface(
                    ID_REMOTE_BUCKET,
                    PinStatus::Unverified,
                    Value::Null,
                    expected,
                    scoop_json(pin),
                );
            }
            let status = if scoop_matches(pin, latest) {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(
                ID_REMOTE_BUCKET,
                status,
                Value::Null,
                expected,
                scoop_json(pin),
            )
        }
    }
}

fn classify_mcp_npm(
    latest: Option<&LatestPins>,
    remote: &RemoteFact<McpPin>,
    intree: Option<&McpPin>,
) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_MCP_NPM,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = json!({
        "ledgerfulEngineTag": latest.tag,
        "version": intree.and_then(|m| m.version.as_deref()),
    });
    match remote {
        RemoteFact::Unverified => surface(
            ID_MCP_NPM,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            let tag_ok = mcp_tag_matches(pin, latest);
            let version_ok = match intree.and_then(|m| m.version.as_deref()) {
                Some(expected_ver) => pin.version.as_deref() == Some(expected_ver),
                None => true,
            };
            let status = if tag_ok && version_ok {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(ID_MCP_NPM, status, Value::Null, expected, mcp_json(pin))
        }
    }
}

fn classify_advisory(latest: Option<&LatestPins>, adv: Option<AdvisoryInput>) -> Option<Advisory> {
    let adv = adv?;
    let status = match latest {
        None => PinStatus::Unverified,
        Some(latest) => {
            let tag_ok = adv.release_tag.as_deref() == Some(latest.tag.as_str());
            let mcp_ok = adv.mcp_engine_tag.as_deref() == Some(latest.tag.as_str());
            if tag_ok && mcp_ok {
                PinStatus::Match
            } else {
                PinStatus::Drift
            }
        }
    };
    Some(Advisory {
        launch_facts_path: adv.launch_facts_path,
        release_tag: adv.release_tag,
        mcp_engine_tag: adv.mcp_engine_tag,
        status,
    })
}

fn overall_from_surfaces(surfaces: &[Surface]) -> PinStatus {
    if surfaces.iter().any(|s| s.status == PinStatus::Drift) {
        PinStatus::Drift
    } else if surfaces.iter().any(|s| s.status == PinStatus::Unverified) {
        PinStatus::Unverified
    } else {
        PinStatus::Match
    }
}

fn latest_json(latest: &LatestPins) -> LatestJson {
    LatestJson {
        tag: latest.tag.clone(),
        sha: latest
            .sha
            .as_deref()
            .map(shorten_sha_for_display)
            .filter(|s| !s.is_empty()),
    }
}

/// Pure classifier. Fetch is [`fetch_latest_pins`].
pub(crate) fn classify_pins(input: ClassifyPinsInput<'_>) -> ReleasePinsEnvelope {
    if !input.is_engine {
        return ReleasePinsEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: KIND,
            status: PinStatus::Skipped,
            latest: None,
            surfaces: Vec::new(),
            advisory: None,
        };
    }

    let latest = input.latest.filter(|_| !input.fetch_error);
    let mut surfaces = vec![
        classify_packaging_homebrew(latest, input.locals.homebrew.as_ref()),
        classify_packaging_scoop(latest, input.locals.scoop.as_ref()),
        classify_mcp_intree(latest, input.locals.mcp.as_ref()),
        classify_remote_homebrew(latest, &input.remotes.homebrew_tap),
        classify_remote_scoop(latest, &input.remotes.scoop_bucket),
        classify_mcp_npm(latest, &input.remotes.npm, input.locals.mcp.as_ref()),
    ];
    surfaces.sort_by(|a, b| a.id.cmp(&b.id));

    let status = if latest.is_none() {
        PinStatus::Unverified
    } else {
        overall_from_surfaces(&surfaces)
    };
    let advisory = classify_advisory(latest, input.advisory);

    ReleasePinsEnvelope {
        schema_version: SCHEMA_VERSION,
        kind: KIND,
        status,
        latest: latest.map(latest_json),
        surfaces,
        advisory,
    }
}

fn map_ureq(err: ureq::Error) -> PinFetchError {
    match err {
        ureq::Error::Status(code, _resp) => PinFetchError::Http {
            status: Some(code),
            detail: format!("HTTP {code}"),
        },
        ureq::Error::Transport(inner) => PinFetchError::Http {
            status: None,
            detail: inner.to_string(),
        },
    }
}

fn github_get_json(agent: &ureq::Agent, url: &str) -> Result<Value, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(map_ureq)?;
    resp.into_json()
        .map_err(|_| PinFetchError::InvalidBody("invalid JSON"))
}

fn github_get_raw(agent: &ureq::Agent, url: &str) -> Result<String, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github.raw+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(map_ureq)?;
    resp.into_string()
        .map_err(|_| PinFetchError::InvalidBody("invalid text body"))
}

fn npm_get_json(agent: &ureq::Agent, url: &str) -> Result<Value, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/json")
        .call()
        .map_err(map_ureq)?;
    resp.into_json()
        .map_err(|_| PinFetchError::InvalidBody("invalid JSON"))
}

fn disabled_bundle() -> PinFetchBundle {
    PinFetchBundle {
        latest: Err(PinFetchError::NetworkDisabled),
        tap: Err(PinFetchError::NetworkDisabled),
        bucket: Err(PinFetchError::NetworkDisabled),
        npm: Err(PinFetchError::NetworkDisabled),
    }
}

fn try_peel_sha(agent: &ureq::Agent, base: &str, tag: &str) -> Option<String> {
    if network_disabled_from_env() {
        return None;
    }
    let url = format!("{base}/repos/{GITHUB_OWNER_REPO}/commits/{tag}");
    let json = github_get_json(agent, &url).ok()?;
    parse_commit_sha(&json)
}

fn fetch_remotes(
    agent: &ureq::Agent,
    endpoints: &PinFetchEndpoints,
) -> (
    Result<String, PinFetchError>,
    Result<String, PinFetchError>,
    Result<Value, PinFetchError>,
) {
    let base = endpoints.github_api_base.trim_end_matches('/').to_string();
    let npm_url = endpoints.npm_latest_url.clone();
    let tap_url = format!("{base}/repos/{HOMEBREW_TAP_REPO}/contents/{HOMEBREW_TAP_PATH}");
    let bucket_url = format!("{base}/repos/{SCOOP_BUCKET_REPO}/contents/{SCOOP_BUCKET_PATH}");
    let tap_agent = agent.clone();
    let bucket_agent = agent.clone();
    let npm_agent = agent.clone();
    std::thread::scope(|s| {
        let tap_h = s.spawn(|| github_get_raw(&tap_agent, &tap_url));
        let bucket_h = s.spawn(|| github_get_raw(&bucket_agent, &bucket_url));
        let npm_h = s.spawn(|| npm_get_json(&npm_agent, &npm_url));
        let tap = tap_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "tap thread panicked".to_string(),
            })
        });
        let bucket = bucket_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "bucket thread panicked".to_string(),
            })
        });
        let npm = npm_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "npm thread panicked".to_string(),
            })
        });
        (tap, bucket, npm)
    })
}

/// Fetch GitHub Latest pin keys (tag + archive digests) and remotes.
///
/// Peel SHA is optional: 404/invalid peel still returns tag + digests.
/// Never calls `fetch_github_latest`. Never sends `GITHUB_TOKEN`. No retry.
pub(crate) fn fetch_latest_pins(endpoints: &PinFetchEndpoints) -> PinFetchBundle {
    if network_disabled_from_env() {
        return disabled_bundle();
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .build();

    let base = endpoints.github_api_base.trim_end_matches('/');
    let release_url = format!("{base}/repos/{GITHUB_OWNER_REPO}/releases/latest");
    let release_json = match github_get_json(&agent, &release_url) {
        Ok(v) => v,
        Err(e) => {
            return PinFetchBundle {
                latest: Err(e),
                tap: Err(PinFetchError::InvalidBody("latest unavailable")),
                bucket: Err(PinFetchError::InvalidBody("latest unavailable")),
                npm: Err(PinFetchError::InvalidBody("latest unavailable")),
            };
        }
    };
    let mut latest = match parse_release_pins(&release_json) {
        Some(p) => p,
        None => {
            return PinFetchBundle {
                latest: Err(PinFetchError::InvalidBody("empty tag_name")),
                tap: Err(PinFetchError::InvalidBody("latest unavailable")),
                bucket: Err(PinFetchError::InvalidBody("latest unavailable")),
                npm: Err(PinFetchError::InvalidBody("latest unavailable")),
            };
        }
    };
    latest.sha = try_peel_sha(&agent, base, &latest.tag);

    let (tap, bucket, npm) = fetch_remotes(&agent, endpoints);
    PinFetchBundle {
        latest: Ok(latest),
        tap,
        bucket,
        npm,
    }
}

fn read_locals(root: &Path) -> LocalPins {
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

fn read_advisory(engine_root: &Path) -> Option<AdvisoryInput> {
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

fn remotes_from_fetch(fetched: &PinFetchBundle) -> RemotePins {
    let homebrew_tap = match &fetched.tap {
        Ok(body) => RemoteFact::Value(parse_homebrew_formula(body)),
        Err(_) => RemoteFact::Unverified,
    };
    let scoop_bucket = match &fetched.bucket {
        Ok(body) => match parse_scoop_manifest(body) {
            Some(pin) => RemoteFact::Value(pin),
            None => RemoteFact::Unverified,
        },
        Err(_) => RemoteFact::Unverified,
    };
    let npm = match &fetched.npm {
        Ok(value) => match parse_npm_document(value) {
            Some(pin) => RemoteFact::Value(pin),
            None => RemoteFact::Unverified,
        },
        Err(_) => RemoteFact::Unverified,
    };
    RemotePins {
        homebrew_tap,
        scoop_bucket,
        npm,
    }
}

fn resolve_engine_root() -> Result<Option<std::path::PathBuf>> {
    let current_dir = std::env::current_dir().into_diagnostic()?;
    if let Ok(layout) = crate::commands::helpers::get_layout() {
        let root = layout.root.as_std_path();
        if is_ledgerful_engine_worktree(root) {
            return Ok(Some(layout.root.into_std_path_buf()));
        }
    }
    if is_ledgerful_engine_worktree(&current_dir) {
        return Ok(Some(current_dir));
    }
    Ok(None)
}

pub(crate) fn collect_release_pins() -> Result<ReleasePinsEnvelope> {
    collect_release_pins_with(&PinFetchEndpoints::production())
}

pub(crate) fn collect_release_pins_with(
    endpoints: &PinFetchEndpoints,
) -> Result<ReleasePinsEnvelope> {
    let Some(engine_root) = resolve_engine_root()? else {
        return Ok(classify_pins(ClassifyPinsInput {
            is_engine: false,
            latest: None,
            fetch_error: false,
            locals: &LocalPins::default(),
            remotes: &RemotePins::unverified(),
            advisory: None,
        }));
    };

    let locals = read_locals(&engine_root);
    let advisory = read_advisory(&engine_root);
    let fetched = fetch_latest_pins(endpoints);
    let latest_owned = fetched.latest.as_ref().ok().cloned();
    let fetch_error = fetched.latest.is_err();
    let remotes = remotes_from_fetch(&fetched);
    Ok(classify_pins(ClassifyPinsInput {
        is_engine: true,
        latest: latest_owned.as_ref(),
        fetch_error,
        locals: &locals,
        remotes: &remotes,
        advisory,
    }))
}

fn compact_cell(v: &Value) -> String {
    if v.is_null() {
        return "-".to_string();
    }
    if let Some(tag) = v.get("ledgerfulEngineTag").and_then(Value::as_str) {
        return tag.to_string();
    }
    if let Some(ver) = v.get("version").and_then(Value::as_str) {
        return ver.to_string();
    }
    if let Some(hash) = v.get("hash").and_then(Value::as_str) {
        return hash.chars().take(12).collect();
    }
    if v.as_object().is_some_and(|o| o.is_empty()) {
        return "-".to_string();
    }
    "-".to_string()
}

fn print_human_table(envelope: &ReleasePinsEnvelope) {
    if envelope.status == PinStatus::Skipped {
        println!("Not a Ledgerful engine worktree; release pins is engine-only.");
        println!("Overall: {}", envelope.status.as_str());
        return;
    }
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Surface", "Status", "Local", "Expected", "Remote"]);
    for s in &envelope.surfaces {
        table.add_row(vec![
            s.id.clone(),
            s.status.as_str().to_string(),
            compact_cell(&s.local),
            compact_cell(&s.expected),
            compact_cell(&s.remote),
        ]);
    }
    println!("{table}");
    println!("Overall: {}", envelope.status.as_str());
}

pub(crate) fn emit_release_pins(envelope: &ReleasePinsEnvelope, json: bool) -> Result<()> {
    if json {
        let body = serde_json::to_string_pretty(envelope).into_diagnostic()?;
        println!("{body}");
    } else {
        print_human_table(envelope);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    const HASH_DARWIN_ARM: &str =
        "550cbc61bde812017a5fc19d61e00dac7cd59ac14fed0a81bf7dda5ce22d29de";
    const HASH_DARWIN_X64: &str =
        "149f14faf2f153c1682505e32ca49cca6a35f2375547cb3cef4de8fa5810a614";
    const HASH_LINUX_X64: &str = "817debe3fa56db93aeb1273b1648a2b6370a50f3c69150caf8cdc423d9c1930d";
    const HASH_WINDOWS: &str = "99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca";
    const HASH_SIDECAR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const LATEST_TAG: &str = "v0.2.10";
    const LATEST_SHA: &str = "c4a2308fe98548899105e33ff38232dfb229ec02";
    const MCP_VERSION: &str = "0.1.19";

    fn scoop_url_for(tag: &str) -> String {
        expected_scoop_url(tag)
    }

    fn published_latest() -> LatestPins {
        let mut archives = BTreeMap::new();
        archives.insert(ARCHIVE_DARWIN_ARM.to_string(), HASH_DARWIN_ARM.to_string());
        archives.insert(ARCHIVE_DARWIN_X64.to_string(), HASH_DARWIN_X64.to_string());
        archives.insert(ARCHIVE_LINUX_X64.to_string(), HASH_LINUX_X64.to_string());
        archives.insert(ARCHIVE_WINDOWS.to_string(), HASH_WINDOWS.to_string());
        LatestPins {
            tag: LATEST_TAG.to_string(),
            sha: Some(LATEST_SHA.to_string()),
            version: "0.2.10".to_string(),
            archives,
        }
    }

    fn matching_homebrew() -> HomebrewPin {
        let mut hashes = BTreeMap::new();
        hashes.insert(ARCHIVE_DARWIN_ARM.to_string(), HASH_DARWIN_ARM.to_string());
        hashes.insert(ARCHIVE_DARWIN_X64.to_string(), HASH_DARWIN_X64.to_string());
        hashes.insert(ARCHIVE_LINUX_X64.to_string(), HASH_LINUX_X64.to_string());
        HomebrewPin {
            version: Some("0.2.10".to_string()),
            hashes,
        }
    }

    fn matching_scoop() -> ScoopPin {
        ScoopPin {
            version: Some("0.2.10".to_string()),
            url: Some(scoop_url_for(LATEST_TAG)),
            hash: Some(HASH_WINDOWS.to_string()),
        }
    }

    fn matching_mcp() -> McpPin {
        McpPin {
            version: Some(MCP_VERSION.to_string()),
            ledgerful_engine_tag: Some(LATEST_TAG.to_string()),
        }
    }

    fn matching_locals() -> LocalPins {
        LocalPins {
            homebrew: Some(matching_homebrew()),
            scoop: Some(matching_scoop()),
            mcp: Some(matching_mcp()),
        }
    }

    fn matching_remotes() -> RemotePins {
        RemotePins {
            homebrew_tap: RemoteFact::Value(matching_homebrew()),
            scoop_bucket: RemoteFact::Value(matching_scoop()),
            npm: RemoteFact::Value(matching_mcp()),
        }
    }

    fn classify_engine(
        latest: Option<&LatestPins>,
        locals: &LocalPins,
        remotes: &RemotePins,
        fetch_error: bool,
        advisory: Option<AdvisoryInput>,
    ) -> ReleasePinsEnvelope {
        classify_pins(ClassifyPinsInput {
            is_engine: true,
            latest,
            fetch_error,
            locals,
            remotes,
            advisory,
        })
    }

    fn surface_status(env: &ReleasePinsEnvelope, id: &str) -> PinStatus {
        env.surfaces
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.status)
            .unwrap_or_else(|| panic!("missing surface {id}"))
    }

    fn ids(env: &ReleasePinsEnvelope) -> Vec<&str> {
        env.surfaces.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn classify_t0_consumer_skipped() {
        let latest = published_latest();
        let env = classify_pins(ClassifyPinsInput {
            is_engine: false,
            latest: Some(&latest),
            fetch_error: false,
            locals: &matching_locals(),
            remotes: &matching_remotes(),
            advisory: None,
        });
        assert_eq!(env.status, PinStatus::Skipped);
        assert_eq!(exit_code_for(env.status), 2);
        assert!(env.surfaces.is_empty());
        let v = serde_json::to_value(&env).expect("json");
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["kind"], "releasePins");
        assert_eq!(v["status"], "skipped");
        assert!(v.get("latest").is_none());
        assert_eq!(v["surfaces"].as_array().map(Vec::len), Some(0));
        assert!(v.get("advisory").is_none());
    }

    #[test]
    fn classify_t1_no_network_unverified() {
        let env = classify_engine(
            None,
            &matching_locals(),
            &RemotePins::unverified(),
            true,
            None,
        );
        assert_eq!(env.status, PinStatus::Unverified);
        assert_eq!(exit_code_for(env.status), 2);
        let v = serde_json::to_value(&env).expect("json");
        assert_eq!(v["status"], "unverified");
        assert!(v.get("latest").is_none());
        assert_eq!(ids(&env).len(), 6);
        for s in &env.surfaces {
            assert_eq!(s.status, PinStatus::Unverified);
        }
    }

    #[test]
    fn classify_t2_invalid_latest_unverified() {
        let env = classify_engine(
            None,
            &matching_locals(),
            &RemotePins::unverified(),
            true,
            None,
        );
        assert_eq!(env.status, PinStatus::Unverified);
        assert_eq!(exit_code_for(env.status), 2);
        assert_ne!(env.status, PinStatus::Match);
        let v = serde_json::to_value(&env).expect("json");
        assert!(v.get("latest").is_none());
    }

    #[test]
    fn classify_t3_all_match() {
        let latest = published_latest();
        let env = classify_engine(
            Some(&latest),
            &matching_locals(),
            &matching_remotes(),
            false,
            None,
        );
        assert_eq!(env.status, PinStatus::Match);
        assert_eq!(exit_code_for(env.status), 0);
        assert_eq!(ids(&env).len(), 6);
        for s in &env.surfaces {
            assert_eq!(s.status, PinStatus::Match, "{}", s.id);
        }
        let v = serde_json::to_value(&env).expect("json");
        assert_eq!(v["latest"]["tag"], LATEST_TAG);
        assert_eq!(v["latest"]["sha"], "c4a2308fe985");
    }

    #[test]
    fn classify_t4_homebrew_template_lag_is_drift() {
        let latest = published_latest();
        let mut locals = matching_locals();
        if let Some(hb) = locals.homebrew.as_mut() {
            hb.version = Some("0.2.9".to_string());
        }
        let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(exit_code_for(env.status), 1);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Drift
        );
    }

    #[test]
    fn classify_t5_scoop_hash_mismatch_is_drift() {
        let latest = published_latest();
        let mut locals = matching_locals();
        if let Some(sc) = locals.scoop.as_mut() {
            sc.hash = Some(HASH_SIDECAR.to_string());
        }
        let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(exit_code_for(env.status), 1);
        assert_eq!(surface_status(&env, ID_PACKAGING_SCOOP), PinStatus::Drift);
    }

    #[test]
    fn classify_t6_mcp_intree_tag_lag_is_drift() {
        let latest = published_latest();
        let mut locals = matching_locals();
        if let Some(mcp) = locals.mcp.as_mut() {
            mcp.ledgerful_engine_tag = Some("v0.2.9".to_string());
        }
        let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(surface_status(&env, ID_MCP_INTREE), PinStatus::Drift);
    }

    #[test]
    fn classify_t7_tap_remote_lag_is_drift() {
        let latest = published_latest();
        let mut remotes = matching_remotes();
        let mut tap = matching_homebrew();
        tap.version = Some("0.2.9".to_string());
        remotes.homebrew_tap = RemoteFact::Value(tap);
        let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(surface_status(&env, ID_REMOTE_TAP), PinStatus::Drift);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Match
        );
    }

    #[test]
    fn classify_t8_bucket_remote_lag_is_drift() {
        let latest = published_latest();
        let mut remotes = matching_remotes();
        let mut bucket = matching_scoop();
        bucket.version = Some("0.2.9".to_string());
        remotes.scoop_bucket = RemoteFact::Value(bucket);
        let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(surface_status(&env, ID_REMOTE_BUCKET), PinStatus::Drift);
    }

    #[test]
    fn classify_t9_npm_engine_tag_lag_is_drift() {
        let latest = published_latest();
        let mut remotes = matching_remotes();
        remotes.npm = RemoteFact::Value(McpPin {
            version: Some(MCP_VERSION.to_string()),
            ledgerful_engine_tag: Some("v0.2.9".to_string()),
        });
        let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(exit_code_for(env.status), 1);
        assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Drift);
    }

    fn release_json_with_sidecar() -> Value {
        serde_json::from_str(&format!(
            r#"{{
                "tag_name": "v0.2.10",
                "target_commitish": "main",
                "assets": [
                    {{
                        "name": "{ARCHIVE_DARWIN_ARM}",
                        "digest": "sha256:{HASH_DARWIN_ARM}"
                    }},
                    {{
                        "name": "{ARCHIVE_DARWIN_ARM}.sha256",
                        "digest": "sha256:{HASH_SIDECAR}"
                    }},
                    {{
                        "name": "{ARCHIVE_DARWIN_ARM}.bundle",
                        "digest": "sha256:{HASH_SIDECAR}"
                    }},
                    {{
                        "name": "{ARCHIVE_DARWIN_X64}",
                        "digest": "sha256:{HASH_DARWIN_X64}"
                    }},
                    {{
                        "name": "{ARCHIVE_LINUX_X64}",
                        "digest": "sha256:{HASH_LINUX_X64}"
                    }},
                    {{
                        "name": "{ARCHIVE_WINDOWS}",
                        "digest": "sha256:{HASH_WINDOWS}"
                    }}
                ]
            }}"#
        ))
        .expect("fixture json")
    }

    #[test]
    fn parse_release_ignores_target_commitish_main() {
        let v = release_json_with_sidecar();
        let pins = parse_release_pins(&v).expect("tag_name");
        assert_eq!(pins.tag, LATEST_TAG);
        assert_ne!(pins.tag, "main");
        assert_eq!(v["target_commitish"], "main");
        assert!(pins.sha.is_none());
        assert_ne!(pins.version, "main");
    }

    #[test]
    fn parse_assets_uses_archive_digest_not_sidecar() {
        let v = release_json_with_sidecar();
        let pins = parse_release_pins(&v).expect("assets");
        assert_eq!(
            pins.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
            Some(HASH_DARWIN_ARM)
        );
        assert!(
            !pins
                .archives
                .contains_key(&format!("{ARCHIVE_DARWIN_ARM}.sha256"))
        );
        assert!(!pins.archives.values().any(|h| h == HASH_SIDECAR));
        assert_ne!(
            pins.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
            Some(HASH_SIDECAR)
        );
    }

    #[test]
    fn classify_t12_npm_unverified_no_drift_is_unverified() {
        let latest = published_latest();
        let mut remotes = matching_remotes();
        remotes.npm = RemoteFact::Unverified;
        let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
        assert_eq!(env.status, PinStatus::Unverified);
        assert_eq!(exit_code_for(env.status), 2);
        assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Unverified);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Match
        );
    }

    #[test]
    fn classify_t13_drift_wins_over_unverified() {
        let latest = published_latest();
        let mut locals = matching_locals();
        if let Some(hb) = locals.homebrew.as_mut() {
            hb.version = Some("0.2.9".to_string());
        }
        let mut remotes = matching_remotes();
        remotes.npm = RemoteFact::Unverified;
        let env = classify_engine(Some(&latest), &locals, &remotes, false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(exit_code_for(env.status), 1);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Drift
        );
        assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Unverified);
    }

    #[test]
    fn classify_t14_template_ahead_of_latest_is_drift() {
        let latest = published_latest();
        let mut locals = matching_locals();
        if let Some(hb) = locals.homebrew.as_mut() {
            hb.version = Some("0.2.11".to_string());
        }
        let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
        assert_eq!(env.status, PinStatus::Drift);
        assert_eq!(exit_code_for(env.status), 1);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Drift
        );
    }

    #[test]
    fn classify_t15_advisory_mismatch_does_not_fail() {
        let latest = published_latest();
        let env = classify_engine(
            Some(&latest),
            &matching_locals(),
            &matching_remotes(),
            false,
            Some(AdvisoryInput {
                launch_facts_path: "../ledgerful-web/src/lib/content/launch-facts.ts".to_string(),
                release_tag: Some("v0.2.9".to_string()),
                mcp_engine_tag: Some(LATEST_TAG.to_string()),
            }),
        );
        assert_eq!(env.status, PinStatus::Match);
        assert_eq!(exit_code_for(env.status), 0);
        let adv = env.advisory.as_ref().expect("advisory");
        assert_eq!(adv.status, PinStatus::Drift);
        assert_eq!(adv.release_tag.as_deref(), Some("v0.2.9"));
    }

    #[test]
    fn json_surfaces_sorted_by_id_stable() {
        let latest = published_latest();
        let env = classify_engine(
            Some(&latest),
            &matching_locals(),
            &matching_remotes(),
            false,
            None,
        );
        let a = serde_json::to_string(&env).expect("ser a");
        let b = serde_json::to_string(&env).expect("ser b");
        assert_eq!(a, b);
        let expected = [
            ID_MCP_INTREE,
            ID_MCP_NPM,
            ID_PACKAGING_HOMEBREW,
            ID_PACKAGING_SCOOP,
            ID_REMOTE_TAP,
            ID_REMOTE_BUCKET,
        ];
        assert_eq!(ids(&env), expected);
        let v: Value = serde_json::from_str(&a).expect("parse");
        let got: Vec<&str> = v["surfaces"]
            .as_array()
            .expect("surfaces")
            .iter()
            .map(|s| s["id"].as_str().expect("id"))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn parse_homebrew_pairs_url_with_sha256() {
        let body = r#"
class Ledgerful < Formula
  version "0.2.10"
  sha256 "should-not-pair-without-url"
  on_macos do
    on_arm do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-aarch64-apple-darwin.tar.gz"
      sha256 "550cbc61bde812017a5fc19d61e00dac7cd59ac14fed0a81bf7dda5ce22d29de"
    end
    on_intel do
      url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-apple-darwin.tar.gz"
      sha256 "149f14faf2f153c1682505e32ca49cca6a35f2375547cb3cef4de8fa5810a614"
    end
  end
  url "https://example.com/unpaired.tar.gz"
end
"#;
        let pin = parse_homebrew_formula(body);
        assert_eq!(pin.version.as_deref(), Some("0.2.10"));
        assert_eq!(
            pin.hashes.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
            Some(HASH_DARWIN_ARM)
        );
        assert_eq!(
            pin.hashes.get(ARCHIVE_DARWIN_X64).map(String::as_str),
            Some(HASH_DARWIN_X64)
        );
        assert!(!pin.hashes.contains_key("unpaired.tar.gz"));
        assert!(
            !pin.hashes
                .values()
                .any(|h| h == "should-not-pair-without-url")
        );
    }

    #[test]
    fn parse_scoop_version_and_hash() {
        let body = r#"{
            "version": "0.2.10",
            "architecture": {
                "64bit": {
                    "url": "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-pc-windows-msvc.zip",
                    "hash": "99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca"
                }
            }
        }"#;
        let pin = parse_scoop_manifest(body).expect("scoop");
        assert_eq!(pin.version.as_deref(), Some("0.2.10"));
        assert_eq!(pin.hash.as_deref(), Some(HASH_WINDOWS));
        assert!(
            pin.url
                .as_deref()
                .is_some_and(|u| u.ends_with(ARCHIVE_WINDOWS))
        );
    }

    #[test]
    fn parse_mcp_ledgerful_engine_tag() {
        let body = r#"{
            "name": "@ledgerful/mcp-server",
            "version": "0.1.19",
            "ledgerfulEngineTag": "v0.2.10"
        }"#;
        let pin = parse_mcp_package(body).expect("mcp");
        assert_eq!(pin.version.as_deref(), Some(MCP_VERSION));
        assert_eq!(pin.ledgerful_engine_tag.as_deref(), Some(LATEST_TAG));
    }

    fn endpoints(server: &httpmock::MockServer) -> PinFetchEndpoints {
        PinFetchEndpoints {
            github_api_base: server.base_url(),
            npm_latest_url: format!("{}/npm/latest", server.base_url()),
        }
    }

    fn github_json_when(when: httpmock::When, path: &str, ua: &str) -> httpmock::When {
        when.method(httpmock::Method::GET)
            .path(path)
            .header("User-Agent", ua)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
    }

    fn github_raw_when(when: httpmock::When, path: &str, ua: &str) -> httpmock::When {
        when.method(httpmock::Method::GET)
            .path(path)
            .header("User-Agent", ua)
            .header("Accept", "application/vnd.github.raw+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
    }

    fn mock_pin_stack(
        server: &httpmock::MockServer,
        peel_status: u16,
    ) -> (
        httpmock::Mock<'_>,
        httpmock::Mock<'_>,
        httpmock::Mock<'_>,
        httpmock::Mock<'_>,
        httpmock::Mock<'_>,
    ) {
        let ua = user_agent();
        let latest_body = serde_json::to_string(&release_json_with_sidecar()).expect("body");
        let latest_mock = server.mock(|when, then| {
            github_json_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(latest_body);
        });
        let peel_mock = server.mock(|when, then| {
            github_json_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10",
                ua.as_str(),
            );
            if peel_status == 200 {
                then.status(200)
                    .header("content-type", "application/json")
                    .body(format!(r#"{{"sha":"{LATEST_SHA}"}}"#));
            } else {
                then.status(peel_status)
                    .header("content-type", "application/json")
                    .body(r#"{"message":"Not Found"}"#);
            }
        });
        let tap_mock = server.mock(|when, then| {
            github_raw_when(
                when,
                "/repos/Ryan-AI-Studios/homebrew-tap/contents/ledgerful.rb",
                ua.as_str(),
            );
            then.status(200).body(r#"version "0.2.10""#);
        });
        let bucket_mock = server.mock(|when, then| {
            github_raw_when(
                when,
                "/repos/Ryan-AI-Studios/scoop-bucket/contents/ledgerful.json",
                ua.as_str(),
            );
            then.status(200)
                .body(r#"{"version":"0.2.10","architecture":{"64bit":{"hash":"99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca","url":"https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-pc-windows-msvc.zip"}}}"#);
        });
        let npm_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/npm/latest")
                .header("User-Agent", ua.as_str());
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"version":"0.1.19","ledgerfulEngineTag":"v0.2.10"}"#);
        });
        (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock)
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_honors_ledgerful_no_network() {
        let server = httpmock::MockServer::start();
        let (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock) =
            mock_pin_stack(&server, 200);
        let _g = TempEnv::set(crate::util::network::NO_NETWORK_ENV, "1");
        let result = fetch_latest_pins(&endpoints(&server));
        assert!(
            matches!(&result.latest, Err(PinFetchError::NetworkDisabled)),
            "NO_NETWORK must fail before HTTP: {}",
            result
                .latest
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        assert_eq!(latest_mock.calls(), 0, "zero HTTP hits on releases/latest");
        assert_eq!(peel_mock.calls(), 0, "zero HTTP hits on peel");
        assert_eq!(tap_mock.calls(), 0, "zero HTTP hits on tap");
        assert_eq!(bucket_mock.calls(), 0, "zero HTTP hits on bucket");
        assert_eq!(npm_mock.calls(), 0, "zero HTTP hits on npm");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_sets_user_agent() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock) =
            mock_pin_stack(&server, 200);
        let got = fetch_latest_pins(&endpoints(&server));
        let latest = got.latest.expect("latest");
        assert_eq!(latest.tag, LATEST_TAG);
        assert_eq!(latest.sha.as_deref(), Some(LATEST_SHA));
        assert_eq!(latest_mock.calls(), 1, "releases/latest must match UA");
        assert_eq!(peel_mock.calls(), 1, "peel must match UA");
        assert_eq!(tap_mock.calls(), 1, "tap must match UA");
        assert_eq!(bucket_mock.calls(), 1, "bucket must match UA");
        assert_eq!(npm_mock.calls(), 1, "npm must match UA");
        assert!(got.tap.is_ok());
        assert!(got.npm.is_ok());
    }

    #[test]
    #[serial_test::serial(env)]
    fn peel_still_unused_for_pin_keys() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let (latest_mock, peel_mock, _tap, _bucket, _npm) = mock_pin_stack(&server, 404);
        let decoy_main = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/main");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#);
        });
        let got = fetch_latest_pins(&endpoints(&server));
        let latest = got.latest.expect("peel 404 still pins");
        assert_eq!(latest.tag, LATEST_TAG);
        assert!(latest.sha.is_none(), "peel 404 omits sha");
        assert_eq!(
            latest.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
            Some(HASH_DARWIN_ARM)
        );
        assert!(!latest.archives.values().any(|h| h == HASH_SIDECAR));
        assert_eq!(latest_mock.calls(), 1);
        assert_eq!(peel_mock.calls(), 1);
        assert_eq!(decoy_main.calls(), 0, "must not GET /commits/main");
        // SHA-required fetch_github_latest would Err on peel 404; pin keys still work.
    }
}
