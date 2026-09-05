use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const GITHUB_OWNER_REPO: &str = "Ryan-AI-Studios/Ledgerful";
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";
pub(crate) const HOMEBREW_TAP_REPO: &str = "Ryan-AI-Studios/homebrew-tap";
pub(crate) const HOMEBREW_TAP_PATH: &str = "ledgerful.rb";
pub(crate) const SCOOP_BUCKET_REPO: &str = "Ryan-AI-Studios/scoop-bucket";
pub(crate) const SCOOP_BUCKET_PATH: &str = "ledgerful.json";
pub(crate) const NPM_LATEST_URL: &str = "https://registry.npmjs.org/@ledgerful/mcp-server/latest";

pub(super) const GITHUB_API_VERSION: &str = "2022-11-28";
pub(super) const KIND: &str = "releasePins";
pub(super) const SCHEMA_VERSION: u32 = 1;

pub(super) const ARCHIVE_DARWIN_ARM: &str = "ledgerful-aarch64-apple-darwin.tar.gz";
pub(super) const ARCHIVE_DARWIN_X64: &str = "ledgerful-x86_64-apple-darwin.tar.gz";
pub(super) const ARCHIVE_LINUX_X64: &str = "ledgerful-x86_64-unknown-linux-gnu.tar.gz";
pub(super) const ARCHIVE_WINDOWS: &str = "ledgerful-x86_64-pc-windows-msvc.zip";
pub(super) const HOMEBREW_ARCHIVES: &[&str] =
    &[ARCHIVE_DARWIN_ARM, ARCHIVE_DARWIN_X64, ARCHIVE_LINUX_X64];

pub(super) const ID_PACKAGING_HOMEBREW: &str = "packaging.homebrew";
pub(super) const ID_PACKAGING_SCOOP: &str = "packaging.scoop";
pub(super) const ID_MCP_INTREE: &str = "mcp.inTree";
pub(super) const ID_REMOTE_TAP: &str = "remote.homebrew-tap";
pub(super) const ID_REMOTE_BUCKET: &str = "remote.scoop-bucket";
pub(super) const ID_MCP_NPM: &str = "mcp.npm";

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
    pub(super) fn unverified() -> Self {
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
