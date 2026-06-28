use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ImpactPacket, RuntimeUsageDelta};
use crate::index::runtime_usage::extract_runtime_usage;
use miette::Result;
use std::process::Command;

pub struct RuntimeUsageProvider;

impl EnrichmentProvider for RuntimeUsageProvider {
    fn name(&self) -> &'static str {
        "Runtime Usage Enrichment Provider"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let mut deltas = Vec::new();

        for change in &packet.changes {
            let current_env_vars = change
                .runtime_usage
                .as_ref()
                .map(|u| u.env_vars.len())
                .unwrap_or(0);
            let current_env_var_names: Vec<String> = change
                .runtime_usage
                .as_ref()
                .map(|u| u.env_vars.clone())
                .unwrap_or_default();
            let current_config_keys = change
                .runtime_usage
                .as_ref()
                .map(|u| u.config_keys.len())
                .unwrap_or(0);

            let path_str = change.path.to_string_lossy().replace('\\', "/");

            let output = Command::new("git")
                .args(["show", &format!("HEAD:{}", path_str)])
                .current_dir(&context.project_root)
                .output();

            let mut previous_env_vars = 0;
            let mut previous_env_var_names: Vec<String> = Vec::new();
            let mut previous_config_keys = 0;

            if let Some(output) = output.ok().filter(|o| o.status.success()) {
                let prev_content = String::from_utf8_lossy(&output.stdout);
                if let Some(prev_usage) = extract_runtime_usage(&change.path, &prev_content) {
                    previous_env_vars = prev_usage.env_vars.len();
                    previous_env_var_names = prev_usage.env_vars;
                    previous_config_keys = prev_usage.config_keys.len();
                }
            }

            let identity_changed = {
                let same_len = current_env_var_names.len() == previous_env_var_names.len();
                !same_len || current_env_var_names != previous_env_var_names
            };

            if current_env_vars != previous_env_vars
                || current_config_keys != previous_config_keys
                || identity_changed
            {
                deltas.push(RuntimeUsageDelta {
                    file_path: change.path.to_string_lossy().to_string(),
                    env_vars_previous_count: previous_env_vars,
                    env_vars_current_count: current_env_vars,
                    config_keys_previous_count: previous_config_keys,
                    config_keys_current_count: current_config_keys,
                    env_vars_previous: previous_env_var_names,
                    env_vars_current: current_env_var_names,
                });
            }
        }

        packet.runtime_usage_delta = deltas;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus};
    use crate::state::migrations::get_migrations;
    use crate::state::storage::StorageManager;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    #[test]
    fn enrich_no_runtime_usage_when_no_changes() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from(r"C:\dev\ledgerful"),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket::default();

        RuntimeUsageProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.runtime_usage_delta.is_empty());
    }

    #[test]
    fn enrich_no_delta_when_counts_unchanged() {
        let mut conn = Connection::open_in_memory().unwrap();
        get_migrations().to_latest(&mut conn).unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = crate::config::model::Config::default();
        let context = EnrichmentContext {
            storage: &storage,
            config: &config,
            file_id_map: HashMap::new(),
            project_root: PathBuf::from(r"C:\dev\ledgerful"),
            warnings: Arc::new(Mutex::new(Vec::new())),
        };
        let mut packet = ImpactPacket {
            changes: vec![ChangedFile {
                path: PathBuf::from("nonexistent.rs"),
                status: "Modified".to_string(),
                old_path: None,
                is_staged: false,
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: FileAnalysisStatus::default(),
                analysis_warnings: Vec::new(),
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }],
            ..Default::default()
        };

        RuntimeUsageProvider.enrich(&context, &mut packet).unwrap();

        assert!(packet.runtime_usage_delta.is_empty());
    }
}
