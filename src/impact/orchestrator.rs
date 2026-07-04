use crate::config::model::Config;
use crate::git::{ChangeType, RepoSnapshot};
use crate::impact::analysis::AnalysisRegistry;
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
use crate::index::analysis::{AnalysisOutcome, analyze_file};
use crate::state::storage::StorageManager;
use crate::util::clock::SystemClock;
use indicatif::{ProgressBar, ProgressStyle};
use miette::Result;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

pub struct ImpactOrchestrator {
    enrichment_providers: Vec<Box<dyn EnrichmentProvider>>,
    analysis_registry: AnalysisRegistry,
}

impl Default for ImpactOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl ImpactOrchestrator {
    /// Determine whether AI-driven enrichment is available. Returns `None` when
    /// the local model appears reachable (or is not configured at all, which is
    /// a legitimate non-AI mode). Returns a warning string when the model is
    /// configured but unreachable, so impact output can annotate that AI
    /// enrichment was skipped instead of silently degrading.
    fn ai_enrichment_status(config: &Config) -> Option<String> {
        let lm = &config.local_model;
        let url = lm
            .embedding_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(Some(lm.base_url.as_str()))?;
        if url.is_empty() {
            return None;
        }
        if !crate::util::network::is_url_reachable(url, Duration::from_millis(500)) {
            return Some(format!(
                "AI enrichment skipped: local model at {} is unreachable",
                url
            ));
        }
        None
    }

    pub fn new() -> Self {
        Self {
            enrichment_providers: Vec::new(),
            analysis_registry: AnalysisRegistry::default(),
        }
    }

    pub fn with_builtins() -> Self {
        let mut orch = Self::new();
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::federated::FederatedProvider,
        ));
        orch.register_enrichment_provider(Box::new(crate::impact::enrichment::api::ApiProvider));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::data_models::DataModelProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::contracts::ContractProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::ci_gates::CIGateProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::infrastructure::InfrastructureProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::environment::EnvironmentProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::observability::ObservabilityProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::coupling::CouplingProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::deploy::DeployProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::ci_self_awareness::CISelfAwarenessProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::ci_predictor::CIPredictorProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::hotspots::HotspotProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::coverage::CoverageProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::services::ServiceProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::runtime_usage::RuntimeUsageProvider,
        ));
        // RiskProvider is removed from enrichment and handled by analysis_registry.
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::dead_code::DeadCodeProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::kg_provider::KGProvider,
        ));
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::adr_provider::AdrProvider,
        ));
        orch
    }

    pub fn register_enrichment_provider(&mut self, provider: Box<dyn EnrichmentProvider>) {
        self.enrichment_providers.push(provider);
    }

    pub fn register_analysis_provider(
        &mut self,
        provider: Box<dyn crate::impact::analysis::ImpactProvider>,
    ) {
        self.analysis_registry.register(provider);
    }

    pub fn run(
        &self,
        packet: &mut ImpactPacket,
        storage: &StorageManager,
        config: &Config,
        project_root: &Path,
    ) -> Result<()> {
        debug!("Starting impact orchestration...");

        // Annotate AI enrichment availability up front so the output always shows
        // whether AI-driven enrichment (semantic / embedding / knowledge) was
        // skipped. This keeps the impact command deterministic and non-fatal when
        // the local model is down.
        if let Some(status) = Self::ai_enrichment_status(config) {
            packet.analysis_warnings.push(status);
        }

        // 1. Prepare Context
        let file_id_map = storage.get_active_file_id_map()?;
        let warnings_collector = Arc::new(Mutex::new(Vec::new()));

        // 0034: cooperative backstop deadline, computed once and threaded
        // through `EnrichmentContext` to every provider (notably
        // `FederatedProvider` → `refresh_federated_dependencies` → scanner)
        // so a multi-sibling federated run shares ONE global deadline instead
        // of each walk getting a fresh budget. Checked at provider boundaries
        // so a hung provider cannot stall the whole command. Non-fatal: on
        // breach we stop the loop and annotate which provider was running,
        // preserving partial results from already-completed providers. This
        // is NOT a thread-kill — synchronous providers already in-flight when
        // the deadline passes will still run to completion, but no further
        // providers are started. The subprocess and walk root causes are
        // bounded separately (scanner.rs:run_federate_export and
        // scan_dependency_dir), so this deadline is a backstop, not the
        // primary fix.
        let deadline = std::time::Instant::now() + config.federation.scan_timeout();

        let context = EnrichmentContext {
            storage,
            config,
            file_id_map,
            project_root: project_root.to_path_buf(),
            warnings: Arc::clone(&warnings_collector),
            deadline,
        };

        // 2. Execute Enrichment Providers (Resilient Execution)
        for provider in &self.enrichment_providers {
            let name = provider.name();
            if std::time::Instant::now() >= deadline {
                let msg = format!(
                    "Impact scan exceeded overall timeout ({}s); stopping before provider '{}'. Partial results retained.",
                    config.federation.scan_timeout_secs, name
                );
                warn!("{}", msg);
                context.add_warning(msg);
                break;
            }
            debug!("Running enrichment provider: {}", name);

            if let Err(e) = provider.enrich(&context, packet) {
                warn!("Enrichment provider '{}' failed: {}", name, e);
                context.add_warning(format!("Provider '{}' failed: {}", name, e));
            }
        }

        // 3. Execute Analysis (Scoring)
        let layout = crate::state::layout::Layout::new(project_root.to_string_lossy().as_ref());
        let rules = match crate::policy::load::load_rules(&layout) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to load policy rules: {}", e);
                context.add_warning(format!("Failed to load policy rules: {}", e));
                crate::policy::rules::Rules::default()
            }
        };
        self.analysis_registry.run(packet, &rules, config)?;

        // 4. Collect Warnings
        if let Ok(w) = warnings_collector.lock() {
            packet.analysis_warnings.extend(w.iter().cloned());
        }

        Ok(())
    }
}

pub(crate) fn map_snapshot_to_packet(
    snapshot: RepoSnapshot,
    base_dir: &Path,
) -> Result<ImpactPacket> {
    let mut packet = ImpactPacket {
        head_hash: snapshot.head_hash,
        branch_name: snapshot.branch_name,
        tree_clean: snapshot.is_clean,
        ..ImpactPacket::with_clock(&SystemClock)
    };

    let pb = ProgressBar::new(snapshot.changes.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );
    pb.set_message("Extracting symbols...");

    packet.changes = snapshot
        .changes
        .into_iter()
        .map(|c| {
            pb.set_message(format!("Extracting symbols from {}", c.path.display()));
            let (status, old_path) = match c.change_type {
                ChangeType::Added => ("Added".to_string(), None),
                ChangeType::Modified => ("Modified".to_string(), None),
                ChangeType::Deleted => ("Deleted".to_string(), Some(c.path.clone())),
                ChangeType::Renamed { ref old_path } => {
                    ("Renamed".to_string(), Some(old_path.clone()))
                }
            };

            let outcome = if matches!(c.change_type, ChangeType::Added | ChangeType::Modified) {
                analyze_file(&c.path, base_dir)
            } else {
                AnalysisOutcome {
                    symbols: None,
                    imports: None,
                    runtime_usage: None,
                    analysis_status: FileAnalysisStatus::default(),
                    analysis_warnings: Vec::new(),
                }
            };

            pb.inc(1);
            ChangedFile {
                path: c.path,
                status,
                old_path,
                is_staged: c.is_staged,
                symbols: outcome.symbols,
                imports: outcome.imports,
                runtime_usage: outcome.runtime_usage,
                analysis_status: outcome.analysis_status,
                analysis_warnings: outcome.analysis_warnings,
                api_routes: Vec::new(),
                data_models: Vec::new(),
                ci_gates: Vec::new(),
            }
        })
        .collect();

    pb.finish_with_message("Symbol extraction complete.");
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FileChange;

    fn make_deleted(path: &str) -> FileChange {
        FileChange {
            path: std::path::PathBuf::from(path),
            change_type: ChangeType::Deleted,
            is_staged: true,
        }
    }

    fn make_added(path: &str) -> FileChange {
        FileChange {
            path: std::path::PathBuf::from(path),
            change_type: ChangeType::Added,
            is_staged: true,
        }
    }

    #[test]
    fn test_deleted_file_old_path() {
        let snapshot = RepoSnapshot {
            head_hash: Some("abc123".to_string()),
            branch_name: Some("main".to_string()),
            is_clean: false,
            changes: vec![
                make_deleted("src/api/users/handler.rs"),
                make_added("src/api/users/new.rs"),
            ],
        };
        let temp = tempfile::tempdir().unwrap();
        let packet = map_snapshot_to_packet(snapshot, temp.path()).unwrap();
        let deleted = packet
            .changes
            .iter()
            .find(|c| c.status == "Deleted")
            .expect("Deleted file not found");
        assert_eq!(
            deleted
                .old_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            Some("src/api/users/handler.rs".to_string())
        );
        let added = packet
            .changes
            .iter()
            .find(|c| c.status == "Added")
            .expect("Added file not found");
        assert!(added.old_path.is_none());
    }

    #[test]
    fn test_ai_enrichment_status_unreachable_annotates() {
        let mut config = Config::default();
        config.local_model.base_url = "http://127.0.0.1:1".to_string();

        let status = ImpactOrchestrator::ai_enrichment_status(&config);
        assert!(status.is_some(), "expected warning for unreachable model");
        let msg = status.unwrap();
        assert!(
            msg.contains("AI enrichment skipped"),
            "expected skip annotation, got: {msg}"
        );
        assert!(
            msg.contains("127.0.0.1:1"),
            "expected URL in annotation, got: {msg}"
        );
    }

    #[test]
    fn test_ai_enrichment_status_empty_url_is_none() {
        let config = Config::default();
        assert!(
            ImpactOrchestrator::ai_enrichment_status(&config).is_none(),
            "empty base_url should not produce a skip annotation"
        );
    }

    #[test]
    fn test_orchestrator_annotates_ai_unavailable_and_continues() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = Config::default();
        // Point at an unreachable model so the AI-enrichment annotation fires.
        config.local_model.base_url = "http://127.0.0.1:1".to_string();
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket::default();

        let orchestrator = ImpactOrchestrator::with_builtins();
        orchestrator
            .run(&mut packet, &storage, &config, temp.path())
            .expect("orchestrator should not fail when model is unreachable");

        assert!(
            packet
                .analysis_warnings
                .iter()
                .any(|w| w.contains("AI enrichment skipped")),
            "expected AI enrichment skip annotation in {:?}",
            packet.analysis_warnings
        );
    }

    /// 0034: a breached backstop deadline annotates analysis_warnings with the
    /// provider that would have run next, and preserves partial results.
    #[test]
    fn test_backstop_deadline_annotates_and_preserves_partial_results() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let mut config = Config::default();
        // Set the backstop to zero so the deadline is already breached when
        // the enrichment loop starts — the first provider triggers the
        // annotation without running.
        config.federation.scan_timeout_secs = 0;
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket::default();

        let orchestrator = ImpactOrchestrator::with_builtins();
        orchestrator
            .run(&mut packet, &storage, &config, temp.path())
            .expect("orchestrator should not fail on a breached deadline");

        assert!(
            packet
                .analysis_warnings
                .iter()
                .any(|w| w.contains("Impact scan exceeded overall timeout")
                    && w.contains("Partial results retained")),
            "expected a backstop-timeout annotation in {:?}",
            packet.analysis_warnings
        );
    }

    #[test]
    fn test_no_info_logs_during_enrichment() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket::default();

        let orchestrator = ImpactOrchestrator::with_builtins();

        // 1. Set up tracing log capture subscriber
        struct SimpleLogCapture {
            logs: Arc<Mutex<Vec<(tracing::Level, String)>>>,
        }
        impl tracing::Subscriber for SimpleLogCapture {
            fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
                metadata.level() <= &tracing::Level::INFO
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, event: &tracing::Event<'_>) {
                let mut msg = String::new();
                struct Visitor<'a>(&'a mut String);
                impl<'a> tracing::field::Visit for Visitor<'a> {
                    fn record_debug(
                        &mut self,
                        field: &tracing::field::Field,
                        value: &dyn std::fmt::Debug,
                    ) {
                        if field.name() == "message" {
                            use std::fmt::Write;
                            let _ = write!(self.0, "{:?}", value);
                        }
                    }
                }
                event.record(&mut Visitor(&mut msg));
                if let Ok(mut logs) = self.logs.lock() {
                    logs.push((*event.metadata().level(), msg));
                }
            }
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = SimpleLogCapture { logs: logs.clone() };

        // 2. Run the orchestrator
        tracing::subscriber::with_default(subscriber, || {
            orchestrator
                .run(&mut packet, &storage, &config, temp.path())
                .unwrap();
        });

        // 3. Assert no INFO logs occurred
        let captured = logs.lock().unwrap();
        let info_logs: Vec<_> = captured
            .iter()
            .filter(|(lvl, _)| *lvl == tracing::Level::INFO)
            .collect();
        assert!(
            info_logs.is_empty(),
            "Expected no INFO logs during enrichment orchestration on empty corpus, but got: {:?}",
            info_logs
        );
    }
}
