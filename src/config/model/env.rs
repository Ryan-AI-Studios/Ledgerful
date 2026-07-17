use super::bridge::BridgeConfig;
use super::local_model::LocalModelConfig;

pub(crate) fn resolve_bridge_config(config: &BridgeConfig) -> BridgeConfig {
    resolve_bridge_config_with(config, &|name| std::env::var(name).ok())
}

pub(crate) fn resolve_bridge_config_with(
    config: &BridgeConfig,
    env_reader: &dyn Fn(&str) -> Option<String>,
) -> BridgeConfig {
    let mut resolved = config.clone();

    if let Some(val) = env_reader("LEDGERFUL_BRIDGE") {
        let normalized = val.trim().to_lowercase();
        if normalized == "1" || normalized == "true" {
            resolved.enabled = true;
        }
    }

    resolved
}

pub(crate) fn resolve_local_model_config(config: &LocalModelConfig) -> LocalModelConfig {
    resolve_local_model_config_with(config, &|name| std::env::var(name).ok(), &|name| {
        read_env_key(name)
    })
}

pub(crate) fn resolve_local_model_config_with(
    config: &LocalModelConfig,
    env_reader: &dyn Fn(&str) -> Option<String>,
    dotenv_reader: &dyn Fn(&str) -> Option<String>,
) -> LocalModelConfig {
    let mut resolved = config.clone();

    let resolve_string = |configured: &str, env_var: &str| -> String {
        if !configured.is_empty() {
            return configured.to_string();
        }
        if let Some(val) = env_reader(env_var)
            && !val.trim().is_empty()
        {
            return val.trim().to_string();
        }
        if let Some(val) = dotenv_reader(env_var) {
            return val;
        }
        String::new()
    };

    let resolve_optional_string = |configured: &Option<String>, env_var: &str| -> Option<String> {
        if configured
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return configured.clone();
        }
        env_reader(env_var)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .or_else(|| dotenv_reader(env_var))
            .filter(|value| !value.trim().is_empty())
    };

    let resolve_usize = |configured: usize, env_var: &str| -> usize {
        if configured != 0 {
            return configured;
        }
        if let Some(val) = env_reader(env_var)
            && let Ok(parsed) = val.trim().parse::<usize>()
        {
            return parsed;
        }
        if let Some(val) = dotenv_reader(env_var)
            && let Ok(parsed) = val.parse::<usize>()
        {
            return parsed;
        }
        0
    };

    let resolve_bool = |configured: bool, env_var: &str| -> bool {
        if configured {
            return true;
        }
        if let Some(val) = env_reader(env_var)
            && let Ok(parsed) = val.trim().to_lowercase().parse::<bool>()
        {
            return parsed;
        }
        if let Some(val) = dotenv_reader(env_var)
            && let Ok(parsed) = val.to_lowercase().parse::<bool>()
        {
            return parsed;
        }
        false
    };

    resolved.base_url = resolve_string(&config.base_url, "LEDGERFUL_LOCAL_MODEL_URL");
    resolved.embedding_url = Some(resolve_string(
        config.embedding_url.as_deref().unwrap_or(""),
        "LEDGERFUL_LOCAL_EMBEDDING_URL",
    ))
    .filter(|s| !s.is_empty());

    if resolved.embedding_url.is_none()
        && (resolved.base_url == "http://127.0.0.1:8081"
            || resolved.base_url == "http://localhost:8081")
    {
        resolved.embedding_url = Some("http://127.0.0.1:8083".to_string());
    }

    resolved.generation_url = Some(resolve_string(
        config.generation_url.as_deref().unwrap_or(""),
        "LEDGERFUL_LOCAL_GENERATION_URL",
    ))
    .filter(|s| !s.is_empty());
    resolved.ollama_cloud_url =
        resolve_optional_string(&config.ollama_cloud_url, "OLLAMA_CLOUD_URL");
    // Try OLLAMA_CLOUD_API_KEY first, then OLLAMA_API_KEY as fallback
    resolved.ollama_cloud_api_key =
        resolve_optional_string(&config.ollama_cloud_api_key, "OLLAMA_CLOUD_API_KEY")
            .or_else(|| resolve_optional_string(&config.ollama_cloud_api_key, "OLLAMA_API_KEY"));
    resolved.ollama_cloud_model =
        resolve_optional_string(&config.ollama_cloud_model, "OLLAMA_CLOUD_MODEL");

    resolved.embedding_model = resolve_string(&config.embedding_model, "LEDGERFUL_EMBEDDING_MODEL");
    resolved.generation_model =
        resolve_string(&config.generation_model, "LEDGERFUL_GENERATION_MODEL");
    resolved.rerank_model = resolve_string(&config.rerank_model, "LEDGERFUL_RERANK_MODEL");
    resolved.dimensions = resolve_usize(config.dimensions, "LEDGERFUL_EMBEDDING_DIMENSIONS");
    resolved.disable_hnsw = resolve_bool(config.disable_hnsw, "LEDGERFUL_DISABLE_HNSW");
    resolved.context_window =
        resolve_usize(config.context_window, "LEDGERFUL_LOCAL_CONTEXT_WINDOW");

    resolved
}

pub(crate) fn read_env_key(target_key: &str) -> Option<String> {
    use std::path::Path;
    let path = Path::new(".env");
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim().strip_prefix("export ").unwrap_or(key.trim());
        if key != target_key {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scoped env-var guard: sets `key` to `value` for the duration of `f`,
    /// then restores the previous value (or removes it if absent).
    /// Use this in threaded test runs where env mutation is unsafe.
    struct EnvGuard;

    impl EnvGuard {
        fn with<F, R>(key: &str, value: &str, f: F) -> R
        where
            F: FnOnce() -> R,
        {
            let previous = std::env::var(key).ok();
            // SAFETY: only used in tests under `cargo nextest` (one process per test)
            // or under `cargo test` when tests are run sequentially in the same process.
            // The guard restores the original value immediately after the closure.
            unsafe { std::env::set_var(key, value) };
            let result = f();
            match previous {
                Some(prev) => unsafe { std::env::set_var(key, prev) },
                None => unsafe { std::env::remove_var(key) },
            }
            result
        }
    }

    #[test]
    fn test_resolve_bridge_config_env_enabled() {
        let env_values: std::collections::HashMap<&str, &str> =
            vec![("LEDGERFUL_BRIDGE", "1")].into_iter().collect();
        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());

        let raw = BridgeConfig::default();
        let resolved = resolve_bridge_config_with(&raw, &env_reader);

        assert!(resolved.enabled);
    }

    #[test]
    fn test_resolve_bridge_config_env_true() {
        let env_values: std::collections::HashMap<&str, &str> =
            vec![("LEDGERFUL_BRIDGE", "true")].into_iter().collect();
        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());

        let raw = BridgeConfig::default();
        let resolved = resolve_bridge_config_with(&raw, &env_reader);

        assert!(resolved.enabled);
    }

    #[test]
    fn test_resolve_bridge_config_env_unset_stays_false() {
        let env_values: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());

        let raw = BridgeConfig::default();
        let resolved = resolve_bridge_config_with(&raw, &env_reader);

        assert!(!resolved.enabled);
    }

    #[test]
    fn test_resolve_local_model_config_env_override() {
        let env_values: std::collections::HashMap<&str, &str> = vec![
            ("LEDGERFUL_EMBEDDING_MODEL", "test-model-env"),
            ("LEDGERFUL_EMBEDDING_DIMENSIONS", "384"),
        ]
        .into_iter()
        .collect();

        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());
        let dotenv_reader = |_: &str| None::<String>;

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(resolved.embedding_model, "test-model-env");
        assert_eq!(resolved.dimensions, 384);
        assert_eq!(resolved.base_url, "");
    }

    #[test]
    fn test_resolve_local_model_config_toml_takes_priority() {
        let env_values: std::collections::HashMap<&str, &str> = vec![
            ("LEDGERFUL_EMBEDDING_MODEL", "env-model"),
            ("LEDGERFUL_LOCAL_MODEL_URL", "http://env:1234"),
        ]
        .into_iter()
        .collect();

        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());
        let dotenv_reader = |_: &str| None::<String>;

        let raw = LocalModelConfig {
            base_url: "http://config:9999".to_string(),
            embedding_url: None,
            generation_url: None,
            ollama_cloud_url: None,
            ollama_cloud_api_key: None,
            ollama_cloud_model: None,
            embedding_model: "config-model".to_string(),
            generation_model: "".to_string(),
            rerank_model: "".to_string(),
            dimensions: 0,
            context_window: 38000,
            timeout_secs: 60,
            prefer_local: false,
            chunk_top_k: 10,
            chunk_min_similarity: 0.3,
            chunk_dedup_threshold: 0.95,
            disable_hnsw: false,
            concurrency: None,
        };
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(resolved.base_url, "http://config:9999");
        assert_eq!(resolved.embedding_model, "config-model");
    }

    #[test]
    fn test_resolve_local_model_config_generation_model_env() {
        let env_values: std::collections::HashMap<&str, &str> = vec![
            ("LEDGERFUL_GENERATION_MODEL", "qwen3-9b"),
            ("LEDGERFUL_RERANK_MODEL", "bge-reranker"),
        ]
        .into_iter()
        .collect();

        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());
        let dotenv_reader = |_: &str| None::<String>;

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(resolved.generation_model, "qwen3-9b");
        assert_eq!(resolved.rerank_model, "bge-reranker");
    }

    #[test]
    fn test_resolve_local_model_config_ollama_cloud_dotenv() {
        let env_reader = |_: &str| None::<String>;
        let dotenv_values: std::collections::HashMap<&str, &str> = vec![
            ("OLLAMA_CLOUD_URL", "https://api.ollama.com"),
            ("OLLAMA_CLOUD_API_KEY", "cloud-token"),
            ("OLLAMA_CLOUD_MODEL", "minimax-m3:cloud"),
        ]
        .into_iter()
        .collect();
        let dotenv_reader = |name: &str| dotenv_values.get(name).map(|v| v.to_string());

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(
            resolved.ollama_cloud_url.as_deref(),
            Some("https://api.ollama.com")
        );
        assert_eq!(
            resolved.ollama_cloud_api_key.as_deref(),
            Some("cloud-token")
        );
        assert_eq!(
            resolved.ollama_cloud_model.as_deref(),
            Some("minimax-m3:cloud")
        );
    }

    #[test]
    fn test_resolve_local_model_config_dimensions_zero_unchanged() {
        let env_values: std::collections::HashMap<&str, &str> =
            vec![("LEDGERFUL_EMBEDDING_DIMENSIONS", "0")]
                .into_iter()
                .collect();

        let env_reader = |name: &str| env_values.get(name).map(|v| v.to_string());
        let dotenv_reader = |_: &str| None::<String>;

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(resolved.dimensions, 0);
    }

    #[test]
    fn test_resolve_local_model_config_dotenv_override() {
        let env_reader = |_: &str| None::<String>;
        let dotenv_values: std::collections::HashMap<&str, &str> = vec![
            ("LEDGERFUL_EMBEDDING_MODEL", "dotenv-model"),
            ("LEDGERFUL_LOCAL_MODEL_URL", "http://dotenv:5678"),
        ]
        .into_iter()
        .collect();
        let dotenv_reader = |name: &str| dotenv_values.get(name).map(|v| v.to_string());

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(resolved.embedding_model, "dotenv-model");
        assert_eq!(resolved.base_url, "http://dotenv:5678");
    }

    #[test]
    fn test_ollama_cloud_api_key_env_precedence_with_guard() {
        // OLLAMA_CLOUD_API_KEY should be read directly from the environment.
        // We use the scoped guard because `cargo test config::model` may run
        // tests in the same process under `cargo test` (not nextest).
        let result = EnvGuard::with("OLLAMA_CLOUD_API_KEY", "direct-api-key", || {
            let raw = LocalModelConfig::default();
            let resolved = resolve_local_model_config(&raw);
            resolved.ollama_cloud_api_key.clone()
        });
        assert_eq!(result, Some("direct-api-key".to_string()));
    }

    #[test]
    fn test_ollama_api_key_fallback() {
        // When OLLAMA_CLOUD_API_KEY is absent from both env and .env,
        // OLLAMA_API_KEY should be used as fallback.
        let env_reader = |name: &str| {
            if name == "OLLAMA_API_KEY" {
                Some("fallback-key".to_string())
            } else {
                None::<String>
            }
        };
        let dotenv_reader = |_: &str| None::<String>;

        let raw = LocalModelConfig::default();
        let resolved = resolve_local_model_config_with(&raw, &env_reader, &dotenv_reader);

        assert_eq!(
            resolved.ollama_cloud_api_key,
            Some("fallback-key".to_string())
        );
    }
}
