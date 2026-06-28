use crate::impact::packet::{ChangedFile, TraceConfigChange, TraceConfigType, TraceEnvVarChange};
use crate::index::env_schema::EnvVarDep;
use globset::{Glob, GlobSetBuilder};
#[cfg(test)]
use std::path::PathBuf;
use tracing::warn;

pub fn detect_trace_config_changes(
    changed_files: &[ChangedFile],
    patterns: &[String],
) -> Vec<TraceConfigChange> {
    let mut builder = GlobSetBuilder::new();

    for pat in patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                warn!("Invalid trace config glob pattern '{}': {}", pat, e);
            }
        }
    }

    let glob_set = match builder.build() {
        Ok(set) => set,
        Err(e) => {
            warn!("Failed to build trace config glob set: {}", e);
            return Vec::new();
        }
    };

    let mut changes = Vec::new();
    for file in changed_files {
        if glob_set.is_match(&file.path) {
            let config_type = TraceConfigType::from_path(&file.path);
            changes.push(TraceConfigChange {
                file: file.path.clone(),
                config_type,
                risk_weight: 5, // Default risk weight for trace config changes
                is_deleted: file.status == "Deleted",
            });
        }
    }

    changes
}

pub fn detect_trace_env_vars(
    env_deps: &[EnvVarDep],
    patterns: &[String],
    exclude_patterns: &[String],
) -> Vec<TraceEnvVarChange> {
    let mut pattern_builder = GlobSetBuilder::new();
    let mut compiled_patterns = Vec::new();

    for pat in patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                pattern_builder.add(glob);
                compiled_patterns.push((pat.clone(), Glob::new(pat).unwrap().compile_matcher()));
            }
            Err(e) => {
                warn!("Invalid trace env-var glob pattern '{}': {}", pat, e);
            }
        }
    }

    let mut exclude_builder = GlobSetBuilder::new();
    for pat in exclude_patterns {
        match Glob::new(pat) {
            Ok(glob) => {
                exclude_builder.add(glob);
            }
            Err(e) => {
                warn!("Invalid trace env-var exclude pattern '{}': {}", pat, e);
            }
        }
    }

    let exclude_set = match exclude_builder.build() {
        Ok(set) => set,
        Err(_) => GlobSetBuilder::new().build().unwrap(),
    };

    let mut changes = Vec::new();
    for dep in env_deps {
        if exclude_set.is_match(&dep.var_name) {
            continue;
        }

        for (pat_str, matcher) in &compiled_patterns {
            if matcher.is_match(&dep.var_name) {
                changes.push(TraceEnvVarChange {
                    var_name: dep.var_name.clone(),
                    pattern: pat_str.clone(),
                    risk_weight: 3, // Default risk weight for trace env var changes
                });
                break;
            }
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ChangedFile;
    use crate::index::env_schema::EnvVarDep;

    #[test]
    fn test_otel_collector_yaml_detected() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("config/otel-collector.yaml"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["**/otel*.yaml".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].config_type,
            TraceConfigType::OpenTelemetryCollector
        );
        assert!(!changes[0].is_deleted);
    }

    #[test]
    fn test_non_trace_yaml_skipped() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("config/app-config.yaml"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["**/otel*.yaml".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 0);
    }

    #[test]
    fn test_invalid_glob_pattern_does_not_abort() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("config/otel-collector.yaml"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["[invalid".to_string(), "**/otel*.yaml".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn test_double_extension_matched() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("config/otel-collector.yaml.tmpl"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["**/otel*.yaml*".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].config_type,
            TraceConfigType::OpenTelemetryCollector
        );
    }

    #[test]
    fn test_jaeger_yaml_detected() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("jaeger.yaml"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["**/jaeger*.yaml".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].config_type, TraceConfigType::JaegerAgent);
    }

    #[test]
    fn test_datadog_yaml_detected() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("datadog.yaml"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["**/datadog*.yaml".to_string()];
        let changes = detect_trace_config_changes(&changed_files, &patterns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].config_type, TraceConfigType::DataDogAgent);
    }

    #[test]
    fn test_otel_env_var_flagged() {
        let env_deps = vec![EnvVarDep {
            var_name: "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            declared: true,
            evidence: "std::env::var(\"OTEL_EXPORTER_OTLP_ENDPOINT\")".to_string(),
        }];
        let patterns = vec!["OTEL_*".to_string()];
        let changes = detect_trace_env_vars(&env_deps, &patterns, &[]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].var_name, "OTEL_EXPORTER_OTLP_ENDPOINT");
    }

    #[test]
    fn test_otel_sdk_disabled_excluded() {
        let env_deps = vec![EnvVarDep {
            var_name: "OTEL_SDK_DISABLED".to_string(),
            declared: true,
            evidence: "process.env.OTEL_SDK_DISABLED".to_string(),
        }];
        let patterns = vec!["OTEL_*".to_string()];
        let exclude = vec!["OTEL_SDK_DISABLED".to_string()];
        let changes = detect_trace_env_vars(&env_deps, &patterns, &exclude);
        assert_eq!(changes.len(), 0);
    }

    #[test]
    fn test_non_trace_env_var_skipped() {
        let env_deps = vec![EnvVarDep {
            var_name: "DATABASE_URL".to_string(),
            declared: true,
            evidence: "process.env.DATABASE_URL".to_string(),
        }];
        let patterns = vec!["OTEL_*".to_string()];
        let changes = detect_trace_env_vars(&env_deps, &patterns, &[]);
        assert_eq!(changes.len(), 0);
    }
}
