use serde_json::Value;

/// Field name patterns that indicate a secret value (lowercase comparison).
const SECRET_FIELD_PATTERNS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "token",
    "secret",
    "password",
    "credential",
    "ollama_key",
    "ollama_cloud_api_key",
    "gemini_api_key",
    "database_url",
    "connection_string",
    "dsn",
    "redis_url",
    "broker_url",
    "smtp_url",
];

/// Check whether a field name looks like it holds a secret value.
pub(crate) fn is_secret_field_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    SECRET_FIELD_PATTERNS
        .iter()
        .any(|&p| lower == p || lower.ends_with(p))
}

const CREDENTIAL_URL_SCHEMES: &[&str] = &[
    "postgres",
    "postgresql",
    "mysql",
    "mariadb",
    "mongodb",
    "mongodb+srv",
    "redis",
    "rediss",
    "amqp",
    "amqps",
    "smtp",
    "http",
    "https",
];

/// Detect credentials only in recognized, structured URL values.
///
/// The input value is intentionally never included in errors or diagnostics.
pub(crate) fn contains_structured_url_secret(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    if !CREDENTIAL_URL_SCHEMES.contains(&parsed.scheme()) {
        return false;
    }
    if parsed
        .password()
        .is_some_and(|password| !password.is_empty())
    {
        return true;
    }
    parsed
        .query_pairs()
        .any(|(key, _)| is_secret_field_name(key.as_ref()))
}

/// Recursively walk a `serde_json::Value` and replace any field whose name
/// matches a known secret pattern with the string `"[REDACTED]"`.
///
/// Operates on both human and JSON config display paths so that secret values
/// are never leaked through `config view`, `config verify`, or any other
/// config serialization surface.
pub fn redact_config_value(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            // Recurse into children first so nested objects are also redacted.
            for val in obj.values_mut() {
                redact_config_value(val);
            }
            // Then redact secret-named fields at this level.
            let secret_keys: Vec<String> = obj
                .keys()
                .filter(|k| is_secret_field_name(k))
                .cloned()
                .collect();
            for key in secret_keys {
                if let Some(inner) = obj.get(&key) {
                    let display = if inner.is_string() && !inner.as_str().unwrap_or("").is_empty() {
                        "[REDACTED]"
                    } else if inner.is_null() || inner.as_str().is_some_and(|s| s.is_empty()) {
                        "(not set)"
                    } else {
                        "[REDACTED]"
                    };
                    obj.insert(key, Value::String(display.to_string()));
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_config_value(item);
            }
        }
        Value::String(string) if contains_structured_url_secret(string) => {
            *string = "[REDACTED]".to_string();
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) fn structured_url_test_cases() -> Vec<(String, bool)> {
    let mut cases: Vec<(String, bool)> = CREDENTIAL_URL_SCHEMES
        .iter()
        .map(|scheme| (format!("{scheme}://user:TA33-SENTINEL@host/db"), true))
        .collect();
    cases.extend([
        ("HTTPS://user:p%40ss@host/path".to_string(), true),
        ("https://host/path?api%5Fkey=value".to_string(), true),
        ("https://user:@host/path".to_string(), false),
        ("https://user@host/path".to_string(), false),
        ("https://host/path?ordinary=value".to_string(), false),
        ("ftp://user:TA33-SENTINEL@host/file".to_string(), false),
        ("https://host/%ZZ".to_string(), false),
        ("not a url %ZZ TA33-SENTINEL".to_string(), false),
    ]);
    cases
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_redact_api_key() {
        let mut val = json!({"ollama_cloud_api_key": "sk-abc123"});
        redact_config_value(&mut val);
        assert_eq!(val["ollama_cloud_api_key"], "[REDACTED]");
    }

    #[test]
    fn test_redact_gemini_key() {
        let mut val = json!({"gemini_api_key": "AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q"});
        redact_config_value(&mut val);
        assert_eq!(val["gemini_api_key"], "[REDACTED]");
    }

    #[test]
    fn test_redact_ollama_key() {
        let mut val = json!({"ollama_key": "some-key-value"});
        redact_config_value(&mut val);
        assert_eq!(val["ollama_key"], "[REDACTED]");
    }

    #[test]
    fn test_redact_nested_secret() {
        let mut val = json!({
            "local_model": {
                "base_url": "http://localhost:11434",
                "ollama_cloud_api_key": "sk-secret",
                "ollama_cloud_model": "minimax-m3:cloud"
            }
        });
        redact_config_value(&mut val);
        assert_eq!(val["local_model"]["ollama_cloud_api_key"], "[REDACTED]");
        assert_eq!(val["local_model"]["base_url"], "http://localhost:11434");
        assert_eq!(val["local_model"]["ollama_cloud_model"], "minimax-m3:cloud");
    }

    #[test]
    fn structured_url_policy_table_covers_all_schemes_encoding_and_false_positives() {
        for (url, expected) in structured_url_test_cases() {
            assert_eq!(contains_structured_url_secret(&url), expected, "{url}");
            let mut display = serde_json::json!({"nested": [[url.clone()]]});
            redact_config_value(&mut display);
            assert_eq!(display["nested"][0][0] == "[REDACTED]", expected, "{url}");
        }
    }

    #[test]
    fn test_redact_token_field() {
        let mut val = json!({"access_token": "ghp_abc123"});
        redact_config_value(&mut val);
        assert_eq!(val["access_token"], "[REDACTED]");
    }

    #[test]
    fn test_redact_secret_field() {
        let mut val = json!({"client_secret": "s3cr3t"});
        redact_config_value(&mut val);
        assert_eq!(val["client_secret"], "[REDACTED]");
    }

    #[test]
    fn test_redact_not_set_shows_placeholder() {
        let mut val = json!({"ollama_cloud_api_key": null});
        redact_config_value(&mut val);
        assert_eq!(val["ollama_cloud_api_key"], "(not set)");
    }

    #[test]
    fn test_redact_empty_shows_placeholder() {
        let mut val = json!({"ollama_cloud_api_key": ""});
        redact_config_value(&mut val);
        assert_eq!(val["ollama_cloud_api_key"], "(not set)");
    }

    #[test]
    fn test_no_redact_on_normal_fields() {
        let mut val = json!({
            "base_url": "http://localhost:11434",
            "embedding_model": "nomic-embed-text",
            "generation_model": "minimax-m3:cloud",
            "timeout_secs": 60
        });
        redact_config_value(&mut val);
        assert_eq!(val["base_url"], "http://localhost:11434");
        assert_eq!(val["embedding_model"], "nomic-embed-text");
    }

    #[test]
    fn test_redact_array_with_secret() {
        let mut val = json!([
            {"name": "config1", "api_key": "abc123"},
            {"name": "config2", "token": "def456"}
        ]);
        redact_config_value(&mut val);
        assert_eq!(val[0]["api_key"], "[REDACTED]");
        assert_eq!(val[1]["token"], "[REDACTED]");
        assert_eq!(val[0]["name"], "config1");
    }

    #[test]
    fn test_sentinel_secret_never_appears() {
        let sentinel = "TEST-SENTINEL-SECRET-VALUE-NEVER-LEAKS";
        let mut val = json!({
            "local_model": {
                "ollama_cloud_api_key": sentinel,
                "ollama_key": sentinel,
            },
            "gemini": {
                "api_key": sentinel,
            }
        });
        redact_config_value(&mut val);
        let serialized = serde_json::to_string(&val).unwrap();
        assert!(
            !serialized.contains(sentinel),
            "Sentinel secret leaked in: {serialized}"
        );
        assert!(serialized.contains("[REDACTED]"));
    }

    #[test]
    fn test_secret_in_map_values() {
        let mut val = json!({
            "providers": {
                "openai": {"api_key": "sk-openai-key"},
                "anthropic": {"api_key": "sk-anthropic-key"}
            }
        });
        redact_config_value(&mut val);
        assert_eq!(val["providers"]["openai"]["api_key"], "[REDACTED]");
        assert_eq!(val["providers"]["anthropic"]["api_key"], "[REDACTED]");
    }

    #[test]
    fn secret_field_classifier_connection_bearing_aliases_classified() {
        for key in [
            "database_url",
            "DATABASE_URL",
            "connection_string",
            "primary_dsn",
            "redis_url",
            "broker_url",
        ] {
            assert!(is_secret_field_name(key), "{key} must be secret-bearing");
        }
    }

    #[test]
    fn secret_field_classifier_ordinary_fields_not_classified() {
        for key in [
            "base_url",
            "documentation_url",
            "username",
            "model",
            "timeout_secs",
        ] {
            assert!(
                !is_secret_field_name(key),
                "{key} must not be classified as secret-bearing"
            );
        }
    }

    #[test]
    fn redact_config_value_structured_credential_url_is_redacted() {
        let mut val = json!({
            "endpoint": "postgres://user:password@example.com/db",
            "documentation_url": "https://example.com/docs"
        });

        redact_config_value(&mut val);

        assert_eq!(val["endpoint"], "[REDACTED]");
        assert_eq!(val["documentation_url"], "https://example.com/docs");
    }
}
