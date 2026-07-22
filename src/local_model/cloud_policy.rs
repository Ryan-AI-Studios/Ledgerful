//! Cloud egress policy for completion / ask / MCP child processes (track 0073).
//!
//! # Propagation
//!
//! MCP `run_ledgerful_tool` sets `LEDGERFUL_CLOUD_POLICY=forbidden` on every
//! tool child (unless host `LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS=1|true`) plus
//! `LEDGERFUL_NON_INTERACTIVE=1`. Children resolve policy via [`CloudPolicy::from_env`].
//!
//! # Caller matrix (Forbidden vs Allowed)
//!
//! | Caller | Policy |
//! |---|---|
//! | MCP tool spawn (all tools) | Forbidden unless host opt-in |
//! | MCP `ask` child | Forbidden (inherits marker) |
//! | CLI `ask --backend local` | Allowed-with-fallback (document) |
//! | CLI `ask --backend gemini` / openrouter / ollama_cloud | Allowed |
//! | CLI `ask` default (no backend) | unchanged legacy |
//! | Semantic extract (`--fast`) | Forbidden when env marker set |
//! | Intent drafter | Forbidden when env marker set |
//! | Bridge CLI spawn | Not HTTP policy; allowlist (M2) |
//!
//! Under Forbidden: zero cloud on complete*, provider chain Local-only,
//! direct Gemini blocked, non-interactive degrade→Gemini blocked.

/// Structured error code substring used in error messages and MCP clients.
pub const CLOUD_POLICY_FORBIDDEN_CODE: &str = "cloud_policy_forbidden";

/// Host-level opt-in env var that MCP parent reads before spawning children.
pub const MCP_ALLOW_CLOUD_EGRESS_ENV: &str = "LEDGERFUL_MCP_ALLOW_CLOUD_EGRESS";

/// Env var set on MCP children to force zero cloud egress.
pub const CLOUD_POLICY_ENV: &str = "LEDGERFUL_CLOUD_POLICY";

/// Value of [`CLOUD_POLICY_ENV`] that forces Forbidden.
pub const CLOUD_POLICY_FORBIDDEN_VALUE: &str = "forbidden";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloudPolicy {
    /// Cloud fallbacks and direct cloud completions are permitted.
    #[default]
    Allowed,
    /// Zero cloud egress: local-only provider chain; hard error if cloud required.
    Forbidden,
}

impl CloudPolicy {
    /// Resolve policy from process environment.
    ///
    /// `LEDGERFUL_CLOUD_POLICY=forbidden` (case-insensitive) → Forbidden.
    /// Repo config / `.env` cannot clear Forbidden once this marker is set —
    /// only process env is consulted here; MCP parent is the sole writer of
    /// the marker (host opt-in decides whether the marker is set at spawn).
    pub fn from_env() -> Self {
        match std::env::var(CLOUD_POLICY_ENV) {
            Ok(v) if v.trim().eq_ignore_ascii_case(CLOUD_POLICY_FORBIDDEN_VALUE) => {
                CloudPolicy::Forbidden
            }
            _ => CloudPolicy::Allowed,
        }
    }

    pub fn is_forbidden(self) -> bool {
        matches!(self, CloudPolicy::Forbidden)
    }

    pub fn is_allowed(self) -> bool {
        matches!(self, CloudPolicy::Allowed)
    }
}

/// True when the host has opted into MCP cloud egress (`1` or `true`, case-insensitive).
pub fn mcp_allow_cloud_egress_from_env() -> bool {
    std::env::var(MCP_ALLOW_CLOUD_EGRESS_ENV)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Env pairs to apply to every MCP tool subprocess spawn.
///
/// Always sets `LEDGERFUL_NON_INTERACTIVE=1`. Sets
/// `LEDGERFUL_CLOUD_POLICY=forbidden` unless host allow-cloud opt-in is true.
pub fn mcp_tool_spawn_env() -> Vec<(String, String)> {
    let mut env = vec![("LEDGERFUL_NON_INTERACTIVE".to_string(), "1".to_string())];
    if !mcp_allow_cloud_egress_from_env() {
        env.push((
            CLOUD_POLICY_ENV.to_string(),
            CLOUD_POLICY_FORBIDDEN_VALUE.to_string(),
        ));
    }
    env
}

/// Build a structured Forbidden error naming the opt-in env var.
pub fn cloud_policy_forbidden_error(context: &str) -> String {
    format!(
        "{CLOUD_POLICY_FORBIDDEN_CODE}: {context}. \
         Cloud egress is blocked by {CLOUD_POLICY_ENV}={CLOUD_POLICY_FORBIDDEN_VALUE}. \
         Host opt-in: set {MCP_ALLOW_CLOUD_EGRESS_ENV}=1 (MCP parent only; repo config/.env cannot clear Forbidden)."
    )
}

/// Return an error if policy is Forbidden (for direct cloud entrypoints).
pub fn deny_if_forbidden(context: &str) -> Result<(), String> {
    if CloudPolicy::from_env().is_forbidden() {
        Err(cloud_policy_forbidden_error(context))
    } else {
        Ok(())
    }
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

    #[test]
    #[serial_test::serial(env)]
    fn from_env_default_allowed() {
        let _g = TempEnv::remove(CLOUD_POLICY_ENV);
        assert_eq!(CloudPolicy::from_env(), CloudPolicy::Allowed);
    }

    #[test]
    #[serial_test::serial(env)]
    fn from_env_forbidden_case_insensitive() {
        let _g = TempEnv::set(CLOUD_POLICY_ENV, "Forbidden");
        assert_eq!(CloudPolicy::from_env(), CloudPolicy::Forbidden);
    }

    #[test]
    #[serial_test::serial(env)]
    fn mcp_spawn_env_sets_forbidden_and_non_interactive() {
        let _a = TempEnv::remove(MCP_ALLOW_CLOUD_EGRESS_ENV);
        let env = mcp_tool_spawn_env();
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEDGERFUL_NON_INTERACTIVE" && v == "1")
        );
        assert!(
            env.iter()
                .any(|(k, v)| k == CLOUD_POLICY_ENV && v == CLOUD_POLICY_FORBIDDEN_VALUE)
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn mcp_spawn_env_omits_forbidden_when_allow_cloud() {
        let _a = TempEnv::set(MCP_ALLOW_CLOUD_EGRESS_ENV, "1");
        let env = mcp_tool_spawn_env();
        assert!(
            env.iter()
                .any(|(k, v)| k == "LEDGERFUL_NON_INTERACTIVE" && v == "1")
        );
        assert!(!env.iter().any(|(k, _)| k == CLOUD_POLICY_ENV));
    }

    #[test]
    #[serial_test::serial(env)]
    fn mcp_spawn_env_allow_true_case_insensitive() {
        let _a = TempEnv::set(MCP_ALLOW_CLOUD_EGRESS_ENV, "TRUE");
        let env = mcp_tool_spawn_env();
        assert!(!env.iter().any(|(k, _)| k == CLOUD_POLICY_ENV));
    }

    #[test]
    fn forbidden_error_names_code_and_opt_in() {
        let err = cloud_policy_forbidden_error("test context");
        assert!(err.contains(CLOUD_POLICY_FORBIDDEN_CODE));
        assert!(err.contains(MCP_ALLOW_CLOUD_EGRESS_ENV));
        assert!(err.contains(CLOUD_POLICY_ENV));
    }
}
