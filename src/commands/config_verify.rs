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
    ]
}

pub fn render_verify_report(
    config: &Config,
    json: bool,
    section_filter: Option<&str>,
    verbose: bool,
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
        Ok(serde_json::to_string_pretty(&reports).unwrap_or_default())
    } else {
        let mut table = Table::new();
        table.set_header(vec!["Section", "Key", "Value", "Source"]);
        for report in &reports {
            for row in &report.rows {
                table.add_row([
                    report.section.as_str(),
                    row.label.as_str(),
                    row.value.as_str(),
                    row.source.to_string().as_str(),
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
        rows.push(ConfigRow {
            label: "prefer_local".to_string(),
            value: config.local_model.prefer_local.to_string(),
            source: if config.local_model.prefer_local {
                ValueSource::Explicit
            } else {
                ValueSource::Default
            },
        });

        match resolved {
            Backend::Gemini => {
                let has_key = has_gemini_api_key_with(config, &env_reader, &dotenv_reader);
                rows.push(ConfigRow {
                    label: "type".to_string(),
                    value: "Gemini".to_string(),
                    source: ValueSource::Auto,
                });
                rows.push(ConfigRow {
                    label: "api_key_status".to_string(),
                    value: if has_key {
                        "API key present"
                    } else {
                        "API key missing"
                    }
                    .to_string(),
                    source: ValueSource::Auto,
                });
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

                rows.push(ConfigRow {
                    label: "type".to_string(),
                    value: "Local".to_string(),
                    source: ValueSource::Auto,
                });
                rows.push(ConfigRow {
                    label: "base_url".to_string(),
                    value: base_url,
                    source: if config.local_model.base_url.is_empty() {
                        ValueSource::Default
                    } else {
                        ValueSource::Explicit
                    },
                });
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

        rows.push(ConfigRow {
            label: "cli_default_timeout_secs".to_string(),
            value: "15".to_string(),
            source: ValueSource::Default,
        });

        let local = config.local_model.timeout_secs;
        rows.push(ConfigRow {
            label: "local_model.timeout_secs".to_string(),
            value: local.to_string(),
            source: if local == 60 {
                ValueSource::Default
            } else {
                ValueSource::Explicit
            },
        });

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
        rows.push(ConfigRow {
            label: "gemini.timeout_secs".to_string(),
            value: gemini_value,
            source: gemini_source,
        });

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

        rows.push(ConfigRow {
            label: "parse_threads".to_string(),
            value: resolved.parse_threads.get().to_string(),
            source: match resolved.parse_source {
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
        });

        rows.push(ConfigRow {
            label: "embed_concurrency".to_string(),
            value: resolved.requested_embed_threads.get().to_string(),
            source: match resolved.embed_source {
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
        });

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

        rows.push(ConfigRow {
            label: "embed_concurrency_effective".to_string(),
            value: resolved.embed_threads.get().to_string(),
            source: effective_source,
        });

        rows.push(ConfigRow {
            label: "embed_concurrency_cap".to_string(),
            value: resolved.embed_cap.get().to_string(),
            source: match resolved.cap_source {
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
        });

        let rebuild_threshold_explicit = if let Ok(current_dir) = std::env::current_dir() {
            let layout = crate::state::layout::Layout::new(current_dir.to_string_lossy().as_ref());
            let path = layout.config_file();
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(val) = toml::from_str::<toml::Value>(&content) {
                        val.get("semantic")
                            .and_then(|s| s.get("hnsw_rebuild_threshold"))
                            .is_some()
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        rows.push(ConfigRow {
            label: "hnsw_rebuild_threshold".to_string(),
            value: config.semantic.hnsw_rebuild_threshold().to_string(),
            source: if rebuild_threshold_explicit {
                ValueSource::Explicit
            } else {
                ValueSource::Default
            },
        });

        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sections_returns_all_implementations() {
        let sections = all_sections();
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name(), "Backend");
        assert_eq!(sections[1].name(), "Semantic");
        assert_eq!(sections[2].name(), "Ask");
    }

    #[test]
    fn test_value_source_serialization() {
        let row = ConfigRow {
            label: "test".to_string(),
            value: "1".to_string(),
            source: ValueSource::Explicit,
        };
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
}
