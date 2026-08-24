//! GitHub Latest vs PATH / worktree (0205).
//!
//! 0137 stays worktree-only and offline. This arm peels Latest SHA from the
//! release tag via `GET /commits/{tag}` — never `releases/latest.target_commitish`
//! (live value is `"main"`).
//!
//! 0201 shipped its own `fetch_latest_pins`; this helper stays doctor-only.

use super::binary_currency::{sha_prefix_equal, shorten_sha_for_display};
use super::finding::{DoctorCategory, DoctorFinding, DoctorSeverity};
use serde::Serialize;
use std::cmp::Ordering;
use std::time::Duration;

/// Official product repo (`Cargo.toml` `package.repository`). Do not follow git origin.
pub(crate) const GITHUB_OWNER_REPO: &str = "Ryan-AI-Studios/Ledgerful";

/// Production GitHub REST base. Inject `base_url` in tests (httpmock).
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";

pub(crate) const BINARY_BEHIND_LATEST_CODE: &str = "binary-behind-latest";
pub(crate) const BINARY_AHEAD_OF_LATEST_CODE: &str = "binary-ahead-of-latest";

const GITHUB_API_VERSION: &str = "2022-11-28";
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Published GitHub Latest facts after a successful peel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedLatest {
    pub tag: String,
    pub sha: String,
    pub version: String,
}

/// Aggregate `environment.githubLatest.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LatestStatus {
    Skipped,
    Unverified,
    Match,
    Behind,
    Ahead,
    Mixed,
    Unknown,
}

/// Per-entity relation vs Latest (`running` / `worktree`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EntityRelation {
    Match,
    Behind,
    Ahead,
    Unknown,
}

/// Additive `environment.githubLatest` object (schemaVersion stays 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GithubLatestEnv {
    pub status: LatestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<EntityRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<EntityRelation>,
}

/// Classifier output: env object + 0..=2 of the 0205 finding codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LatestClassification {
    pub env: GithubLatestEnv,
    pub findings: Vec<DoctorFinding>,
}

/// Injected facts for the pure classifier (fetch is a separate function).
pub(crate) struct ClassifyLatestInput<'a> {
    pub is_engine: bool,
    pub running_ver: &'a str,
    pub running_sha: &'a str,
    pub worktree_ver: Option<&'a str>,
    pub worktree_head: Option<&'a str>,
    pub latest: Option<&'a PublishedLatest>,
    pub fetch_error: bool,
}

/// Fail-soft fetch errors. Doctor maps every variant to `unverified` (no finding).
#[derive(Debug)]
pub(crate) enum LatestFetchError {
    NetworkDisabled,
    Http { status: Option<u16>, detail: String },
    InvalidBody(&'static str),
}

impl std::fmt::Display for LatestFetchError {
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

/// Tiny local `X.Y.Z` parse (not a semver crate).
///
/// Strip one leading `v`. Strip `+` build metadata and `-pre` suffixes
/// before splitting. Parse failure skips the **behind** arm only.
fn parse_ver(raw: &str) -> Option<(u32, u32, u32)> {
    let s = raw.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let s = match s.split_once('+') {
        Some((core, _)) => core,
        None => s,
    };
    let s = match s.split_once('-') {
        Some((core, _)) => core,
        None => s,
    };
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn cmp_versions(a: &str, b: &str) -> Option<Ordering> {
    Some(parse_ver(a)?.cmp(&parse_ver(b)?))
}

fn sha_usable(sha: &str) -> bool {
    let s = sha.trim();
    !s.is_empty() && !s.eq_ignore_ascii_case("unknown")
}

fn strip_leading_v(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

/// Release JSON → `tag_name` only. Never reads `target_commitish`.
pub(crate) fn parse_release_tag_name(value: &serde_json::Value) -> Option<String> {
    value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Commits JSON → top-level `sha` (40-hex). Reject empty / non-hex.
pub(crate) fn parse_commit_sha(value: &serde_json::Value) -> Option<String> {
    let sha = value.get("sha").and_then(|v| v.as_str()).map(str::trim)?;
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha.to_ascii_lowercase())
    } else {
        None
    }
}

/// SHA prefix-equal Latest is Match even if versions differ.
/// Unknown/empty SHA skips commit comparison only; version still classifies
/// behind/ahead. Equal version + unusable SHA stays Unknown (T11). Parse
/// failure is not Ahead (SHA mismatch already failed prefix-equal).
fn classify_running(ver: &str, sha: &str, latest: &PublishedLatest) -> EntityRelation {
    if sha_prefix_equal(sha, &latest.sha) {
        return EntityRelation::Match;
    }
    match cmp_versions(ver, &latest.version) {
        Some(Ordering::Less) => EntityRelation::Behind,
        Some(Ordering::Greater) => EntityRelation::Ahead,
        Some(Ordering::Equal) if sha_usable(sha) => EntityRelation::Ahead,
        Some(Ordering::Equal) | None => EntityRelation::Unknown,
    }
}

fn classify_worktree(
    ver: Option<&str>,
    sha: Option<&str>,
    latest: &PublishedLatest,
) -> EntityRelation {
    let sha = sha.unwrap_or("");
    if sha_prefix_equal(sha, &latest.sha) {
        return EntityRelation::Match;
    }
    if !sha_usable(sha) {
        return EntityRelation::Unknown;
    }
    match ver.and_then(|v| cmp_versions(v, &latest.version)) {
        Some(Ordering::Less) => EntityRelation::Behind,
        Some(Ordering::Greater) => EntityRelation::Ahead,
        Some(Ordering::Equal) => EntityRelation::Unknown,
        None => EntityRelation::Unknown,
    }
}

fn aggregate_status(running: EntityRelation, worktree: EntityRelation) -> LatestStatus {
    match (running, worktree) {
        (EntityRelation::Behind, EntityRelation::Ahead) => LatestStatus::Mixed,
        (EntityRelation::Behind, _) => LatestStatus::Behind,
        (EntityRelation::Ahead, _) => LatestStatus::Ahead,
        (EntityRelation::Match, _) => LatestStatus::Match,
        (EntityRelation::Unknown, _) => LatestStatus::Unknown,
    }
}

fn release_tag_url(tag: &str) -> String {
    format!("https://github.com/{GITHUB_OWNER_REPO}/releases/tag/{tag}")
}

fn behind_latest_remediation(tag: &str) -> String {
    format!("{}\nledgerful --version", release_tag_url(tag))
}

fn ahead_of_latest_remediation(tag: &str, sha12: &str) -> String {
    format!(
        "This binary is not GitHub Latest {tag} ({sha12}) — do not recapture public exhibits from this binary.\n{}",
        release_tag_url(tag)
    )
}

fn ahead_of_latest_worktree_remediation(tag: &str, sha12: &str) -> String {
    format!(
        "This worktree is not GitHub Latest {tag} ({sha12}) — do not recapture public exhibits from this tree.\n{}",
        release_tag_url(tag)
    )
}

fn behind_latest_message(running_ver: &str, running_sha: &str, latest: &PublishedLatest) -> String {
    let latest_sha12 = shorten_sha_for_display(&latest.sha);
    let run_sha = if sha_usable(running_sha) {
        format!(" ({})", shorten_sha_for_display(running_sha))
    } else {
        String::new()
    };
    format!(
        "Installed ledgerful binary is behind GitHub Latest {} ({latest_sha12}): running {running_ver}{run_sha}",
        latest.tag
    )
}

fn ahead_of_latest_message(
    running_ver: &str,
    running_sha: &str,
    latest: &PublishedLatest,
) -> String {
    format!(
        "Installed ledgerful binary is not GitHub Latest {} ({}): running {} ({}) — do not recapture public exhibits from this binary",
        latest.tag,
        shorten_sha_for_display(&latest.sha),
        running_ver,
        shorten_sha_for_display(running_sha)
    )
}

fn ahead_of_latest_worktree_message(
    worktree_ver: &str,
    worktree_sha: &str,
    latest: &PublishedLatest,
) -> String {
    format!(
        "This worktree is not GitHub Latest {} ({}): cargo {} ({}) — do not recapture public exhibits from this tree",
        latest.tag,
        shorten_sha_for_display(&latest.sha),
        worktree_ver,
        shorten_sha_for_display(worktree_sha)
    )
}

pub(crate) fn build_behind_latest_finding(
    running_ver: &str,
    running_sha: &str,
    latest: &PublishedLatest,
) -> DoctorFinding {
    DoctorFinding {
        code: BINARY_BEHIND_LATEST_CODE.to_string(),
        severity: DoctorSeverity::Warn,
        category: DoctorCategory::Tools,
        message: behind_latest_message(running_ver, running_sha, latest),
        remediation: Some(behind_latest_remediation(&latest.tag)),
    }
}

pub(crate) fn build_ahead_of_latest_finding(
    running_ver: &str,
    running_sha: &str,
    latest: &PublishedLatest,
) -> DoctorFinding {
    DoctorFinding {
        code: BINARY_AHEAD_OF_LATEST_CODE.to_string(),
        severity: DoctorSeverity::Info,
        category: DoctorCategory::Tools,
        message: ahead_of_latest_message(running_ver, running_sha, latest),
        remediation: Some(ahead_of_latest_remediation(
            &latest.tag,
            &shorten_sha_for_display(&latest.sha),
        )),
    }
}

fn build_ahead_of_latest_worktree_finding(
    worktree_ver: &str,
    worktree_sha: &str,
    latest: &PublishedLatest,
) -> DoctorFinding {
    DoctorFinding {
        code: BINARY_AHEAD_OF_LATEST_CODE.to_string(),
        severity: DoctorSeverity::Info,
        category: DoctorCategory::Tools,
        message: ahead_of_latest_worktree_message(worktree_ver, worktree_sha, latest),
        remediation: Some(ahead_of_latest_worktree_remediation(
            &latest.tag,
            &shorten_sha_for_display(&latest.sha),
        )),
    }
}

fn skipped() -> LatestClassification {
    LatestClassification {
        env: GithubLatestEnv {
            status: LatestStatus::Skipped,
            tag: None,
            sha: None,
            running: None,
            worktree: None,
        },
        findings: Vec::new(),
    }
}

fn unverified() -> LatestClassification {
    LatestClassification {
        env: GithubLatestEnv {
            status: LatestStatus::Unverified,
            tag: None,
            sha: None,
            running: Some(EntityRelation::Unknown),
            worktree: Some(EntityRelation::Unknown),
        },
        findings: Vec::new(),
    }
}

/// Pure classifier. Fetch is [`fetch_github_latest`].
pub(crate) fn classify_github_latest(input: ClassifyLatestInput<'_>) -> LatestClassification {
    if !input.is_engine {
        return skipped();
    }
    let Some(latest) = input.latest.filter(|_| !input.fetch_error) else {
        return unverified();
    };

    let running = classify_running(input.running_ver, input.running_sha, latest);
    let worktree = classify_worktree(input.worktree_ver, input.worktree_head, latest);
    let status = aggregate_status(running, worktree);

    let mut findings = Vec::new();
    if running == EntityRelation::Behind {
        findings.push(build_behind_latest_finding(
            input.running_ver,
            input.running_sha,
            latest,
        ));
    }
    // Running-ahead (T4/T9): subject is PATH. Mixed (F8): ahead subject is worktree.
    // Do not use `if worktree == Ahead` alone — that would emit on T18 (PATH match).
    if running == EntityRelation::Ahead {
        findings.push(build_ahead_of_latest_finding(
            input.running_ver,
            input.running_sha,
            latest,
        ));
    } else if running == EntityRelation::Behind && worktree == EntityRelation::Ahead {
        // Fail-soft: mixed without worktree facts cannot happen; no unwrap.
        if let (Some(v), Some(s)) = (input.worktree_ver, input.worktree_head) {
            findings.push(build_ahead_of_latest_worktree_finding(v, s, latest));
        }
    }
    findings.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then(a.message.cmp(&b.message))
            .then(a.severity.as_str().cmp(b.severity.as_str()))
    });

    LatestClassification {
        env: GithubLatestEnv {
            status,
            tag: Some(latest.tag.clone()),
            sha: Some(shorten_sha_for_display(&latest.sha)),
            running: Some(running),
            worktree: Some(worktree),
        },
        findings,
    }
}

fn user_agent() -> String {
    format!("ledgerful/{}", env!("CARGO_PKG_VERSION"))
}

fn github_get(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value, LatestFetchError> {
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _resp) => LatestFetchError::Http {
                status: Some(code),
                detail: format!("HTTP {code}"),
            },
            ureq::Error::Transport(inner) => LatestFetchError::Http {
                status: None,
                detail: inner.to_string(),
            },
        })?;
    resp.into_json()
        .map_err(|_| LatestFetchError::InvalidBody("invalid JSON"))
}

/// Fetch GitHub Latest: `releases/latest` `tag_name` + peeled `GET /commits/{tag}` SHA.
///
/// 0201 shipped its own `fetch_latest_pins`; this helper stays doctor-only.
/// Never uses `target_commitish`. Never sends `GITHUB_TOKEN` / `GH_TOKEN`.
/// Never spawns `gh` / `git`. No retry. Check `LEDGERFUL_NO_NETWORK` before
/// `AgentBuilder` / `call()`.
pub(crate) fn fetch_github_latest(base_url: &str) -> Result<PublishedLatest, LatestFetchError> {
    if crate::util::network::network_disabled_from_env() {
        return Err(LatestFetchError::NetworkDisabled);
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .build();

    let base = base_url.trim_end_matches('/');
    let release_url = format!("{base}/repos/{GITHUB_OWNER_REPO}/releases/latest");
    let release_json = github_get(&agent, &release_url)?;
    let tag = parse_release_tag_name(&release_json)
        .ok_or(LatestFetchError::InvalidBody("empty tag_name"))?;

    let commits_url = format!("{base}/repos/{GITHUB_OWNER_REPO}/commits/{tag}");
    let commits_json = github_get(&agent, &commits_url)?;
    let sha =
        parse_commit_sha(&commits_json).ok_or(LatestFetchError::InvalidBody("non-hex SHA"))?;

    let version = strip_leading_v(tag.trim()).to_string();
    Ok(PublishedLatest { tag, sha, version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::finding::{
        dashboard_failures, is_action_critical, ready_for_publish,
    };
    use crate::commands::doctor::{
        BINARY_BEHIND_TREE_CODE, BINARY_BEHIND_TREE_REMEDIATION, build_binary_behind_tree_finding,
        classify_binary_currency,
    };

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    const LATEST_SHA: &str = "c4a2308fe98548899105e33ff38232dfb229ec02";
    const LATEST_SHA12: &str = "c4a2308fe985";
    const LATEST_TAG: &str = "v0.2.10";
    const TIP_SHA: &str = "55cd2dc9d5b6aaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLDER_TREE_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn published() -> PublishedLatest {
        PublishedLatest {
            tag: LATEST_TAG.to_string(),
            sha: LATEST_SHA.to_string(),
            version: "0.2.10".to_string(),
        }
    }

    fn classify_engine(
        running_ver: &str,
        running_sha: &str,
        worktree_ver: &str,
        worktree_head: &str,
        latest: Option<&PublishedLatest>,
    ) -> LatestClassification {
        classify_github_latest(ClassifyLatestInput {
            is_engine: true,
            running_ver,
            running_sha,
            worktree_ver: Some(worktree_ver),
            worktree_head: Some(worktree_head),
            latest,
            fetch_error: false,
        })
    }

    fn codes(c: &LatestClassification) -> Vec<&str> {
        c.findings.iter().map(|f| f.code.as_str()).collect()
    }

    fn env_json(c: &LatestClassification) -> serde_json::Value {
        serde_json::to_value(&c.env).expect("serialize githubLatest")
    }

    #[test]
    fn classify_t0_consumer_skipped() {
        let latest = published();
        let c = classify_github_latest(ClassifyLatestInput {
            is_engine: false,
            running_ver: "0.2.9",
            running_sha: OLD_SHA,
            worktree_ver: Some("0.2.9"),
            worktree_head: Some(OLD_SHA),
            latest: Some(&latest),
            fetch_error: false,
        });
        assert_eq!(c.env.status, LatestStatus::Skipped);
        assert!(c.findings.is_empty());
        let v = env_json(&c);
        assert_eq!(v["status"], "skipped");
        assert!(v.get("tag").is_none());
        assert!(v.get("sha").is_none());
        assert!(v.get("running").is_none());
        assert!(v.get("worktree").is_none());
    }

    #[test]
    fn classify_t1_no_latest_is_unverified_not_match() {
        let c = classify_github_latest(ClassifyLatestInput {
            is_engine: true,
            running_ver: "0.2.10",
            running_sha: TIP_SHA,
            worktree_ver: Some("0.2.10"),
            worktree_head: Some(TIP_SHA),
            latest: None,
            fetch_error: true,
        });
        assert_eq!(c.env.status, LatestStatus::Unverified);
        assert_ne!(c.env.status, LatestStatus::Match);
        assert!(c.findings.is_empty());
        let v = env_json(&c);
        assert_eq!(v["status"], "unverified");
        assert_ne!(v["status"], "match");
        assert!(v.get("tag").is_none());
        assert!(v.get("sha").is_none());
        assert_eq!(v["running"], "unknown");
        assert_eq!(v["worktree"], "unknown");
    }

    #[test]
    fn classify_t3_match_no_finding() {
        let latest = published();
        let c = classify_engine("0.2.10", LATEST_SHA, "0.2.10", LATEST_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Match);
        assert_eq!(c.env.running, Some(EntityRelation::Match));
        assert_eq!(c.env.worktree, Some(EntityRelation::Match));
        assert!(c.findings.is_empty());
        let v = env_json(&c);
        assert_eq!(v["status"], "match");
        assert_eq!(v["tag"], LATEST_TAG);
        assert_eq!(v["sha"], LATEST_SHA12);
        assert_eq!(v["running"], "match");
        assert_eq!(v["worktree"], "match");
    }

    #[test]
    fn classify_t4_same_version_sha_mismatch_is_ahead_not_behind() {
        // 0199 class: cargo tip at the same version as Latest.
        let latest = published();
        let c = classify_engine("0.2.10", TIP_SHA, "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Ahead);
        assert_eq!(c.env.running, Some(EntityRelation::Ahead));
        assert_eq!(c.env.worktree, Some(EntityRelation::Unknown));
        assert_eq!(codes(&c), vec![BINARY_AHEAD_OF_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_BEHIND_LATEST_CODE));
        let msg = &c.findings[0].message;
        assert!(msg.contains(LATEST_TAG), "must name tag: {msg}");
        assert!(msg.contains(LATEST_SHA12), "must name Latest SHA: {msg}");
        assert!(
            msg.contains(&shorten_sha_for_display(TIP_SHA)),
            "must name running SHA: {msg}"
        );
    }

    #[test]
    fn classify_t5_running_older_version_is_behind() {
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.10", LATEST_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Behind);
        assert_eq!(c.env.running, Some(EntityRelation::Behind));
        assert_eq!(c.env.worktree, Some(EntityRelation::Match));
        assert_eq!(codes(&c), vec![BINARY_BEHIND_LATEST_CODE]);
    }

    #[test]
    fn classify_t6_old_checkout_behind_only() {
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.9", OLD_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Behind);
        assert_eq!(c.env.running, Some(EntityRelation::Behind));
        assert_eq!(c.env.worktree, Some(EntityRelation::Behind));
        assert_eq!(codes(&c), vec![BINARY_BEHIND_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
    }

    #[test]
    fn classify_t7_behind_only_no_ahead_when_running_older() {
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Behind);
        assert_eq!(c.env.running, Some(EntityRelation::Behind));
        assert_eq!(c.env.worktree, Some(EntityRelation::Unknown));
        assert_eq!(codes(&c), vec![BINARY_BEHIND_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
    }

    #[test]
    fn classify_f8_mixed_running_behind_worktree_newer_emits_both() {
        // Contrast T7: equal-version worktree SHA mismatch stays unknown/behind-only.
        // F8: worktree version > Latest → mixed, both codes (ahead sorts before behind).
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Mixed);
        assert_eq!(c.env.running, Some(EntityRelation::Behind));
        assert_eq!(c.env.worktree, Some(EntityRelation::Ahead));
        assert_eq!(
            codes(&c),
            vec![BINARY_AHEAD_OF_LATEST_CODE, BINARY_BEHIND_LATEST_CODE]
        );
        assert_eq!(c.env.sha.as_deref(), Some(LATEST_SHA12));
        let v = env_json(&c);
        assert_eq!(v["status"], "mixed");
        assert_eq!(v["sha"], LATEST_SHA12);
        assert_eq!(v["running"], "behind");
        assert_eq!(v["worktree"], "ahead");
        assert!(ready_for_publish(&c.findings));
    }

    #[test]
    fn classify_t15_mixed_ahead_subject_is_worktree() {
        // F8 fixture: PATH old, worktree version > Latest. Ahead must name the tree.
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Mixed);
        let ahead = c
            .findings
            .iter()
            .find(|f| f.code == BINARY_AHEAD_OF_LATEST_CODE)
            .expect("ahead finding");
        let behind = c
            .findings
            .iter()
            .find(|f| f.code == BINARY_BEHIND_LATEST_CODE)
            .expect("behind finding");
        let tip12 = shorten_sha_for_display(TIP_SHA);
        let old12 = shorten_sha_for_display(OLD_SHA);
        assert!(
            ahead.message.contains("This worktree"),
            "ahead must use worktree stem: {}",
            ahead.message
        );
        assert!(
            ahead.message.contains("0.2.11"),
            "ahead must name worktree ver: {}",
            ahead.message
        );
        assert!(
            ahead.message.contains(&tip12),
            "ahead must name worktree SHA: {}",
            ahead.message
        );
        assert!(
            !ahead.message.contains("0.2.9"),
            "ahead must not name running ver: {}",
            ahead.message
        );
        assert!(
            !ahead.message.contains(&old12),
            "ahead must not name running SHA: {}",
            ahead.message
        );
        assert!(
            !ahead.message.contains("Installed ledgerful binary"),
            "ahead must not use running stem: {}",
            ahead.message
        );
        assert!(
            !ahead.message.contains("This binary"),
            "ahead message must not say This binary: {}",
            ahead.message
        );
        assert!(
            !ahead.message.contains("cargo install --path ."),
            "ahead must not suggest cargo install: {}",
            ahead.message
        );
        let ahead_rem = ahead.remediation.as_deref().unwrap_or("");
        assert!(
            ahead_rem.contains("This worktree"),
            "ahead remediation must name worktree: {ahead_rem}"
        );
        assert!(
            !ahead_rem.contains("This binary"),
            "ahead remediation must not say This binary: {ahead_rem}"
        );
        assert!(
            !ahead_rem.contains("cargo install --path ."),
            "ahead remediation must not suggest cargo install: {ahead_rem}"
        );
        assert!(
            !ahead_rem.contains("--force"),
            "ahead remediation must not say --force: {ahead_rem}"
        );
        assert!(
            behind.message.contains("0.2.9"),
            "behind must name running ver: {}",
            behind.message
        );
        assert!(
            behind.message.contains("Installed ledgerful binary"),
            "behind must keep PATH stem: {}",
            behind.message
        );
    }

    #[test]
    fn classify_t16_running_ahead_message_still_installed_binary() {
        // T4-style: equal version, running SHA ≠ Latest — running-ahead text stays byte-stable.
        let latest = published();
        let c = classify_engine("0.2.10", TIP_SHA, "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Ahead);
        assert_eq!(codes(&c), vec![BINARY_AHEAD_OF_LATEST_CODE]);
        let msg = &c.findings[0].message;
        let rem = c.findings[0].remediation.as_deref().unwrap_or("");
        let tip12 = shorten_sha_for_display(TIP_SHA);
        assert!(
            msg.contains("Installed ledgerful binary"),
            "running-ahead message stem: {msg}"
        );
        assert!(
            msg.contains(&tip12),
            "running-ahead must name running SHA: {msg}"
        );
        assert!(
            rem.contains("This binary"),
            "running-ahead remediation stem: {rem}"
        );
        assert!(
            !msg.contains("This worktree"),
            "running-ahead message must not say worktree: {msg}"
        );
        assert!(
            !rem.contains("This worktree"),
            "running-ahead remediation must not say worktree: {rem}"
        );
    }

    #[test]
    fn classify_t17_mixed_ahead_is_info() {
        let latest = published();
        let c = classify_engine("0.2.9", OLD_SHA, "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Mixed);
        let ahead = c
            .findings
            .iter()
            .find(|f| f.code == BINARY_AHEAD_OF_LATEST_CODE)
            .expect("ahead finding");
        let behind = c
            .findings
            .iter()
            .find(|f| f.code == BINARY_BEHIND_LATEST_CODE)
            .expect("behind finding");
        assert_eq!(ahead.severity, DoctorSeverity::Info);
        assert!(!is_action_critical(ahead));
        assert_eq!(behind.severity, DoctorSeverity::Warn);
        assert!(is_action_critical(behind));
        assert!(ready_for_publish(&c.findings));
    }

    #[test]
    fn classify_t18_running_match_worktree_newer_no_ahead() {
        // PATH matches Latest; tree version > Latest → status=match, not mixed; no 0205 finding.
        let latest = published();
        let c = classify_engine("0.2.10", LATEST_SHA, "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Match);
        assert_ne!(c.env.status, LatestStatus::Mixed);
        assert_eq!(c.env.running, Some(EntityRelation::Match));
        assert_eq!(c.env.worktree, Some(EntityRelation::Ahead));
        assert!(c.findings.is_empty());
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
        assert!(!codes(&c).contains(&BINARY_BEHIND_LATEST_CODE));
    }

    #[test]
    fn classify_t8_running_matches_latest_worktree_sha_mismatch_no_finding() {
        let latest = published();
        let c = classify_engine("0.2.10", LATEST_SHA, "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Match);
        assert_eq!(c.env.running, Some(EntityRelation::Match));
        assert_eq!(c.env.worktree, Some(EntityRelation::Unknown));
        assert!(c.findings.is_empty());
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
        assert!(!codes(&c).contains(&BINARY_BEHIND_LATEST_CODE));
    }

    #[test]
    fn classify_t9_newer_cargo_version_is_ahead() {
        let latest = published();
        let c = classify_engine("0.2.11", TIP_SHA, "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Ahead);
        assert_eq!(c.env.running, Some(EntityRelation::Ahead));
        assert_eq!(c.env.worktree, Some(EntityRelation::Ahead));
        assert_eq!(codes(&c), vec![BINARY_AHEAD_OF_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_BEHIND_LATEST_CODE));
    }

    #[test]
    fn classify_t11_unknown_running_sha_no_ahead_finding() {
        let latest = published();
        let c = classify_engine("0.2.10", "unknown", "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Unknown);
        assert_eq!(c.env.running, Some(EntityRelation::Unknown));
        assert_eq!(c.env.worktree, Some(EntityRelation::Unknown));
        assert!(c.findings.is_empty());
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
        let v = env_json(&c);
        assert_eq!(v["status"], "unknown");
        assert_eq!(v["tag"], LATEST_TAG);
        assert_eq!(v["sha"], LATEST_SHA12);
        assert_eq!(v["running"], "unknown");
        assert_eq!(v["worktree"], "unknown");
    }

    #[test]
    fn classify_unknown_running_sha_older_version_is_behind() {
        let latest = published();
        let c = classify_engine("0.2.9", "unknown", "0.2.10", LATEST_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Behind);
        assert_eq!(c.env.running, Some(EntityRelation::Behind));
        assert_eq!(c.env.worktree, Some(EntityRelation::Match));
        assert_eq!(codes(&c), vec![BINARY_BEHIND_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
        assert!(ready_for_publish(&c.findings));
        let v = env_json(&c);
        assert_eq!(v["status"], "behind");
        assert_eq!(v["running"], "behind");
    }

    #[test]
    fn classify_unknown_running_sha_newer_version_is_ahead() {
        let latest = published();
        let c = classify_engine("0.2.11", "unknown", "0.2.11", TIP_SHA, Some(&latest));
        assert_eq!(c.env.status, LatestStatus::Ahead);
        assert_eq!(c.env.running, Some(EntityRelation::Ahead));
        assert_eq!(codes(&c), vec![BINARY_AHEAD_OF_LATEST_CODE]);
        assert!(!codes(&c).contains(&BINARY_BEHIND_LATEST_CODE));
    }

    #[test]
    fn classify_t13_worktree_older_same_version_not_ahead() {
        let latest = published();
        let c = classify_engine(
            "0.2.10",
            LATEST_SHA,
            "0.2.10",
            OLDER_TREE_SHA,
            Some(&latest),
        );
        assert_eq!(c.env.status, LatestStatus::Match);
        assert_eq!(c.env.running, Some(EntityRelation::Match));
        assert_eq!(c.env.worktree, Some(EntityRelation::Unknown));
        assert!(c.findings.is_empty());
        assert!(!codes(&c).contains(&BINARY_AHEAD_OF_LATEST_CODE));
    }

    #[test]
    fn classify_t14_unverified_running_worktree_unknown_no_tag() {
        let c = classify_github_latest(ClassifyLatestInput {
            is_engine: true,
            running_ver: "0.2.10",
            running_sha: TIP_SHA,
            worktree_ver: Some("0.2.10"),
            worktree_head: Some(TIP_SHA),
            latest: None,
            fetch_error: false,
        });
        assert_eq!(c.env.status, LatestStatus::Unverified);
        assert!(c.findings.is_empty());
        let v = env_json(&c);
        assert_eq!(v["status"], "unverified");
        assert!(v.get("tag").is_none());
        assert!(v.get("sha").is_none());
        assert_eq!(v["running"], "unknown");
        assert_eq!(v["worktree"], "unknown");
    }

    #[test]
    fn parse_release_ignores_target_commitish_main() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "tag_name": "v0.2.10",
                "target_commitish": "main",
                "sha": "should-not-win"
            }"#,
        )
        .expect("fixture json");
        let tag = parse_release_tag_name(&v).expect("tag_name");
        assert_eq!(tag, "v0.2.10");
        assert_ne!(tag, "main");
        // Parser must not treat target_commitish "main" as a SHA.
        assert!(
            parse_commit_sha(&v).is_none(),
            "release JSON sha/target_commitish must not parse as 40-hex SHA"
        );
        assert_ne!(v["target_commitish"], LATEST_SHA);
        assert_eq!(v["target_commitish"], "main");
    }

    #[test]
    fn parse_ver_handles_build_metadata() {
        assert_eq!(parse_ver("0.2.10+build"), Some((0, 2, 10)));
        assert_eq!(parse_ver("0.2.10-rc.1"), Some((0, 2, 10)));
        assert_eq!(parse_ver("v0.2.10"), Some((0, 2, 10)));
        assert_eq!(parse_ver("0.2.10+build"), parse_ver("0.2.10"));
        assert_eq!(parse_ver("0.2.10-rc.1"), parse_ver("0.2.10"));
        assert_eq!(cmp_versions("0.2.9", "0.2.10"), Some(Ordering::Less));
        assert_eq!(cmp_versions("0.2.11", "0.2.10"), Some(Ordering::Greater));
    }

    fn github_get_when(when: httpmock::When, path: &str, ua: &str) -> httpmock::When {
        when.method(httpmock::Method::GET)
            .path(path)
            .header("User-Agent", ua)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
    }

    fn mock_latest_and_peel(
        server: &httpmock::MockServer,
    ) -> (httpmock::Mock<'_>, httpmock::Mock<'_>) {
        let ua = user_agent();
        let latest_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "tag_name": "v0.2.10",
                        "target_commitish": "main"
                    }"#,
                );
        });
        let peel_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "sha": "c4a2308fe98548899105e33ff38232dfb229ec02",
                        "commit": {
                            "tree": { "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" }
                        }
                    }"#,
                );
        });
        (latest_mock, peel_mock)
    }

    #[test]
    #[serial_test::serial(env)]
    fn peel_commits_json_uses_sha_field() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let (_latest, _peel) = mock_latest_and_peel(&server);
        // Wrong peel (`target_commitish` / branch name) would hit /commits/main.
        let main_decoy = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/main");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"sha":"55cd2dc9d5b6aaaaaaaaaaaaaaaaaaaaaaaaaa"}"#);
        });

        let got = fetch_github_latest(&server.base_url()).expect("peel");
        assert_eq!(got.tag, LATEST_TAG);
        assert_eq!(got.sha, LATEST_SHA);
        assert_ne!(got.sha, "main");
        assert_ne!(got.sha, TIP_SHA);
        assert_eq!(main_decoy.calls(), 0, "must not GET /commits/main");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_honors_ledgerful_no_network() {
        let server = httpmock::MockServer::start();
        let (latest_mock, peel_mock) = mock_latest_and_peel(&server);
        let _g = TempEnv::set(crate::util::network::NO_NETWORK_ENV, "1");
        let result = fetch_github_latest(&server.base_url());
        assert!(
            matches!(result, Err(LatestFetchError::NetworkDisabled)),
            "NO_NETWORK must fail before HTTP: {result:?}"
        );
        assert_eq!(latest_mock.calls(), 0, "zero HTTP hits on releases/latest");
        assert_eq!(peel_mock.calls(), 0, "zero HTTP hits on commits peel");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_sets_user_agent() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let (latest_mock, peel_mock) = mock_latest_and_peel(&server);
        let got = fetch_github_latest(&server.base_url()).expect("fetch with UA");
        assert_eq!(got.sha, LATEST_SHA);
        assert_eq!(latest_mock.calls(), 1, "releases/latest must match UA");
        assert_eq!(peel_mock.calls(), 1, "commits peel must match UA");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_http_429_is_error_no_peel() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let ua = user_agent();
        let latest_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(429)
                .header("content-type", "application/json")
                .body(r#"{"message":"API rate limit exceeded"}"#);
        });
        let peel_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "sha": "c4a2308fe98548899105e33ff38232dfb229ec02"
                    }"#,
                );
        });

        let result = fetch_github_latest(&server.base_url());
        assert!(
            matches!(
                result,
                Err(LatestFetchError::Http {
                    status: Some(429),
                    ..
                })
            ),
            "429 must be Http at fetch layer (no peel): {result:?}"
        );
        assert_eq!(
            latest_mock.calls(),
            1,
            "releases/latest 429 must be hit once (no retry)"
        );
        assert_eq!(peel_mock.calls(), 0, "peel must not run after 4xx latest");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_empty_tag_name_is_invalid_body() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let ua = user_agent();
        let latest_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "tag_name": "",
                        "target_commitish": "main"
                    }"#,
                );
        });
        let peel_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "sha": "c4a2308fe98548899105e33ff38232dfb229ec02"
                    }"#,
                );
        });

        let result = fetch_github_latest(&server.base_url());
        assert!(
            matches!(result, Err(LatestFetchError::InvalidBody(_))),
            "empty tag_name must be InvalidBody: {result:?}"
        );
        assert_eq!(latest_mock.calls(), 1, "releases/latest must be hit once");
        assert_eq!(
            peel_mock.calls(),
            0,
            "peel must not run after empty tag_name"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_invalid_json_is_invalid_body() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let ua = user_agent();
        let latest_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body("{not json");
        });
        let peel_mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "sha": "c4a2308fe98548899105e33ff38232dfb229ec02"
                    }"#,
                );
        });

        let result = fetch_github_latest(&server.base_url());
        assert!(
            matches!(result, Err(LatestFetchError::InvalidBody(_))),
            "invalid JSON must be InvalidBody: {result:?}"
        );
        assert_eq!(latest_mock.calls(), 1, "releases/latest must be hit once");
        assert_eq!(peel_mock.calls(), 0, "peel must not run after invalid JSON");
    }

    #[test]
    #[serial_test::serial(env)]
    fn fetch_non_hex_commit_sha_is_invalid_body() {
        let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let server = httpmock::MockServer::start();
        let ua = user_agent();
        let latest_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "tag_name": "v0.2.10",
                        "target_commitish": "main"
                    }"#,
                );
        });
        let peel_mock = server.mock(|when, then| {
            github_get_when(
                when,
                "/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10",
                ua.as_str(),
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"sha":"main"}"#);
        });

        let result = fetch_github_latest(&server.base_url());
        assert!(
            matches!(result, Err(LatestFetchError::InvalidBody(_))),
            "non-hex peel SHA must be InvalidBody: {result:?}"
        );
        assert_eq!(latest_mock.calls(), 1, "releases/latest must be hit once");
        assert_eq!(
            peel_mock.calls(),
            1,
            "peel must run once; SHA 'main' must not be accepted"
        );
    }

    #[test]
    fn ahead_is_not_action_critical_behind_is() {
        let latest = published();
        let behind = build_behind_latest_finding("0.2.9", OLD_SHA, &latest);
        let ahead = build_ahead_of_latest_finding("0.2.10", TIP_SHA, &latest);
        assert_eq!(behind.severity, DoctorSeverity::Warn);
        assert_eq!(behind.category, DoctorCategory::Tools);
        assert_eq!(ahead.severity, DoctorSeverity::Info);
        assert_eq!(ahead.category, DoctorCategory::Tools);
        assert!(is_action_critical(&behind));
        assert!(!is_action_critical(&ahead));
        assert_eq!(dashboard_failures(std::slice::from_ref(&behind)), 1);
        assert_eq!(dashboard_failures(std::slice::from_ref(&ahead)), 0);
    }

    #[test]
    fn ready_for_publish_true_on_ahead_and_behind_and_unverified() {
        let latest = published();
        let behind = build_behind_latest_finding("0.2.9", OLD_SHA, &latest);
        let ahead = build_ahead_of_latest_finding("0.2.10", TIP_SHA, &latest);
        assert!(ready_for_publish(std::slice::from_ref(&ahead)));
        assert!(ready_for_publish(std::slice::from_ref(&behind)));
        assert!(ready_for_publish(&[]));
        assert!(ready_for_publish(&[ahead.clone(), behind.clone()]));
        assert_ne!(behind.severity, DoctorSeverity::Block);
        assert_ne!(ahead.severity, DoctorSeverity::Block);
    }

    #[test]
    fn t7_0137_remediations_coexist_unmerged() {
        let latest = published();
        let class = classify_engine("0.2.9", OLD_SHA, "0.2.10", TIP_SHA, Some(&latest));
        assert_eq!(codes(&class), vec![BINARY_BEHIND_LATEST_CODE]);
        let behind_latest = &class.findings[0];
        let latest_rem = behind_latest.remediation.as_deref().expect("0205 rem");
        assert!(
            !latest_rem.contains("cargo install --path ."),
            "behind-latest must not install cargo tip: {latest_rem}"
        );
        assert!(
            !latest_rem.contains("--force"),
            "behind-latest must not copy 0137 --force: {latest_rem}"
        );
        assert!(latest_rem.contains(LATEST_TAG));
        assert!(latest_rem.contains("ledgerful --version"));

        let lag = classify_binary_currency("0.2.9", Some("0.2.10"), OLD_SHA, Some(TIP_SHA), true)
            .expect("0137 PATH ≠ HEAD");
        let behind_tree = build_binary_behind_tree_finding(&lag);
        assert_eq!(behind_tree.code, BINARY_BEHIND_TREE_CODE);
        let tree_rem = behind_tree.remediation.as_deref().expect("0137 rem");
        assert_eq!(tree_rem, BINARY_BEHIND_TREE_REMEDIATION);
        assert!(tree_rem.contains("cargo install --path . --force"));

        let mut both = class.findings.clone();
        both.push(behind_tree);
        assert!(both.iter().any(|f| f.code == BINARY_BEHIND_LATEST_CODE));
        assert!(both.iter().any(|f| f.code == BINARY_BEHIND_TREE_CODE));
        assert!(!both.iter().any(|f| f.code == BINARY_AHEAD_OF_LATEST_CODE));
        let latest_text = both
            .iter()
            .find(|f| f.code == BINARY_BEHIND_LATEST_CODE)
            .and_then(|f| f.remediation.as_deref())
            .unwrap_or("");
        assert!(!latest_text.contains("cargo install --path ."));
    }
}
