use crate::config::ConfigError;
use camino::Utf8PathBuf;
use std::ffi::OsString;
use std::fs;

pub const DEFAULT_CONFIG: &str = r#"[core]
strict = false
auto_fix = false

[watch]
debounce_ms = 1000
ignore_patterns = [
    "target", "target/**", ".git", ".git/**", "node_modules", "node_modules/**",
    ".claude", ".claude/**", ".codex", ".codex/**", ".opencode", ".opencode/**",
    ".agents", ".agents/**", ".ledgerful", ".ledgerful/**"
]

[temporal]
max_commits = 1000
max_files_per_commit = 50
coupling_threshold = 0.75
min_shared_commits = 3
min_revisions = 5
decay_half_life = 100

[hotspots]
max_commits = 500
limit = 10
history_budget_secs = 45
overall_budget_secs = 25
# A canonical list records hotspot_history. --snapshot still persists when this is false.
auto_snapshot = true
# budget_threshold = 0.5  # informational default when omitted; required for --fail

[verify]
mode = "auto"
# default_timeout_secs = 300
# suite_timeout_secs raises or lowers auto-plan cargo test / cargo nextest only.
# An explicit value replaces DEFAULT_TEST_SUITE_TIMEOUT_SECS in either direction.
# Omitted keeps that built-in. Explicit [[verify.steps]] ignore it.
# suite_timeout_secs = 900
# Process policy (default-strict with a built-in cargo/npm/pytest/git allowlist).
# allowed_commands extends the built-in list (does not replace it):
# allowed_commands = ["my-custom-tool"]
# denied_commands = ["curl"]
# strict = true
# allow_shell_steps = false
# Steps to run when `ledgerful verify` is invoked without -c.
# Each step has a description, command, optional timeout_secs (defaults to 300),
# and optional shell = true (requires allow_shell_steps = true).
# [[verify.steps]]
# description = "Run project tests"
# command = "cargo nextest run --workspace --all-features --profile ci"
# timeout_secs = 300
# [[verify.steps]]
# description = "Check formatting"
# command = "cargo fmt --check"

[gemini]
# Prefer GEMINI_API_KEY in the environment or local .env.
# api_key = "..."
# Optional override for every ask mode:
# model = "gemini-3.1-pro"
fast_model = "gemini-3.1-flash-lite"
deep_model = "gemini-3.1-pro"
timeout_secs = 120
context_window = 128000

[index]
stale_threshold_days = 3

[local_model]
# Use 127.0.0.1 — 'localhost' resolves to ::1 (IPv6) on Windows, which breaks IPv4-only servers
base_url = "http://127.0.0.1:8081"
embedding_url = "http://127.0.0.1:8083"
# Default ask budget for Local when CLI --timeout is omitted (default 300).
# Cold on-demand model loads often need well more than 15s; set lower for
# snappy fail, or raise --timeout per request.
# timeout_secs = 300
# Optional fallback for Ollama Cloud completions.
# Prefer OLLAMA_CLOUD_* environment variables or local .env for secrets.
# Native:   ollama_cloud_url = "https://ollama.com/api"  (POST /api/chat)
# OpenAI:   ollama_cloud_url = "https://ollama.com"      (POST /v1/chat/completions)
# NOTE: https://api.ollama.com does NOT support /v1/chat/completions
# ollama_cloud_model = "minimax-m3:cloud"
# Backward-compatible alias: ollama_key = "..."  (same as ollama_cloud_api_key)

[semantic]
hnsw_rebuild_threshold = 500
# parse_concurrency = 8
# embed_concurrency = 4
# embed_concurrency_cap = 4
# Legacy combined field:
# concurrency = 8

[intent]
required = "always"
tui_enabled = true
require_signing = false
# Hex Ed25519 public keys (64 chars). Empty = soft pin (VALID unknown key).
# trusted_public_keys = []
min_sig_version = 1

[gate]
mode = "observe"

# [doctor]
# Finding codes whose human bodies are suppressed (0226). Sorted. Stale
# codes (no finding this run) are inert — no GC.
# acknowledged_codes = []

[bridge]
# Master switch for the Ledgerful data-interchange bridge. Default is false.
# When false, Ledgerful performs zero implicit bridge activity (no IPC, no
# provider spawn, no automatic verify/watch pushes). Explicit `bridge export`
# and `bridge import` remain usable because they are pure-local I/O.
enabled = false

# External context-provider binary invoked by `bridge query` when the IPC path
# is unavailable. The default `ai-brains` is the documented example provider.
# Must accept `sync query <query> --format ndjson` on stdin and emit
# `BridgeRecord` NDJSON lines on stdout.
provider_command = "ai-brains"

# Overall emit budget for `bridge export --hotspots` (0394). Distinct from
# `[hotspots] history_budget_secs`. `0` disables the wall clock.
# export_overall_budget_secs = 25

[impact]
# Overall emit budget for prospective `--paths`, working-tree, and `--base-ref`
# (0347 / 0419). Distinct from `[hotspots] history_budget_secs`. `0` disables
# the wall clock (unbounded; may hang).
prospective_budget_secs = 25

[review]
# Overall emit budget for `ledgerful review` (0348). Distinct from
# `[hotspots] history_budget_secs` and `[impact] prospective_budget_secs`.
# `0` disables the wall clock. CLI `--timeout` on `review` is this budget.
overall_budget_secs = 25

[audit]
# Overall emit budget for unscoped `audit` / `ledger audit` (0350). Distinct
# from `[hotspots] history_budget_secs`. `0` disables the wall clock.
# CLI `--timeout` on unscoped audit is this budget, not the history walk.
overall_budget_secs = 25

[coverage.deploy]
# Overall emit budget for `ledgerful deploy impact` (0358). Distinct from
# `[impact] prospective_budget_secs`. `0` disables the wall clock.
# CLI `--timeout` on `deploy impact` (not parent `deploy`) is this budget.
overall_budget_secs = 25

[impact.risk_weights]
rs = 1.0
toml = 0.8
json = 0.7
yml = 0.3
yaml = 0.3
md = 0.1
txt = 0.1
codex = 0.01
claude = 0.01
"#;

pub const DEFAULT_CONFIG_TEMPLATE_ENV: &str = "LEDGERFUL_DEFAULT_CONFIG";
pub const USER_DEFAULT_CONFIG_FILE: &str = "default-config.toml";

pub fn default_config_contents() -> Result<String, ConfigError> {
    if let Some(path) = default_config_template_path()
        && path.exists()
    {
        return fs::read_to_string(path.as_std_path()).map_err(|source| ConfigError::ReadFailed {
            path: path.to_string(),
            source,
        });
    }

    Ok(DEFAULT_CONFIG.to_string())
}

/// Raw snapshot of the environment inputs that select the default template.
///
/// Values are kept as `OsString` so non-UTF-8 input is still visible to the
/// resolver. The type deliberately does not derive `Debug`: callers must not
/// log raw override or home contents.
#[derive(Clone, Default)]
pub(crate) struct TemplateEnv {
    pub(crate) override_path: Option<OsString>,
    pub(crate) userprofile: Option<OsString>,
    pub(crate) home: Option<OsString>,
}

impl TemplateEnv {
    /// Capture the current process environment once.
    pub(crate) fn from_process() -> Self {
        Self {
            override_path: std::env::var_os(DEFAULT_CONFIG_TEMPLATE_ENV),
            userprofile: std::env::var_os("USERPROFILE"),
            home: std::env::var_os("HOME"),
        }
    }
}

/// Which input selected the default template candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateOrigin {
    Override,
    UserProfile,
    Home,
}

/// Outcome of resolving the default template candidate from a [`TemplateEnv`].
pub(crate) enum TemplateSelection {
    /// No candidate: neither the override, USERPROFILE nor HOME is set.
    Absent,
    /// A selected candidate. `path` is `None` when the candidate cannot be
    /// represented as UTF-8; the runtime loader then falls back to built-in.
    Candidate {
        origin: TemplateOrigin,
        path: Option<Utf8PathBuf>,
    },
}

/// Resolve the default template candidate from an explicit snapshot.
///
/// Mirrors the runtime precedence: the override wins even when empty or
/// missing; USERPROFILE wins over HOME even when empty; HOME is ignored when
/// USERPROFILE is present. Non-UTF-8 inputs produce `path: None`.
pub(crate) fn select_template_candidate(env: &TemplateEnv) -> TemplateSelection {
    if let Some(raw) = env.override_path.as_ref() {
        return TemplateSelection::Candidate {
            origin: TemplateOrigin::Override,
            path: os_to_utf8(raw),
        };
    }

    let (origin, raw) = if let Some(raw) = env.userprofile.as_ref() {
        (TemplateOrigin::UserProfile, raw)
    } else if let Some(raw) = env.home.as_ref() {
        (TemplateOrigin::Home, raw)
    } else {
        return TemplateSelection::Absent;
    };

    TemplateSelection::Candidate {
        origin,
        path: os_to_utf8(raw).map(|home| home.join(".ledgerful").join(USER_DEFAULT_CONFIG_FILE)),
    }
}

fn os_to_utf8(raw: &OsString) -> Option<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(raw.into()).ok()
}

pub(crate) fn default_config_template_path() -> Option<Utf8PathBuf> {
    match select_template_candidate(&TemplateEnv::from_process()) {
        TemplateSelection::Candidate { path, .. } => path,
        TemplateSelection::Absent => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;

    #[test]
    fn config_template_uses_127() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.local_model.base_url, "http://127.0.0.1:8081");
        assert_eq!(
            config.local_model.embedding_url.as_deref(),
            Some("http://127.0.0.1:8083")
        );
        assert_eq!(config.gate.mode, "observe");
        assert_eq!(config.review.overall_budget_secs, 25);
        assert_eq!(config.audit.overall_budget_secs, 25);
        assert_eq!(config.hotspots.overall_budget_secs, 25);
        assert_eq!(config.hotspots.history_budget_secs, 45);
        assert!(config.hotspots.auto_snapshot);
        assert_eq!(config.impact.prospective_budget_secs, 25);
        assert_eq!(config.coverage.deploy.overall_budget_secs, 25);
    }

    #[test]
    fn config_template_sets_hnsw_rebuild_threshold() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.semantic.hnsw_rebuild_threshold(), 500);
    }

    #[test]
    fn config_template_excludes_agent_dotfiles() {
        let config: crate::config::model::Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        let patterns = &config.watch.ignore_patterns;
        assert!(
            patterns.iter().any(|p| p == ".claude/**"),
            "missing .claude/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".agents/**"),
            "missing .agents/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".codex/**"),
            "missing .codex/**"
        );
        assert!(
            patterns.iter().any(|p| p == ".opencode/**"),
            "missing .opencode/**"
        );
        assert!(
            patterns.iter().any(|p| p == "target/**"),
            "missing target/**"
        );
    }

    fn env_of(
        override_path: Option<&str>,
        userprofile: Option<&str>,
        home: Option<&str>,
    ) -> TemplateEnv {
        TemplateEnv {
            override_path: override_path.map(OsString::from),
            userprofile: userprofile.map(OsString::from),
            home: home.map(OsString::from),
        }
    }

    fn selected(env: &TemplateEnv) -> Option<(TemplateOrigin, Option<Utf8PathBuf>)> {
        match select_template_candidate(env) {
            TemplateSelection::Absent => None,
            TemplateSelection::Candidate { origin, path } => Some((origin, path)),
        }
    }

    #[test]
    fn user_template_secret_resolver_precedence_table() {
        // Override wins over both homes, even when it is empty.
        assert!(matches!(
            selected(&env_of(Some(""), Some("/u"), Some("/h"))),
            Some((TemplateOrigin::Override, Some(p))) if p.as_str().is_empty()
        ));
        // USERPROFILE wins over HOME; HOME is ignored when USERPROFILE is present.
        assert!(matches!(
            selected(&env_of(None, Some("/u"), Some("/h"))),
            Some((TemplateOrigin::UserProfile, Some(_)))
        ));
        // Empty USERPROFILE still shadows HOME and resolves relative to cwd.
        let empty_user = selected(&env_of(None, Some(""), Some("/h"))).unwrap();
        assert_eq!(empty_user.0, TemplateOrigin::UserProfile);
        assert_eq!(
            empty_user.1.unwrap(),
            Utf8Path::new(".ledgerful").join(USER_DEFAULT_CONFIG_FILE)
        );
        // HOME is used only when USERPROFILE is absent, even if empty.
        let home_only = selected(&env_of(None, None, Some(""))).unwrap();
        assert_eq!(home_only.0, TemplateOrigin::Home);
        assert_eq!(
            home_only.1.unwrap(),
            Utf8Path::new(".ledgerful").join(USER_DEFAULT_CONFIG_FILE)
        );
        // Nothing set: no candidate.
        assert!(selected(&env_of(None, None, None)).is_none());
    }

    #[test]
    fn user_template_secret_resolver_relative_override_is_kept_verbatim() {
        let got = selected(&env_of(Some("relative/template.toml"), None, None)).unwrap();
        assert_eq!(got.0, TemplateOrigin::Override);
        assert_eq!(got.1.unwrap().as_str(), "relative/template.toml");
    }

    #[test]
    fn user_template_secret_resolver_non_utf8_userprofile_has_no_path() {
        #[cfg(unix)]
        let bad: OsString = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0xff, b'x'])
        };
        #[cfg(not(unix))]
        let bad: OsString = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0xD800])
        };
        let env = TemplateEnv {
            override_path: None,
            userprofile: Some(bad),
            home: Some(OsString::from("/valid/home")),
        };
        let got = selected(&env).unwrap();
        assert_eq!(got.0, TemplateOrigin::UserProfile);
        assert!(
            got.1.is_none(),
            "non-UTF-8 USERPROFILE must not fall back to HOME"
        );
    }

    #[test]
    fn user_template_secret_default_template_path_wrapper_is_stable() {
        // The Option-returning wrapper and the descriptor agree on the same snapshot.
        let env = env_of(Some("abc.toml"), None, None);
        let via_descriptor = match select_template_candidate(&env) {
            TemplateSelection::Candidate { path, .. } => path,
            TemplateSelection::Absent => None,
        };
        assert_eq!(
            via_descriptor.map(|p| p.to_string()),
            Some("abc.toml".to_string())
        );
    }
}
