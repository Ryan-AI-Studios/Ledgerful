use serde::{Deserialize, Serialize};

/// `[review]` configuration — opt-in backends for `ledgerful review`.
///
/// Empty / omitted table is valid. The command still emits git + impact join.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ReviewConfig {
    /// Extra markdown/text files to load as promised requirements.
    #[serde(default)]
    pub requirements_files: Vec<String>,
    /// When non-empty, classify `filesChanged` into intended vs unexpected.
    #[serde(default)]
    pub expected_path_globs: Vec<String>,
    #[serde(default)]
    pub github: ReviewGithubConfig,
    #[serde(default)]
    pub conductor: ReviewConductorConfig,
    #[serde(default)]
    pub ci: ReviewCiConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ReviewGithubConfig {
    /// Default off. When true, `--id` is a pull number.
    #[serde(default)]
    pub enabled: bool,
    /// Optional `owner/repo`. Empty → parse `git remote get-url origin`.
    #[serde(default)]
    pub repo: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ReviewConductorConfig {
    /// Absolute conductor root. Empty = off. Never default to coordinated.
    #[serde(default)]
    pub root: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ReviewCiConfig {
    /// Fetch GitHub check runs when github is enabled. Default off.
    #[serde(default)]
    pub github_checks: bool,
}
