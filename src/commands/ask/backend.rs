use crate::commands::ask::Backend;
use crate::config::model::Config;
use std::env;

/// Resolve the ordered provider entries with full per-provider config
/// (model, timeout, base_url, api_key_env). Applies env var overrides
/// and --backend CLI flag reordering.
pub fn resolve_provider_entries(
    config: &Config,
    explicit: Option<Backend>,
) -> std::result::Result<Vec<crate::config::model::ProviderEntry>, String> {
    use crate::config::model::{Provider, ProviderEntry};

    let env_reader = |name: &str| env::var(name).ok();

    let mut entries = config.ask.providers.priority.clone();

    // Apply env var overrides (R7)
    for i in 1..=4 {
        let env_name = format!("LEDGERFUL_ASK_PROVIDER_{i}");
        if let Some(val) = env_reader(&env_name) {
            let provider = Provider::from_str_fail_fast(&val, &env_name)?;
            let model = env_reader(&format!("LEDGERFUL_ASK_MODEL_{i}"));
            if i <= entries.len() {
                entries[i - 1].backend = provider;
                if let Some(m) = model {
                    entries[i - 1].model = Some(m);
                }
            } else {
                entries.push(ProviderEntry {
                    backend: provider,
                    model,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                });
            }
        }
    }

    // If config and env vars are empty, use legacy behavior for backward compat
    if entries.is_empty() {
        // 0073 F-001: Forbidden must force Local-only even on the legacy early-return
        // path (defense-in-depth for any resolve_provider_entries caller).
        if crate::local_model::CloudPolicy::from_env().is_forbidden() {
            return Ok(vec![ProviderEntry {
                backend: Provider::Local,
                model: Some(config.local_model.generation_model.clone()),
                timeout_secs: Some(config.local_model.timeout_secs),
                api_key_env: None,
                base_url: Some(config.local_model.base_url.clone()),
            }]);
        }
        let dotenv_reader = |name: &str| crate::config::model::read_env_key(name);
        let legacy = resolve_backend_with(config, explicit, &env_reader, &dotenv_reader);
        let legacy_provider = match legacy {
            Backend::Gemini => Provider::Gemini,
            Backend::Local | Backend::OllamaCloud | Backend::OpenRouter => {
                if crate::local_model::client::has_ollama_cloud_fallback(&config.local_model) {
                    Provider::OllamaCloud
                } else {
                    Provider::Local
                }
            }
        };
        return Ok(vec![ProviderEntry {
            backend: legacy_provider,
            model: None,
            timeout_secs: None,
            api_key_env: None,
            base_url: None,
        }]);
    }

    // --backend override: move specified provider to front (R7a)
    if let Some(b) = explicit {
        let target = match b {
            Backend::Gemini => Provider::Gemini,
            Backend::Local => Provider::Local,
            Backend::OllamaCloud => Provider::OllamaCloud,
            Backend::OpenRouter => Provider::OpenRouter,
        };
        let existing = entries.iter().position(|e| e.backend == target);
        match existing {
            Some(idx) => {
                let entry = entries.remove(idx);
                entries.insert(0, entry);
            }
            None => {
                let default_entry = match target {
                    Provider::OllamaCloud => ProviderEntry {
                        backend: Provider::OllamaCloud,
                        model: config.local_model.ollama_cloud_model.clone(),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: None,
                        base_url: config.local_model.ollama_cloud_url.clone(),
                    },
                    Provider::Gemini => ProviderEntry {
                        backend: Provider::Gemini,
                        model: config.gemini.fast_model.clone(),
                        timeout_secs: config.gemini.timeout_secs,
                        api_key_env: None,
                        base_url: None,
                    },
                    Provider::Local => ProviderEntry {
                        backend: Provider::Local,
                        model: Some(config.local_model.generation_model.clone()),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: None,
                        base_url: Some(config.local_model.base_url.clone()),
                    },
                    Provider::OpenRouter => ProviderEntry {
                        backend: Provider::OpenRouter,
                        model: env_reader("OPENROUTER_MODEL"),
                        timeout_secs: Some(config.local_model.timeout_secs),
                        api_key_env: Some("OPENROUTER_API_KEY".to_string()),
                        base_url: Some("https://openrouter.ai/api/v1".to_string()),
                    },
                };
                entries.insert(0, default_entry);
            }
        }
    }

    // 0073: under CloudPolicy::Forbidden the resolved list is pure Local-only
    // (truncate priority cloud tail + ignore LEDGERFUL_ASK_PROVIDER_* cloud slots).
    if crate::local_model::CloudPolicy::from_env().is_forbidden() {
        entries.retain(|e| e.backend == Provider::Local);
        if entries.is_empty() {
            entries.push(ProviderEntry {
                backend: Provider::Local,
                model: Some(config.local_model.generation_model.clone()),
                timeout_secs: Some(config.local_model.timeout_secs),
                api_key_env: None,
                base_url: Some(config.local_model.base_url.clone()),
            });
        }
    }

    Ok(entries)
}

pub fn resolve_backend(config: &Config, explicit: Option<Backend>) -> Backend {
    resolve_backend_with(config, explicit, &|name| env::var(name).ok(), &|name| {
        crate::config::model::read_env_key(name)
    })
}

pub fn resolve_backend_with(
    config: &Config,
    explicit: Option<Backend>,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> Backend {
    let has_gemini_key = config.gemini.api_key.is_some()
        || env_reader("GEMINI_API_KEY").is_some()
        || dotenv_reader("GEMINI_API_KEY").is_some();

    let has_local = crate::local_model::client::is_configured(&config.local_model);

    if let Some(b) = explicit {
        if b == Backend::Gemini && !has_gemini_key {
            return Backend::Local;
        }
        // New provider variants map to Local for the legacy path
        // (they're handled by the provider priority chain in execute_ask)
        return match b {
            Backend::OllamaCloud | Backend::OpenRouter => Backend::Local,
            other => other,
        };
    }

    if config.local_model.prefer_local && has_local {
        return Backend::Local;
    }

    if !has_gemini_key && has_local {
        return Backend::Local;
    }

    Backend::Gemini
}

/// Resolve the ordered provider priority list (Track TA14).
/// Delegates to `resolve_provider_entries` and maps to `Vec<Provider>`.
pub fn resolve_provider_priority(
    config: &Config,
    explicit: Option<Backend>,
) -> std::result::Result<Vec<crate::config::model::Provider>, String> {
    let entries = resolve_provider_entries(config, explicit)?;
    Ok(entries.into_iter().map(|e| e.backend).collect())
}

/// Sanitize an error message for safe logging (R7b).
/// Strips bearer tokens and api_key= values from the string.
/// Uses `to_ascii_lowercase` (not `to_lowercase`) to preserve byte
/// alignment between the search string and the original — `to_lowercase`
/// can change byte length for non-ASCII characters (e.g. German sharp-s),
/// causing byte-index panics when slicing the original.
pub fn sanitize_error_for_logging(err: &str) -> String {
    let lower = err.to_ascii_lowercase();
    let mut sanitized = err.to_string();

    // Strip bearer tokens (case-insensitive)
    if let Some(idx) = lower.find("bearer ") {
        let start = idx;
        let rest = &sanitized[start + 7..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}bearer [REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 7 + end..]
        );
    }

    // Strip api_key= values
    let lower2 = sanitized.to_ascii_lowercase();
    if let Some(idx) = lower2.find("api_key=") {
        let start = idx;
        let rest = &sanitized[start + 8..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']' || c == '&')
            .unwrap_or(rest.len());
        sanitized = format!(
            "{}api_key=[REDACTED]{}",
            &sanitized[..start],
            &sanitized[start + 8 + end..]
        );
    }

    sanitized
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

    fn clear_provider_env() {
        for key in [
            "LEDGERFUL_ASK_PROVIDER_1",
            "LEDGERFUL_ASK_PROVIDER_2",
            "LEDGERFUL_ASK_PROVIDER_3",
            "LEDGERFUL_ASK_PROVIDER_4",
            "LEDGERFUL_ASK_MODEL_1",
            "LEDGERFUL_ASK_MODEL_2",
            "LEDGERFUL_ASK_MODEL_3",
            "LEDGERFUL_ASK_MODEL_4",
        ] {
            let _ = TempEnv::remove(key);
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_backend_uses_local_when_only_ollama_cloud_is_configured() {
        clear_provider_env();
        let mut config = Config::default();
        config.local_model.ollama_cloud_url = Some("https://api.ollama.com".to_string());
        config.local_model.ollama_cloud_api_key = Some("token".to_string());
        config.local_model.ollama_cloud_model = Some("minimax-m3:cloud".to_string());

        let backend = resolve_backend_with(&config, None, &|_| None, &|_| None);
        assert_eq!(backend, Backend::Local);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_priority_empty_config_falls_back_to_legacy() {
        clear_provider_env();
        let config = Config::default();
        let providers = resolve_provider_priority(&config, None).unwrap();
        assert_eq!(providers.len(), 1);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_priority_with_config_uses_order() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};

        clear_provider_env();
        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![
                ProviderEntry {
                    backend: Provider::OllamaCloud,
                    model: Some("glm-5.2".to_string()),
                    timeout_secs: Some(30),
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::Gemini,
                    model: Some("gemini-3.1-flash-lite".to_string()),
                    timeout_secs: Some(60),
                    api_key_env: None,
                    base_url: None,
                },
            ],
        };

        let providers = resolve_provider_priority(&config, None).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0], Provider::OllamaCloud);
        assert_eq!(providers[1], Provider::Gemini);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_priority_backend_flag_moves_to_front() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};

        clear_provider_env();
        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![
                ProviderEntry {
                    backend: Provider::Gemini,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::Local,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
            ],
        };

        let providers = resolve_provider_priority(&config, Some(Backend::Local)).unwrap();
        assert_eq!(providers[0], Provider::Local);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_entries_forbidden_truncates_to_local_only() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};
        use crate::local_model::cloud_policy::{CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE};

        clear_provider_env();
        let _pol = TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE);
        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![
                ProviderEntry {
                    backend: Provider::Local,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::Gemini,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::OpenRouter,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
                ProviderEntry {
                    backend: Provider::OllamaCloud,
                    model: None,
                    timeout_secs: None,
                    api_key_env: None,
                    base_url: None,
                },
            ],
        };

        let entries = resolve_provider_entries(&config, Some(Backend::Local)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].backend, Provider::Local);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_entries_forbidden_empty_priority_forces_local() {
        // F-001: empty priority early-return must still force Local under Forbidden
        // even when ollama cloud credentials would yield OllamaCloud on the legacy path.
        use crate::config::model::Provider;
        use crate::local_model::cloud_policy::{CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE};

        clear_provider_env();
        let _pol = TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE);
        let mut config = Config::default();
        config.ask.providers.priority.clear();
        config.local_model.ollama_cloud_url = Some("https://api.ollama.com".to_string());
        config.local_model.ollama_cloud_api_key = Some("token".to_string());
        config.local_model.ollama_cloud_model = Some("minimax-m3:cloud".to_string());

        let entries = resolve_provider_entries(&config, Some(Backend::Local)).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "Forbidden empty-priority must be single Local"
        );
        assert_eq!(entries[0].backend, Provider::Local);
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_entries_forbidden_ignores_cloud_env_slots() {
        use crate::config::model::{Provider, ProviderEntry, ProvidersConfig};
        use crate::local_model::cloud_policy::{CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE};

        clear_provider_env();
        let _pol = TempEnv::set(CLOUD_POLICY_ENV, CLOUD_POLICY_FORBIDDEN_VALUE);
        let _p1 = TempEnv::set("LEDGERFUL_ASK_PROVIDER_1", "gemini");
        let _p2 = TempEnv::set("LEDGERFUL_ASK_PROVIDER_2", "openrouter");
        let mut config = Config::default();
        config.ask.providers = ProvidersConfig {
            priority: vec![ProviderEntry {
                backend: Provider::Local,
                model: None,
                timeout_secs: None,
                api_key_env: None,
                base_url: None,
            }],
        };

        let entries = resolve_provider_entries(&config, None).unwrap();
        assert!(entries.iter().all(|e| e.backend == Provider::Local));
        assert!(!entries.is_empty());
    }

    #[test]
    #[serial_test::serial(env)]
    fn resolve_provider_priority_env_var_invalid_fails_fast() {
        clear_provider_env();
        let key = "LEDGERFUL_ASK_PROVIDER_1";
        // Legitimate: test-only env mutation (edition-2024 set_var is unsafe).
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe {
            std::env::set_var(key, "typo_cloud");
        }
        let config = Config::default();
        let result = resolve_provider_priority(&config, None);
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        unsafe {
            std::env::remove_var(key);
        }
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Invalid provider"));
        assert!(err.contains("typo_cloud"));
    }

    #[test]
    fn sanitize_error_for_logging_strips_bearer_tokens() {
        let input = "Error: bearer sk-abc123xyz failed";
        let sanitized = sanitize_error_for_logging(input);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("sk-abc123xyz"));
    }

    #[test]
    fn sanitize_error_for_logging_strips_api_key_values() {
        let input = "Failed at https://example.com?api_key=secret123";
        let sanitized = sanitize_error_for_logging(input);
        assert!(sanitized.contains("[REDACTED]"));
        assert!(!sanitized.contains("secret123"));
    }

    #[test]
    fn sanitize_error_for_logging_preserves_safe_strings() {
        let input = "Connection refused at localhost:8081";
        let sanitized = sanitize_error_for_logging(input);
        assert_eq!(sanitized, input);
    }

    mod gemini_model {
        use crate::config::model::GeminiConfig;
        use crate::gemini::modes::GeminiMode;

        fn clear_gemini_model_env() {
            // Legitimate: test-only env cleanup (edition-2024 remove_var is unsafe).
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe {
                std::env::remove_var("GEMINI_FAST_MODEL");
                std::env::remove_var("GEMINI_DEEP_MODEL");
            }
        }

        #[test]
        #[serial_test::serial(env)]
        fn test_select_gemini_model_logic() {
            let packet = crate::impact::packet::ImpactPacket::default();

            // 1. Defaults
            clear_gemini_model_env();
            let config = GeminiConfig {
                fast_model: Some("fast".to_string()),
                deep_model: Some("deep".to_string()),
                ..GeminiConfig::default()
            };
            let fast_model =
                crate::gemini::wrapper::select_gemini_model(&config, GeminiMode::Suggest, &packet);
            assert_eq!(fast_model, "fast");

            let deep_model = crate::gemini::wrapper::select_gemini_model(
                &config,
                GeminiMode::ReviewPatch,
                &packet,
            );
            assert_eq!(deep_model, "deep");

            // 2. Config Overrides
            let config_custom = GeminiConfig {
                model: Some("custom".to_string()),
                ..GeminiConfig::default()
            };
            let model = crate::gemini::wrapper::select_gemini_model(
                &config_custom,
                GeminiMode::Suggest,
                &packet,
            );
            assert_eq!(model, "custom");

            // 3. Env Overrides
            // Legitimate: test-only env mutation (edition-2024 set_var is unsafe).
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe {
                std::env::set_var("GEMINI_FAST_MODEL", "env-fast");
                std::env::set_var("GEMINI_DEEP_MODEL", "env-deep");
            }
            let config_empty = GeminiConfig::default();
            let fast_model_env = crate::gemini::wrapper::select_gemini_model(
                &config_empty,
                GeminiMode::Suggest,
                &packet,
            );
            assert_eq!(fast_model_env, "env-fast");

            let deep_model_env = crate::gemini::wrapper::select_gemini_model(
                &config_empty,
                GeminiMode::ReviewPatch,
                &packet,
            );
            assert_eq!(deep_model_env, "env-deep");

            clear_gemini_model_env();
        }
    }

    mod context_window_budget {
        use crate::config::model::GeminiConfig;

        const MIN_CONTEXT_CHARS: usize = 32_768;

        fn char_limit(config: &GeminiConfig) -> usize {
            (config.context_window as u64 * 4 * 80 / 100).max(MIN_CONTEXT_CHARS as u64) as usize
        }

        #[test]
        fn test_default_context_window_yields_hardcoded_budget() {
            let config = GeminiConfig::default(); // defaults to 128,000
            assert_eq!(char_limit(&config), 409_600);
        }

        #[test]
        fn test_custom_context_window_adjusts_budget() {
            let config = GeminiConfig {
                context_window: 200_000,
                ..Default::default()
            };
            assert_eq!(char_limit(&config), 640_000);
        }

        #[test]
        fn test_small_context_window_budget() {
            let config = GeminiConfig {
                context_window: 32_000,
                ..Default::default()
            };
            assert_eq!(char_limit(&config), 102_400);
        }

        #[test]
        fn test_zero_context_window_fallback() {
            let config = GeminiConfig {
                context_window: 0,
                ..Default::default()
            };
            assert_eq!(char_limit(&config), 32_768);
        }
    }
}
