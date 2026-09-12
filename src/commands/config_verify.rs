use crate::commands::ask::{self, Backend};
use crate::config::model::Config;
use comfy_table::Table;
use serde::Serialize;

#[derive(Serialize)]
pub struct SectionReport {
    pub section: String,
    pub rows: Vec<ConfigRow>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ConfigRow {
    pub label: String,
    pub value: String,
    pub source: ValueSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<RowOrigin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip)]
    pub toml_key: Option<&'static str>,
}

impl ConfigRow {
    fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        source: ValueSource,
        toml_key: Option<&'static str>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            source,
            origin: None,
            location: None,
            toml_key,
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RowOrigin {
    File,
    Env,
    Dotenv,
    Default,
}

/// On-disk TOML key presence + config path for provenance (0323).
#[derive(Clone, Debug, Default)]
pub struct ProvenanceContext {
    pub keys: std::collections::BTreeSet<String>,
    pub config_location: String,
    raw: Option<toml::Value>,
}

impl ProvenanceContext {
    pub fn from_layout(layout: &crate::state::layout::Layout) -> Self {
        let path = layout.config_file();
        let config_location = path
            .strip_prefix(&layout.root)
            .map(|rel| rel.as_str().replace('\\', "/"))
            .unwrap_or_else(|_| ".ledgerful/config.toml".to_string());
        let raw = if path.exists() {
            std::fs::read_to_string(path.as_std_path())
                .ok()
                .and_then(|content| toml::from_str::<toml::Value>(&content).ok())
        } else {
            None
        };
        let keys = raw.as_ref().map(present_toml_keys).unwrap_or_default();
        Self {
            keys,
            config_location,
            raw,
        }
    }

    pub fn from_current_layout() -> Self {
        match crate::commands::helpers::get_layout() {
            Ok(layout) => Self::from_layout(&layout),
            Err(_) => Self::default(),
        }
    }
}

pub fn present_toml_keys(value: &toml::Value) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    collect_toml_keys(value, "", &mut keys);
    keys
}

fn collect_toml_keys(
    value: &toml::Value,
    prefix: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.insert(path.clone());
                collect_toml_keys(child, &path, out);
            }
        }
        toml::Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let path = format!("{prefix}[{idx}]");
                out.insert(path.clone());
                collect_toml_keys(child, &path, out);
            }
        }
        _ => {}
    }
}

pub fn strip_url_userinfo(value: &str) -> String {
    let Some(scheme_at) = value.find("://") else {
        return value.to_string();
    };
    let rest_start = scheme_at + 3;
    let rest = &value[rest_start..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return value.to_string();
    };
    format!(
        "{}{}{}",
        &value[..rest_start],
        &authority[at + 1..],
        &rest[authority_end..]
    )
}

fn apply_provenance(row: &mut ConfigRow, ctx: &ProvenanceContext) {
    if row.label == "base_url" {
        row.value = strip_url_userinfo(&row.value);
    }
    let Some(toml_key) = row.toml_key else {
        return;
    };
    if toml_key == "local_model.base_url" && !toml_string_nonempty(ctx.raw.as_ref(), toml_key) {
        if std::env::var("LEDGERFUL_LOCAL_MODEL_URL")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            row.origin = Some(RowOrigin::Env);
            row.location = Some("LEDGERFUL_LOCAL_MODEL_URL".to_string());
            row.source = ValueSource::Explicit;
            return;
        }
        if crate::config::model::read_env_key("LEDGERFUL_LOCAL_MODEL_URL")
            .is_some_and(|v| !v.trim().is_empty())
        {
            row.origin = Some(RowOrigin::Dotenv);
            row.location = Some("LEDGERFUL_LOCAL_MODEL_URL".to_string());
            row.source = ValueSource::Explicit;
            return;
        }
    }
    if ctx.keys.contains(toml_key) {
        row.origin = Some(RowOrigin::File);
        row.location = Some(ctx.config_location.clone());
        row.source = ValueSource::Explicit;
    } else {
        row.origin = Some(RowOrigin::Default);
        row.location = None;
        row.source = ValueSource::Default;
    }
}

fn toml_string_nonempty(root: Option<&toml::Value>, dotted: &str) -> bool {
    let mut cur = match root {
        Some(value) => value,
        None => return false,
    };
    for part in dotted.split('.') {
        match cur.get(part) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn source_cell(row: &ConfigRow) -> String {
    match row.origin {
        Some(RowOrigin::File) => "explicit (file)".to_string(),
        Some(RowOrigin::Env) => format!(
            "explicit (env:{})",
            row.location
                .as_deref()
                .unwrap_or("LEDGERFUL_LOCAL_MODEL_URL")
        ),
        Some(RowOrigin::Dotenv) => format!(
            "explicit (dotenv:{})",
            row.location
                .as_deref()
                .unwrap_or("LEDGERFUL_LOCAL_MODEL_URL")
        ),
        Some(RowOrigin::Default) => "default".to_string(),
        None => row.source.to_string(),
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ValueSource {
    Explicit,
    Default,
    Auto,
    Inherited,
}

impl std::fmt::Display for ValueSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explicit => write!(f, "explicit"),
            Self::Default => write!(f, "default"),
            Self::Auto => write!(f, "auto-derived"),
            Self::Inherited => write!(f, "inherited"),
        }
    }
}

pub trait ConfigSection {
    fn name(&self) -> &'static str;
    fn order(&self) -> u8;
    fn is_applicable(&self, _config: &Config) -> bool {
        true
    }
    fn render_rows(&self, config: &Config) -> Vec<ConfigRow>;
}

pub fn all_sections() -> Vec<Box<dyn ConfigSection>> {
    vec![
        Box::new(BackendSection),
        Box::new(SemanticSection),
        Box::new(AskSection),
        Box::new(GateSection),
    ]
}

pub fn render_verify_report(
    config: &Config,
    json: bool,
    section_filter: Option<&str>,
    verbose: bool,
) -> miette::Result<String> {
    render_verify_report_with(
        config,
        json,
        section_filter,
        verbose,
        &ProvenanceContext::from_current_layout(),
    )
}

pub fn render_verify_report_with(
    config: &Config,
    json: bool,
    section_filter: Option<&str>,
    verbose: bool,
    ctx: &ProvenanceContext,
) -> miette::Result<String> {
    let mut sections = all_sections();
    sections.sort_by_key(|s| s.order());

    if let Some(filter) = section_filter {
        let valid = sections
            .iter()
            .any(|s| s.name().eq_ignore_ascii_case(filter));
        if !valid {
            return Err(miette::miette!("Section '{}' not found in config", filter));
        }
    }

    let filtered_sections: Vec<_> = sections
        .into_iter()
        .filter(|s| {
            if let Some(filter) = section_filter {
                s.name().eq_ignore_ascii_case(filter)
            } else {
                s.is_applicable(config)
            }
        })
        .collect();

    let mut reports = Vec::new();
    for section in filtered_sections {
        let mut rows = section.render_rows(config);
        for row in &mut rows {
            apply_provenance(row, ctx);
        }
        if !verbose {
            rows.retain(|r| r.source != ValueSource::Default);
        }
        if !rows.is_empty() || verbose {
            reports.push(SectionReport {
                section: section.name().to_string(),
                rows,
            });
        }
    }

    if json {
        Ok(serde_json::to_string_pretty(&reports)
            .map_err(|e| miette::miette!("Failed to serialize config verify report: {e}"))?)
    } else {
        let mut table = Table::new();
        table.set_header(vec!["Section", "Key", "Value", "Source"]);
        for report in &reports {
            for row in &report.rows {
                let source = source_cell(row);
                table.add_row([
                    report.section.as_str(),
                    row.label.as_str(),
                    row.value.as_str(),
                    source.as_str(),
                ]);
            }
        }
        Ok(table.to_string())
    }
}

pub struct BackendSection;

impl ConfigSection for BackendSection {
    fn name(&self) -> &'static str {
        "Backend"
    }

    fn order(&self) -> u8 {
        1
    }

    fn render_rows(&self, config: &Config) -> Vec<ConfigRow> {
        let mut rows = Vec::new();

        let env_reader = |name: &str| std::env::var(name).ok();
        let dotenv_reader = |name: &str| crate::config::model::read_env_key(name);

        let resolved = ask::resolve_backend_with(config, None, &env_reader, &dotenv_reader);

        // Prefer local setting row
        rows.push(ConfigRow::new(
            "prefer_local",
            config.local_model.prefer_local.to_string(),
            if config.local_model.prefer_local {
                ValueSource::Explicit
            } else {
                ValueSource::Default
            },
            Some("local_model.prefer_local"),
        ));

        match resolved {
            Backend::Gemini => {
                let has_key = has_gemini_api_key_with(config, &env_reader, &dotenv_reader);
                rows.push(ConfigRow::new("type", "Gemini", ValueSource::Auto, None));
                rows.push(ConfigRow::new(
                    "api_key_status",
                    if has_key {
                        "API key present"
                    } else {
                        "API key missing"
                    },
                    ValueSource::Auto,
                    None,
                ));
            }
            Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
                let base_url =
                    if crate::local_model::client::has_ollama_cloud_fallback(&config.local_model)
                        && config.local_model.base_url.is_empty()
                    {
                        "Ollama Cloud fallback".to_string()
                    } else if config.local_model.base_url.is_empty() {
                        "(not configured)".to_string()
                    } else {
                        config.local_model.base_url.clone()
                    };

                rows.push(ConfigRow::new("type", "Local", ValueSource::Auto, None));
                rows.push(ConfigRow::new(
                    "base_url",
                    base_url,
                    if config.local_model.base_url.is_empty() {
                        ValueSource::Default
                    } else {
                        ValueSource::Explicit
                    },
                    Some("local_model.base_url"),
                ));
            }
        }

        rows
    }
}

fn has_gemini_api_key_with(
    config: &Config,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if config
        .gemini
        .api_key
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty())
    {
        return true;
    }
    if env_reader("GEMINI_API_KEY")
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty())
    {
        return true;
    }
    if dotenv_reader("GEMINI_API_KEY")
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty())
    {
        return true;
    }
    false
}

/// U22: surfaces the resolved timeout values that `ledgerful ask` uses.
/// The CLI `--timeout` flag always overrides these at runtime (default 15s);
/// this section documents the *fallback* values from config.
pub struct AskSection;

impl ConfigSection for AskSection {
    fn name(&self) -> &'static str {
        "Ask"
    }

    fn order(&self) -> u8 {
        3
    }

    fn render_rows(&self, config: &Config) -> Vec<ConfigRow> {
        let mut rows = Vec::new();

        rows.push(ConfigRow::new(
            "cli_default_timeout_secs",
            "15",
            ValueSource::Default,
            None,
        ));

        let local = config.local_model.timeout_secs;
        rows.push(ConfigRow::new(
            "local_model.timeout_secs",
            local.to_string(),
            if local == 60 {
                ValueSource::Default
            } else {
                ValueSource::Explicit
            },
            Some("local_model.timeout_secs"),
        ));

        let gemini_value = config
            .gemini
            .timeout_secs
            .map(|v| v.to_string())
            .unwrap_or_else(|| "120 (default)".to_string());
        let gemini_source = if config.gemini.timeout_secs.is_some() {
            ValueSource::Explicit
        } else {
            ValueSource::Default
        };
        rows.push(ConfigRow::new(
            "gemini.timeout_secs",
            gemini_value,
            gemini_source,
            Some("gemini.timeout_secs"),
        ));

        rows
    }
}

pub struct SemanticSection;

impl ConfigSection for SemanticSection {
    fn name(&self) -> &'static str {
        "Semantic"
    }

    fn order(&self) -> u8 {
        2
    }

    fn render_rows(&self, config: &Config) -> Vec<ConfigRow> {
        let mut rows = Vec::new();

        let available_parallelism = std::thread::available_parallelism().ok().map(|n| {
            std::num::NonZeroUsize::new(n.get()).expect("available_parallelism is non-zero")
        });
        let resolve_opts = crate::semantic::concurrency::ResolveOptions {
            available_parallelism,
            ..Default::default()
        };
        let resolved = crate::semantic::concurrency::resolve_split_semantic_concurrency(
            None,
            &config.semantic,
            config.local_model.concurrency,
            resolve_opts,
        );

        rows.push(ConfigRow::new(
            "parse_threads",
            resolved.parse_threads.get().to_string(),
            match resolved.parse_source {
                crate::semantic::concurrency::ConcurrencySource::Cli => ValueSource::Explicit,
                crate::semantic::concurrency::ConcurrencySource::ConfigParse => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbed => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbedCap => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLegacy => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLocalModel => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::Default => ValueSource::Default,
                crate::semantic::concurrency::ConcurrencySource::Auto => ValueSource::Auto,
            },
            None,
        ));

        rows.push(ConfigRow::new(
            "embed_concurrency",
            resolved.requested_embed_threads.get().to_string(),
            match resolved.embed_source {
                crate::semantic::concurrency::ConcurrencySource::Cli => ValueSource::Explicit,
                crate::semantic::concurrency::ConcurrencySource::ConfigParse => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbed => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbedCap => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLegacy => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLocalModel => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::Default => ValueSource::Default,
                crate::semantic::concurrency::ConcurrencySource::Auto => ValueSource::Auto,
            },
            None,
        ));

        let effective_source = if resolved.embed_threads.get()
            < resolved.requested_embed_threads.get()
        {
            ValueSource::Auto
        } else {
            match resolved.embed_source {
                crate::semantic::concurrency::ConcurrencySource::Cli => ValueSource::Explicit,
                crate::semantic::concurrency::ConcurrencySource::ConfigParse => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbed => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbedCap => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLegacy => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLocalModel => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::Default => ValueSource::Default,
                crate::semantic::concurrency::ConcurrencySource::Auto => ValueSource::Auto,
            }
        };

        rows.push(ConfigRow::new(
            "embed_concurrency_effective",
            resolved.embed_threads.get().to_string(),
            effective_source,
            None,
        ));

        rows.push(ConfigRow::new(
            "embed_concurrency_cap",
            resolved.embed_cap.get().to_string(),
            match resolved.cap_source {
                crate::semantic::concurrency::ConcurrencySource::Cli => ValueSource::Explicit,
                crate::semantic::concurrency::ConcurrencySource::ConfigParse => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbed => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigEmbedCap => {
                    ValueSource::Explicit
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLegacy => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::ConfigLocalModel => {
                    ValueSource::Inherited
                }
                crate::semantic::concurrency::ConcurrencySource::Default => ValueSource::Default,
                crate::semantic::concurrency::ConcurrencySource::Auto => ValueSource::Auto,
            },
            None,
        ));

        rows.push(ConfigRow::new(
            "hnsw_rebuild_threshold",
            config.semantic.hnsw_rebuild_threshold().to_string(),
            ValueSource::Default,
            Some("semantic.hnsw_rebuild_threshold"),
        ));

        rows
    }
}

pub struct GateSection;

impl ConfigSection for GateSection {
    fn name(&self) -> &'static str {
        "Gate"
    }

    fn order(&self) -> u8 {
        4
    }

    fn render_rows(&self, config: &Config) -> Vec<ConfigRow> {
        let mut rows = Vec::new();

        rows.push(ConfigRow::new(
            "mode",
            config.gate.mode.clone(),
            if config.gate.mode == "observe" {
                ValueSource::Default
            } else {
                ValueSource::Explicit
            },
            Some("gate.mode"),
        ));

        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sections_returns_all_implementations() {
        let sections = all_sections();
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].name(), "Backend");
        assert_eq!(sections[1].name(), "Semantic");
        assert_eq!(sections[2].name(), "Ask");
        assert_eq!(sections[3].name(), "Gate");
    }

    #[test]
    fn test_value_source_serialization() {
        let row = ConfigRow::new("test", "1", ValueSource::Explicit, None);
        let serialized = serde_json::to_string(&row).unwrap();
        assert!(serialized.contains("explicit"));
    }

    #[test]
    fn test_render_verify_report_human() {
        let config = Config::default();
        let report = render_verify_report(&config, false, None, true).unwrap();
        assert!(report.contains("Backend"));
        assert!(report.contains("Semantic"));
        assert!(report.contains("Ask"));
    }

    #[test]
    fn test_ask_section_shows_timeout() {
        let config = Config::default();
        let report = render_verify_report(&config, true, Some("Ask"), true).unwrap();
        assert!(report.contains("cli_default_timeout_secs"));
        assert!(report.contains("\"15\""));
        assert!(report.contains("local_model.timeout_secs"));
        assert!(report.contains("gemini.timeout_secs"));
    }

    #[test]
    fn test_ask_section_marks_overridden_values_explicit() {
        let mut config = Config::default();
        config.local_model.timeout_secs = 5;
        config.gemini.timeout_secs = Some(45);
        let report = render_verify_report(&config, true, Some("Ask"), true).unwrap();
        // 5s local + 45s gemini should both be marked as explicit
        assert!(report.contains("\"5\""));
        assert!(report.contains("\"45\""));
    }

    #[test]
    fn test_render_verify_report_unknown_section_fails() {
        let config = Config::default();
        let res = render_verify_report(&config, false, Some("NonExistentSection"), true);
        assert!(res.is_err());
        assert_eq!(
            res.err().unwrap().to_string(),
            "Section 'NonExistentSection' not found in config"
        );
    }

    #[test]
    fn test_embed_concurrency_report() {
        let config = Config::default();
        let report = render_verify_report(&config, true, Some("Semantic"), true).unwrap();
        assert!(report.contains("embed_concurrency"));
        assert!(report.contains("embed_concurrency_effective"));
        assert!(report.contains("embed_concurrency_cap"));
    }

    fn ctx_from_toml(toml: &str, location: &str) -> ProvenanceContext {
        let raw = toml::from_str::<toml::Value>(toml).expect("valid toml fixture");
        ProvenanceContext {
            keys: present_toml_keys(&raw),
            config_location: location.to_string(),
            raw: Some(raw),
        }
    }

    #[test]
    fn present_toml_keys_walks_dotted_tables() {
        let raw = toml::from_str::<toml::Value>("[gate]\nmode = \"enforce\"\n").unwrap();
        let keys = present_toml_keys(&raw);
        assert!(keys.contains("gate"));
        assert!(keys.contains("gate.mode"));
        assert!(!keys.contains("mode"));
    }

    #[test]
    fn gate_mode_present_is_file_even_when_observe() {
        let ctx = ctx_from_toml("[gate]\nmode = \"observe\"\n", ".ledgerful/config.toml");
        let mut config = Config::default();
        config.gate.mode = "observe".to_string();
        let report = render_verify_report_with(&config, true, Some("Gate"), true, &ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&report).unwrap();
        let row = &v[0]["rows"][0];
        assert_eq!(row["label"], "mode");
        assert_eq!(row["origin"], "file");
        assert_eq!(row["source"], "explicit");
        assert!(row["location"].as_str().unwrap().ends_with("config.toml"));
        assert!(row.get("toml_key").is_none());
    }

    #[test]
    fn gate_mode_enforce_is_file() {
        let ctx = ctx_from_toml(
            "[gate]\nmode = \"enforce\"\n",
            "repo/.ledgerful/config.toml",
        );
        let mut config = Config::default();
        config.gate.mode = "enforce".to_string();
        let report = render_verify_report_with(&config, true, Some("Gate"), true, &ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(v[0]["rows"][0]["origin"], "file");
        assert_eq!(v[0]["rows"][0]["source"], "explicit");
        let human = render_verify_report_with(&config, false, Some("Gate"), true, &ctx).unwrap();
        assert!(human.contains("explicit (file)"), "{human}");
    }

    #[test]
    fn absent_toml_key_is_default() {
        let ctx = ProvenanceContext::default();
        let config = Config::default();
        let report = render_verify_report_with(&config, true, Some("Gate"), true, &ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&report).unwrap();
        assert_eq!(v[0]["rows"][0]["origin"], "default");
        assert_eq!(v[0]["rows"][0]["source"], "default");
        assert!(v[0]["rows"][0].get("location").is_none());
    }

    #[test]
    fn derived_rows_omit_origin_and_location() {
        let ctx = ProvenanceContext::default();
        let config = Config::default();
        let report = render_verify_report_with(&config, true, Some("Ask"), true, &ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&report).unwrap();
        let rows = v[0]["rows"].as_array().unwrap();
        let cli = rows
            .iter()
            .find(|r| r["label"] == "cli_default_timeout_secs")
            .unwrap();
        assert!(cli.get("origin").is_none());
        assert!(cli.get("location").is_none());

        let backend =
            render_verify_report_with(&config, true, Some("Backend"), true, &ctx).unwrap();
        let bv: serde_json::Value = serde_json::from_str(&backend).unwrap();
        for label in ["type", "api_key_status"] {
            if let Some(row) = bv[0]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["label"] == label)
            {
                assert!(row.get("origin").is_none(), "{label}: {row}");
                assert!(row.get("location").is_none(), "{label}: {row}");
            }
        }
        let ty = bv[0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == "type")
            .unwrap();
        assert!(ty.get("origin").is_none());
        assert!(ty.get("location").is_none());
    }

    #[test]
    fn strip_url_userinfo_removes_credentials() {
        assert_eq!(
            strip_url_userinfo("http://user:pass@host:11434"),
            "http://host:11434"
        );
        assert_eq!(strip_url_userinfo("http://host:11434"), "http://host:11434");
        assert_eq!(
            strip_url_userinfo("http://token@host:11434"),
            "http://host:11434"
        );
        assert_eq!(
            strip_url_userinfo("http://host/path@segment"),
            "http://host/path@segment"
        );
        assert_eq!(
            strip_url_userinfo("http://user:pass@host/path@x"),
            "http://host/path@x"
        );
    }

    #[test]
    fn apply_provenance_strips_base_url_userinfo() {
        let ctx = ctx_from_toml(
            "[local_model]\nbase_url = \"http://user:pass@host:1\"\n",
            ".ledgerful/config.toml",
        );
        let mut row = ConfigRow::new(
            "base_url",
            "http://user:pass@host:1",
            ValueSource::Explicit,
            Some("local_model.base_url"),
        );
        apply_provenance(&mut row, &ctx);
        assert_eq!(row.value, "http://host:1");
        assert_eq!(row.origin, Some(RowOrigin::File));
    }
}
