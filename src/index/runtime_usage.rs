use crate::index::env_patterns::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeUsage {
    pub env_vars: Vec<String>,
    pub config_keys: Vec<String>,
}

pub fn extract_runtime_usage(path: &Path, content: &str) -> Option<RuntimeUsage> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    let mut env_vars = Vec::new();

    match extension {
        "rs" => {
            collect_captures(&RUST_ENV_VAR, content, &mut env_vars);
            collect_captures(&RUST_ENV_VAR_OS, content, &mut env_vars);
            collect_captures(&RUST_ENV_MACRO, content, &mut env_vars);
            collect_captures(&RUST_OPTION_ENV, content, &mut env_vars);
            collect_captures(&RUST_ENV_VAR_DEFAULT, content, &mut env_vars);
            collect_captures(&RUST_SET_ENV, content, &mut env_vars);
        }
        "ts" | "tsx" | "js" | "jsx" => {
            collect_captures(&TS_ENV_DOT, content, &mut env_vars);
            collect_captures(&TS_ENV_INDEXED, content, &mut env_vars);
            collect_captures(&TS_IMPORT_META_ENV, content, &mut env_vars);
            collect_captures(&TS_ENV_DESTRUCTURING, content, &mut env_vars);
            collect_captures(&TS_ENV_DEFAULT, content, &mut env_vars);
            collect_captures(&TS_SET_ENV, content, &mut env_vars);
        }
        "py" => {
            collect_captures(&PY_ENV_GET, content, &mut env_vars);
            collect_captures(&PY_ENVIRON_GET, content, &mut env_vars);
            collect_captures(&PY_ENV_INDEXED, content, &mut env_vars);
            collect_captures(&PY_ENV_GET_DEFAULT, content, &mut env_vars);
        }
        _ => return None,
    }

    let mut config_keys = Vec::new();
    for hint in CONFIG_HINTS.iter() {
        if hint.is_match(content) {
            config_keys.push(hint.as_str().trim_matches('\\').to_string());
        }
    }

    env_vars.sort_unstable();
    env_vars.dedup();
    config_keys.sort_unstable();
    config_keys.dedup();

    if env_vars.is_empty() && config_keys.is_empty() {
        None
    } else {
        Some(RuntimeUsage {
            env_vars,
            config_keys,
        })
    }
}

fn collect_captures(regex: &Regex, content: &str, out: &mut Vec<String>) {
    for capture in regex.captures_iter(content) {
        if let Some(m) = capture.get(1) {
            out.push(m.as_str().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_runtime_usage_rust() {
        let content = r#"
let _ = std::env::var("DATABASE_URL");
let _ = env!("API_TOKEN");
"#;
        let usage = extract_runtime_usage(Path::new("src/main.rs"), content).unwrap();
        assert!(usage.env_vars.contains(&"DATABASE_URL".to_string()));
        assert!(usage.env_vars.contains(&"API_TOKEN".to_string()));
    }

    #[test]
    fn test_extract_runtime_usage_typescript() {
        let content = r#"
dotenv.config();
const token = process.env.API_TOKEN;
const mode = process.env["NODE_ENV"];
"#;
        let usage = extract_runtime_usage(Path::new("src/app.ts"), content).unwrap();
        assert!(usage.env_vars.contains(&"API_TOKEN".to_string()));
        assert!(usage.env_vars.contains(&"NODE_ENV".to_string()));
        assert!(usage.config_keys.iter().any(|key| key.contains("dotenv")));
    }

    #[test]
    fn test_extract_runtime_usage_python() {
        let content = r#"
import os
value = os.getenv("DATABASE_URL")
mode = os.environ["APP_MODE"]
"#;
        let usage = extract_runtime_usage(Path::new("app.py"), content).unwrap();
        assert!(usage.env_vars.contains(&"DATABASE_URL".to_string()));
        assert!(usage.env_vars.contains(&"APP_MODE".to_string()));
    }
}
