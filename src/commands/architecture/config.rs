use std::collections::BTreeSet;

#[path = "proposal/policy_limits.rs"]
mod policy_limits;
#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
use std::path::PathBuf;

use globset::{Glob, GlobBuilder};
use miette::{Diagnostic, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub(super) const ARCHITECTURE_SCHEMA_VERSION: u32 = 1;
pub(super) const DEFAULT_CONFIG_REL: &str = ".ledgerful/architecture.toml";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchitectureConfig {
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) components: Vec<ComponentDef>,
    #[serde(default)]
    pub(super) rules: Vec<BoundaryRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentDef {
    pub(super) name: String,
    pub(super) paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BoundaryRule {
    pub(super) id: String,
    pub(super) effect: RuleEffect,
    pub(super) from: String,
    pub(super) to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RuleEffect {
    Allow,
    Forbid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum ConfigSource {
    Local,
    TrustedPath,
}

#[derive(Debug, Error, Diagnostic)]
pub(super) enum ArchitectureError {
    #[error("ARCHITECTURE_CONFIG_NOT_FOUND: {path}")]
    NotFound { path: PathBuf },
    #[error("ARCHITECTURE_CONFIG_UNREADABLE: {path}: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ARCHITECTURE_CONFIG_INVALID: {detail}")]
    Invalid { detail: String },
}

pub(super) fn invalid_config(detail: impl Into<String>) -> ArchitectureError {
    ArchitectureError::Invalid {
        detail: detail.into(),
    }
}

pub(super) fn parse_architecture_toml(text: &str) -> Result<ArchitectureConfig> {
    let config = decode_architecture_toml(text)?;
    validate_config(&config)?;
    Ok(config)
}

fn decode_architecture_toml(text: &str) -> Result<ArchitectureConfig> {
    let toml = toml::Deserializer::parse(text)
        .map_err(|error| toml_error(text, &error, "invalid TOML"))?;
    ArchitectureConfig::deserialize(toml)
        .map_err(|error| toml_error(text, &error, schema_diagnostic(error.message())).into())
}

pub(super) fn parse_bounded_architecture_toml(
    raw: &[u8],
) -> std::result::Result<ArchitectureConfig, &'static str> {
    let text = std::str::from_utf8(raw).map_err(|_| "INVALID_CONTEXT_CONFIG")?;
    policy_limits::admit(text)?;
    parse_architecture_toml(text).map_err(|_| "INVALID_CONTEXT_CONFIG")
}

fn schema_diagnostic(message: &str) -> &'static str {
    // Use only fixed labels. Neither the offending input nor the parser's
    // original diagnostic belongs in the error shown to the caller.
    if message == "missing field `schema_version`" {
        return "the schema_version field is required";
    }
    match message.rsplit_once(", expected ") {
        Some((item, expected)) if item.starts_with("unknown field ") => match expected {
            "one of `schema_version`, `components`, `rules`" => "unknown top-level key",
            "`name` or `paths`" => "unknown component key",
            "one of `id`, `effect`, `from`, `to`" => "unknown rule key",
            _ => "unknown architecture config key",
        },
        Some((item, "`allow` or `forbid`")) if item.starts_with("unknown variant ") => {
            "rule effect must be allow or forbid"
        }
        _ => "does not match the architecture config schema",
    }
}

fn toml_error(text: &str, error: &toml::de::Error, message: &str) -> ArchitectureError {
    // The parser's messages can include arbitrary keys, values, and source excerpts.
    let prefix = error.span().and_then(|span| text.get(..span.start));
    let Some(prefix) = prefix else {
        return invalid_config(message);
    };
    let line = prefix.bytes().filter(|&byte| byte == b'\n').count() + 1;
    let line_prefix = prefix.rsplit('\n').next().unwrap_or("");
    let column = line_prefix.chars().count() + 1;
    invalid_config(format!("{message} at line {line}, column {column}"))
}

fn validate_config(config: &ArchitectureConfig) -> Result<()> {
    if config.schema_version != ARCHITECTURE_SCHEMA_VERSION {
        return Err(invalid_config(format!(
            "schema_version must be {ARCHITECTURE_SCHEMA_VERSION}, found {}",
            config.schema_version
        ))
        .into());
    }
    if config.components.is_empty() {
        return Err(invalid_config("components must contain at least one component").into());
    }

    let mut names = BTreeSet::new();
    for component in &config.components {
        let name = component.name.as_str();
        if !is_valid_component_name(name) {
            return Err(invalid_config(format!("invalid component name {name:?}")).into());
        }
        if !names.insert(name) {
            return Err(invalid_config(format!("duplicate component name {name:?}")).into());
        }
        if component.paths.is_empty() {
            return Err(invalid_config(format!("component {name:?} has no paths")).into());
        }
        for path in &component.paths {
            compile_glob(name, path)?;
        }
    }

    let mut ids = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    for rule in &config.rules {
        if rule.id.is_empty() || !ids.insert(rule.id.as_str()) {
            return Err(invalid_config(format!("empty or duplicate rule id {:?}", rule.id)).into());
        }
        let from = rule.from.as_str();
        let to = rule.to.as_str();
        for (field, endpoint) in [("from", from), ("to", to)] {
            if endpoint != "*" && !names.contains(endpoint) {
                return Err(invalid_config(format!(
                    "rule {:?} {field} names unknown component {endpoint:?}",
                    rule.id
                ))
                .into());
            }
        }
        if from == to && from != "*" {
            return Err(invalid_config(format!(
                "rule {:?} names the same component {from:?} at both endpoints",
                rule.id
            ))
            .into());
        }
        if !pairs.insert((from, to)) {
            return Err(invalid_config(format!(
                "rule {:?} duplicates boundary {from:?} -> {to:?}",
                rule.id
            ))
            .into());
        }
    }
    Ok(())
}

pub(super) fn is_valid_component_name(name: &str) -> bool {
    let Some((first, rest)) = name.as_bytes().split_first() else {
        return false;
    };
    let lower_or_digit = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    name.len() <= 64
        && lower_or_digit(*first)
        && rest
            .iter()
            .all(|&byte| lower_or_digit(byte) || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn compile_glob(component: &str, pattern: &str) -> Result<Glob> {
    let drive_prefix =
        matches!(pattern.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic());
    if pattern.is_empty()
        || pattern.starts_with(['/', '~'])
        || pattern.starts_with("./")
        || drive_prefix
        || pattern.contains('\\')
        || pattern.split('/').any(|segment| segment == "..")
    {
        return Err(invalid_config(format!(
            "component {component:?} has a non-repo-relative path glob {pattern:?}"
        ))
        .into());
    }

    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|error| {
            invalid_config(format!(
                "component {component:?} has invalid path glob {pattern:?}: {error}"
            ))
            .into()
        })
}

pub(super) fn config_digest(raw: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(raw).to_hex())
}
