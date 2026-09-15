use crate::config::model::Config;
use crate::git::{ChangeType, RepoSnapshot};
use crate::impact::analysis::AnalysisRegistry;
use crate::impact::budget::{
    AnalysisBudget, CompletenessStop, PROSPECTIVE_BUDGET_WARN, completeness_for_overall,
    stage_slug_for_provider,
};
use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
use crate::index::analysis::{AnalysisOutcome, analyze_file};
use crate::state::storage::StorageManager;
use crate::util::clock::SystemClock;
use indicatif::{ProgressBar, ProgressStyle};
use miette::Result;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Plumbing for history-backed enrichment (0308). Not hung on Config or ImpactPacket.
#[derive(Clone)]
pub struct ImpactHistoryOpts {
    pub skip_git_history_enrichment: bool,
    pub cancel: Arc<AtomicBool>,
    /// Overall emit budget seconds (`Some(0)` = no wall clock). `None` = not an
    /// overall-deadline run (keep 0034 federation 120s).
    pub overall_budget_secs: Option<u64>,
    /// Resolved Instant for `overall_budget_secs` (`None` when disabled).
    pub overall_deadline: Option<Instant>,
}

impl Default for ImpactHistoryOpts {
    fn default() -> Self {
        Self {
            skip_git_history_enrichment: false,
            cancel: Arc::new(AtomicBool::new(false)),
            overall_budget_secs: None,
            overall_deadline: None,
        }
    }
}

impl std::fmt::Debug for ImpactHistoryOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImpactHistoryOpts")
            .field(
                "skip_git_history_enrichment",
                &self.skip_git_history_enrichment,
            )
            .field("overall_budget_secs", &self.overall_budget_secs)
            .finish_non_exhaustive()
    }
}

impl ImpactHistoryOpts {
    pub fn for_run(
        skip_git_history_enrichment: bool,
        cancel: Arc<AtomicBool>,
        analysis_mode: &str,
        cli_timeout: Option<u64>,
        config: &Config,
    ) -> Self {
        let overall_budget_secs = crate::impact::budget::overall_budget_secs_for_mode(
            analysis_mode,
            cli_timeout,
            config.impact.prospective_budget_secs,
        );
        let overall_deadline = match overall_budget_secs {
            Some(0) | None => None,
            Some(secs) => Some(Instant::now() + Duration::from_secs(secs)),
        };
        Self {
            skip_git_history_enrichment,
            cancel,
            overall_budget_secs,
            overall_deadline,
        }
    }
}

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
    ///
    /// When `LEDGERFUL_NO_NETWORK` is truthy (Action CI / offline honesty, track
    /// 0082 RT-X3), skips the reachability probe entirely — no `TcpStream` —
    /// and annotates that enrichment was skipped for the env policy.
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
        // RT-X3: honor Action's LEDGERFUL_NO_NETWORK=1 before any socket open.
        if crate::util::network::network_disabled_from_env() {
            return Some("AI enrichment skipped: LEDGERFUL_NO_NETWORK is set".to_string());
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
        orch.register_enrichment_provider(Box::new(
            crate::impact::enrichment::signature_delta::SignatureDeltaProvider,
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
        self.run_with_history_opts(
            packet,
            storage,
            config,
            project_root,
            ImpactHistoryOpts::default(),
        )
    }

    pub fn run_with_history_opts(
        &self,
        packet: &mut ImpactPacket,
        storage: &StorageManager,
        config: &Config,
        project_root: &Path,
        opts: ImpactHistoryOpts,
    ) -> Result<()> {
        debug!("Starting impact orchestration...");

        // 0147: empty-tree fast path — no AI probe, no providers, no analysis registry.
        // Callers with zero structural seeds (clean tree or empty base-ref) get honest
        // low-risk defaults without O(repo) enrichment (federation walk, hotspots, …).
        if packet.tree_clean && packet.changes.is_empty() {
            apply_empty_tree_impact_defaults(packet);
            return Ok(());
        }

        // Annotate AI enrichment availability up front so the output always shows
        // whether AI-driven enrichment (semantic / embedding / knowledge) was
        // skipped. This keeps the impact command deterministic and non-fatal when
        // the local model is down.
        if let Some(status) = Self::ai_enrichment_status(config) {
            packet.analysis_warnings.push(status);
        }

        // 1. Prepare Context — 0257: ChangedFile.path only (not rename old_path).
        let changed_paths: Vec<_> = packet.changes.iter().map(|c| c.path.as_path()).collect();
        let file_id_map = storage.get_active_file_id_map_for_paths(&changed_paths)?;
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
        let overall_deadline = opts.overall_deadline;
        let deadline =
            overall_deadline.unwrap_or_else(|| Instant::now() + config.federation.scan_timeout());

        let history_budget = Some(AnalysisBudget::capped_by_overall(
            config.hotspots.history_budget_secs,
            opts.overall_deadline,
            Arc::clone(&opts.cancel),
        ));
        let context = EnrichmentContext {
            storage,
            config,
            file_id_map,
            project_root: project_root.to_path_buf(),
            warnings: Arc::clone(&warnings_collector),
            deadline,
            skip_git_history_enrichment: opts.skip_git_history_enrichment,
            history_budget,
        };

        // 2. Execute Enrichment Providers (Resilient Execution)
        for provider in &self.enrichment_providers {
            let name = provider.name();
            let slug = stage_slug_for_provider(name);
            if opts.cancel.load(Ordering::Relaxed) {
                if opts.overall_deadline.is_some() {
                    apply_overall_stop(
                        packet,
                        CompletenessStop::Cancelled,
                        opts.overall_budget_secs,
                        slug,
                    );
                }
                break;
            }
            if Instant::now() >= deadline {
                if opts.overall_deadline.is_some() {
                    apply_overall_stop(
                        packet,
                        CompletenessStop::Budget,
                        opts.overall_budget_secs,
                        slug,
                    );
                } else {
                    let msg = format!(
                        "Impact scan exceeded overall timeout ({}s); stopping before provider '{}'. Partial results retained.",
                        config.federation.scan_timeout_secs, name
                    );
                    warn!("{}", msg);
                    context.add_warning(msg);
                }
                break;
            }
            debug!("Running enrichment provider: {}", name);

            if let Err(e) = provider.enrich(&context, packet) {
                warn!("Enrichment provider '{}' failed: {}", name, e);
                context.add_warning(format!("Provider '{}' failed: {}", name, e));
            }
            // In-flight providers are not killed. If the last (or current)
            // provider returns after the overall Instant, still record the
            // stop so persist is skipped and completeness is honest.
            if opts.overall_deadline.is_some() {
                if opts.cancel.load(Ordering::Relaxed) {
                    apply_overall_stop(
                        packet,
                        CompletenessStop::Cancelled,
                        opts.overall_budget_secs,
                        slug,
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    apply_overall_stop(
                        packet,
                        CompletenessStop::Budget,
                        opts.overall_budget_secs,
                        slug,
                    );
                    break;
                }
            }
        }

        // 3. Execute Analysis (Scoring) — load policy rules via resolved layout.
        // Rules-only: soft-fail to defaults. Never invent private worktree state
        // when git discover succeeded but resolve failed (fail-closed).
        let rules = match crate::state::layout::get_layout_or_cwd_if_not_git() {
            Ok(layout) => match crate::policy::load::load_rules(&layout) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Failed to load policy rules: {}", e);
                    context.add_warning(format!("Failed to load policy rules: {}", e));
                    crate::policy::rules::Rules::default()
                }
            },
            Err(e) => {
                warn!(
                    "Failed to resolve layout for policy rules (no private state invent): {}",
                    e
                );
                context.add_warning(format!("Failed to resolve layout for policy rules: {e}"));
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

fn apply_overall_stop(
    packet: &mut ImpactPacket,
    stop: CompletenessStop,
    budget_secs: Option<u64>,
    stage: &str,
) {
    let secs = budget_secs.filter(|s| *s > 0);
    packet.completeness = Some(completeness_for_overall(stop, secs, stage));
    if stop == CompletenessStop::Budget {
        warn!(stage, ?stop, "{PROSPECTIVE_BUDGET_WARN}");
        eprintln!("{PROSPECTIVE_BUDGET_WARN}");
    } else {
        warn!(stage, ?stop, "prospective analysis stopped");
    }
}

/// Apply empty-tree impact defaults when there are no structural seeds.
///
/// Only meaningful when `tree_clean && changes.is_empty()`; no-ops otherwise.
/// Matches the empty risk copy in [`ImpactPacket::finalize_risk_level`] so
/// empty packets stay agent-honest without running the analysis registry.
/// Does **not** add federation / Cross-repo analysis warnings.
pub(crate) fn apply_empty_tree_impact_defaults(packet: &mut ImpactPacket) {
    if !(packet.tree_clean && packet.changes.is_empty()) {
        return;
    }
    packet.risk_level = crate::impact::packet::RiskLevel::Low;
    if packet.risk_reasons.is_empty() {
        packet.risk_reasons.push("No changes detected".to_string());
    }
}

pub(crate) fn map_snapshot_to_packet(
    snapshot: RepoSnapshot,
    base_dir: &Path,
) -> Result<ImpactPacket> {
    map_snapshot_to_packet_with_progress(snapshot, base_dir, false)
}

/// Map a git snapshot to an impact packet.
///
/// `hide_progress` is the explicit `--json` / silent-path flag (0347): do not
/// rely on TTY auto-hide. Machine stdout must stay spinner-free.
pub(crate) fn map_snapshot_to_packet_with_progress(
    snapshot: RepoSnapshot,
    base_dir: &Path,
    hide_progress: bool,
) -> Result<ImpactPacket> {
    let mut packet = ImpactPacket {
        head_hash: snapshot.head_hash,
        branch_name: snapshot.branch_name,
        tree_clean: snapshot.is_clean,
        ..ImpactPacket::with_clock(&SystemClock)
    };

    let pb = if hide_progress {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(snapshot.changes.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta}) {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        pb.set_message("Extracting symbols...");
        pb
    };

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
                    content: None,
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

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    #[test]
    #[serial_test::serial(env)]
    fn test_ai_enrichment_status_unreachable_annotates() {
        let _no_net = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
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
    #[serial_test::serial(env)]
    fn test_ai_enrichment_status_empty_url_is_none() {
        let _no_net = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let config = Config::default();
        assert!(
            ImpactOrchestrator::ai_enrichment_status(&config).is_none(),
            "empty base_url should not produce a skip annotation"
        );
    }

    /// DoD-4 (0082 RT-X3): with `LEDGERFUL_NO_NETWORK=1` and a configured
    /// `base_url`, enrichment annotates the env policy and never enters
    /// `is_url_reachable` (zero probe / TcpStream).
    #[test]
    #[serial_test::serial(env)]
    fn test_ai_enrichment_status_no_network_skips_probe() {
        let _no_net = TempEnv::set(crate::util::network::NO_NETWORK_ENV, "1");
        let mut config = Config::default();
        // DEFAULT_CONFIG-style URL: would normally trigger a reachability probe.
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        config.local_model.embedding_url = Some("http://127.0.0.1:8083".to_string());

        crate::util::network::reset_reachability_probe_call_count();
        let before = crate::util::network::reachability_probe_call_count();

        let status = ImpactOrchestrator::ai_enrichment_status(&config);
        let after = crate::util::network::reachability_probe_call_count();

        assert_eq!(
            before, after,
            "is_url_reachable must not be entered when LEDGERFUL_NO_NETWORK is set"
        );
        let msg = status.expect("expected skip annotation under LEDGERFUL_NO_NETWORK");
        assert!(
            msg.contains("LEDGERFUL_NO_NETWORK"),
            "expected env-policy annotation, got: {msg}"
        );
        assert!(
            !msg.contains("unreachable"),
            "must not claim model unreachable when probe was skipped: {msg}"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn test_ai_enrichment_status_no_network_true_case_insensitive() {
        let _no_net = TempEnv::set(crate::util::network::NO_NETWORK_ENV, "True");
        let mut config = Config::default();
        config.local_model.base_url = "http://127.0.0.1:8081".to_string();
        crate::util::network::reset_reachability_probe_call_count();
        let status = ImpactOrchestrator::ai_enrichment_status(&config);
        assert_eq!(crate::util::network::reachability_probe_call_count(), 0);
        assert!(
            status
                .as_deref()
                .is_some_and(|m| m.contains("LEDGERFUL_NO_NETWORK")),
            "got: {status:?}"
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn test_orchestrator_annotates_ai_unavailable_and_continues() {
        let _no_net = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
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

    /// 0147 B1: empty tree_clean + empty changes short-circuits before any
    /// enrichment provider (spy panics if called) and before AI probe.
    #[test]
    #[serial_test::serial(env)]
    fn test_empty_tree_short_circuit_skips_providers_and_ai_probe() {
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use crate::impact::packet::RiskLevel;
        use std::sync::atomic::{AtomicBool, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        struct FlagSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for FlagSpy {
            fn name(&self) -> &'static str {
                "FlagSpy"
            }
            fn enrich(
                &self,
                _context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                self.called.store(true, Ordering::SeqCst);
                panic!("FlagSpy must not be called on empty-tree short-circuit");
            }
        }

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        // Unreachable model would normally annotate AI skip — short-circuit must
        // not run the probe either.
        let _no_net = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
        let mut config = Config::default();
        config.local_model.base_url = "http://127.0.0.1:1".to_string();
        let temp = tempfile::tempdir().unwrap();

        let mut packet = ImpactPacket {
            tree_clean: true,
            changes: Vec::new(),
            head_hash: Some("abc123".to_string()),
            ..ImpactPacket::default()
        };

        crate::util::network::reset_reachability_probe_call_count();
        let probe_before = crate::util::network::reachability_probe_call_count();

        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(FlagSpy {
            called: Arc::clone(&called),
        }));

        orchestrator
            .run(&mut packet, &storage, &config, temp.path())
            .expect("empty-tree short-circuit should return Ok");

        let probe_after = crate::util::network::reachability_probe_call_count();
        assert_eq!(
            probe_before, probe_after,
            "AI reachability probe must not run on empty-tree short-circuit"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "enrichment provider must not be called on empty-tree short-circuit"
        );
        assert_eq!(packet.risk_level, RiskLevel::Low);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r == "No changes detected"),
            "expected 'No changes detected', got {:?}",
            packet.risk_reasons
        );
        assert!(
            packet.analysis_warnings.is_empty(),
            "empty path must not emit federation/AI warnings, got {:?}",
            packet.analysis_warnings
        );
    }

    /// 0147 B5: dirty path still enters enrichment providers.
    #[test]
    fn test_dirty_path_still_runs_providers() {
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use std::sync::atomic::{AtomicBool, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        struct FlagSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for FlagSpy {
            fn name(&self) -> &'static str {
                "FlagSpy"
            }
            fn enrich(
                &self,
                _context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                self.called.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();

        let mut packet = ImpactPacket {
            tree_clean: false,
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        };

        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(FlagSpy {
            called: Arc::clone(&called),
        }));

        orchestrator
            .run(&mut packet, &storage, &config, temp.path())
            .expect("dirty-path orchestrator run should return Ok");

        assert!(
            called.load(Ordering::SeqCst),
            "enrichment provider must be called when changes are non-empty"
        );
    }

    #[test]
    fn overall_expired_stops_before_cooperative_provider() {
        use crate::impact::budget::{CompletenessScope, CompletenessStop, PROSPECTIVE_BUDGET_WARN};
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use crate::impact::packet::RiskLevel;
        use std::sync::atomic::{AtomicBool, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        struct CooperativeSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for CooperativeSpy {
            fn name(&self) -> &'static str {
                "Federated Intelligence Enrichment Provider"
            }
            fn enrich(
                &self,
                context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                if std::time::Instant::now() >= context.deadline {
                    return Ok(());
                }
                self.called.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket {
            tree_clean: false,
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        };
        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(CooperativeSpy {
            called: Arc::clone(&called),
        }));
        let opts = ImpactHistoryOpts {
            overall_budget_secs: Some(25),
            overall_deadline: Some(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            ),
            ..ImpactHistoryOpts::default()
        };
        orchestrator
            .run_with_history_opts(&mut packet, &storage, &config, temp.path(), opts)
            .expect("overall stop should return Ok");
        assert!(
            !called.load(Ordering::SeqCst),
            "expired overall must skip the provider"
        );
        let c = packet.completeness.expect("overall completeness");
        assert_eq!(c.stop, CompletenessStop::Budget);
        assert_eq!(c.scope, Some(CompletenessScope::Overall));
        assert_eq!(c.stage.as_deref(), Some("federated"));
        assert_eq!(c.budget_secs, Some(25));
        assert!(c.filter.is_none());
        assert!(c.commits_requested.is_none());
        assert!(
            !packet
                .analysis_warnings
                .iter()
                .any(|w| w == PROSPECTIVE_BUDGET_WARN),
            "warn token must not appear in JSON analysisWarnings: {:?}",
            packet.analysis_warnings
        );
        assert_eq!(packet.risk_level, RiskLevel::Low);
    }

    #[test]
    #[allow(non_snake_case)]
    fn prospective_one_file__overall_expired__emits_core_json_with_scope_overall() {
        overall_expired_stops_before_cooperative_provider();
    }

    #[test]
    #[allow(non_snake_case)]
    fn timeout_zero__disables_overall_wall_clock() {
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use std::sync::atomic::{AtomicBool, Ordering};
        let called = Arc::new(AtomicBool::new(false));
        struct Spy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for Spy {
            fn name(&self) -> &'static str {
                "FlagSpy"
            }
            fn enrich(
                &self,
                _context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                self.called.store(true, Ordering::SeqCst);
                Ok(())
            }
        }
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket {
            tree_clean: false,
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        };
        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(Spy {
            called: Arc::clone(&called),
        }));
        let opts = ImpactHistoryOpts {
            overall_budget_secs: Some(0),
            overall_deadline: None,
            ..ImpactHistoryOpts::default()
        };
        orchestrator
            .run_with_history_opts(&mut packet, &storage, &config, temp.path(), opts)
            .expect("timeout 0 should not overall-stop");
        assert!(called.load(Ordering::SeqCst));
        assert!(
            packet
                .completeness
                .as_ref()
                .is_none_or(|c| c.scope.is_none()),
            "timeout 0 must not emit scope=overall: {:?}",
            packet.completeness
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn map_snapshot_to_packet__json__hides_spinner() {
        let src = include_str!("orchestrator.rs");
        assert!(
            src.contains("ProgressBar::hidden()"),
            "json/silent path must construct ProgressBar::hidden, not TTY auto-hide"
        );
        assert!(
            src.contains("hide_progress"),
            "map_snapshot_to_packet must take an explicit hide_progress flag"
        );
        let snapshot = RepoSnapshot {
            head_hash: Some("abc123".to_string()),
            branch_name: Some("main".to_string()),
            is_clean: false,
            changes: vec![make_added("src/lib.rs")],
        };
        let temp = tempfile::tempdir().unwrap();
        let packet = map_snapshot_to_packet_with_progress(snapshot, temp.path(), true).unwrap();
        assert_eq!(packet.changes.len(), 1);
    }

    fn dirty_one_file_packet() -> ImpactPacket {
        ImpactPacket {
            tree_clean: false,
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/lib.rs"),
                status: "Modified".to_string(),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        }
    }

    fn memory_storage() -> (StorageManager, tempfile::TempDir) {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        (storage, tempfile::tempdir().unwrap())
    }

    #[test]
    #[allow(non_snake_case)]
    fn prospective_one_file__cooperative_stalled_provider__retains_changes_and_risk_reasons() {
        use crate::impact::budget::{CompletenessScope, CompletenessStop};
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use crate::impact::packet::TemporalCoupling;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let coupling_called = Arc::new(AtomicBool::new(false));
        let stalled_called = Arc::new(AtomicBool::new(false));
        let analyze_count = Arc::new(AtomicUsize::new(0));

        struct CouplingSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for CouplingSpy {
            fn name(&self) -> &'static str {
                "Coupling Enrichment Provider"
            }
            fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
                self.called.store(true, Ordering::SeqCst);
                packet.risk_reasons.push("coupling-seed-reason".to_string());
                packet.temporal_couplings.push(TemporalCoupling {
                    file_a: std::path::PathBuf::from("src/a.rs"),
                    file_b: std::path::PathBuf::from("src/b.rs"),
                    score: 0.91,
                });
                let give_up = Instant::now() + Duration::from_secs(2);
                while Instant::now() < context.deadline && Instant::now() < give_up {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            }
        }

        struct StalledSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for StalledSpy {
            fn name(&self) -> &'static str {
                "Federated Intelligence Enrichment Provider"
            }
            fn enrich(
                &self,
                context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                if Instant::now() >= context.deadline {
                    return Ok(());
                }
                self.called.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        struct CountAnalysis {
            n: Arc<AtomicUsize>,
        }
        impl crate::impact::analysis::ImpactProvider for CountAnalysis {
            fn name(&self) -> &'static str {
                "CountAnalysis"
            }
            fn analyze(
                &self,
                _packet: &ImpactPacket,
                _rules: &crate::policy::rules::Rules,
                _config: &Config,
            ) -> Result<crate::impact::packet::RiskImpact> {
                self.n.fetch_add(1, Ordering::SeqCst);
                Ok(crate::impact::packet::RiskImpact::default())
            }
        }

        let (storage, temp) = memory_storage();
        let config = Config::default();
        let mut packet = dirty_one_file_packet();
        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(CouplingSpy {
            called: Arc::clone(&coupling_called),
        }));
        orchestrator.register_enrichment_provider(Box::new(StalledSpy {
            called: Arc::clone(&stalled_called),
        }));
        orchestrator.register_analysis_provider(Box::new(CountAnalysis {
            n: Arc::clone(&analyze_count),
        }));
        let opts = ImpactHistoryOpts {
            overall_budget_secs: Some(25),
            overall_deadline: Some(Instant::now() + Duration::from_millis(150)),
            ..ImpactHistoryOpts::default()
        };
        orchestrator
            .run_with_history_opts(&mut packet, &storage, &config, temp.path(), opts)
            .expect("cooperative stall should return Ok");
        assert!(
            coupling_called.load(Ordering::SeqCst),
            "first provider must run and honor context.deadline"
        );
        assert!(
            !stalled_called.load(Ordering::SeqCst),
            "later provider must not start after overall Instant"
        );
        assert_eq!(packet.changes.len(), 1);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r == "coupling-seed-reason"),
            "seed risk reasons must survive the overall stop: {:?}",
            packet.risk_reasons
        );
        assert!(
            packet.risk_reasons.iter().any(|r| r.contains("temporal")),
            "scoring after stop must keep coupling-backed temporal reasons: {:?}",
            packet.risk_reasons
        );
        assert_eq!(analyze_count.load(Ordering::SeqCst), 1);
        let c = packet.completeness.expect("overall completeness");
        assert_eq!(c.stop, CompletenessStop::Budget);
        assert_eq!(c.scope, Some(CompletenessScope::Overall));
    }

    #[test]
    #[allow(non_snake_case)]
    fn analysis_registry__runs_once_after_stop__temporal_reasons_present_when_coupling_finished() {
        prospective_one_file__cooperative_stalled_provider__retains_changes_and_risk_reasons();
    }

    #[test]
    #[allow(non_snake_case)]
    fn completeness__overall_stop_overwrites_history_object() {
        use crate::impact::budget::{
            CompletenessFilter, CompletenessScope, CompletenessStop, completeness_for_error,
        };
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};

        struct SkipSpy;
        impl EnrichmentProvider for SkipSpy {
            fn name(&self) -> &'static str {
                "Hotspot Enrichment Provider"
            }
            fn enrich(
                &self,
                _context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                panic!("expired overall must not start providers");
            }
        }

        let (storage, temp) = memory_storage();
        let config = Config::default();
        let mut packet = dirty_one_file_packet();
        packet.completeness = Some(completeness_for_error(
            500,
            None,
            CompletenessFilter::Default,
            Some(45),
        ));
        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(SkipSpy));
        let opts = ImpactHistoryOpts {
            overall_budget_secs: Some(25),
            overall_deadline: Some(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            ),
            ..ImpactHistoryOpts::default()
        };
        orchestrator
            .run_with_history_opts(&mut packet, &storage, &config, temp.path(), opts)
            .expect("overall overwrite should return Ok");
        let c = packet.completeness.expect("overwritten completeness");
        assert_eq!(c.scope, Some(CompletenessScope::Overall));
        assert_eq!(c.stop, CompletenessStop::Budget);
        assert_eq!(c.stage.as_deref(), Some("hotspots"));
        assert_eq!(c.budget_secs, Some(25));
        assert!(c.filter.is_none());
        assert!(c.commits_requested.is_none());
    }

    #[test]
    #[allow(non_snake_case)]
    fn final_provider__returns_after_deadline__records_overall_stop() {
        use crate::impact::budget::{CompletenessScope, CompletenessStop, is_overall_stop};
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        let analyze_count = Arc::new(AtomicUsize::new(0));

        struct LastSpy {
            called: Arc<AtomicBool>,
        }
        impl EnrichmentProvider for LastSpy {
            fn name(&self) -> &'static str {
                "Hotspot Enrichment Provider"
            }
            fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
                self.called.store(true, Ordering::SeqCst);
                packet.risk_reasons.push("last-provider-seed".to_string());
                let give_up = Instant::now() + Duration::from_secs(2);
                while Instant::now() < context.deadline && Instant::now() < give_up {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(20));
                Ok(())
            }
        }

        struct CountAnalysis {
            n: Arc<AtomicUsize>,
        }
        impl crate::impact::analysis::ImpactProvider for CountAnalysis {
            fn name(&self) -> &'static str {
                "CountAnalysis"
            }
            fn analyze(
                &self,
                _packet: &ImpactPacket,
                _rules: &crate::policy::rules::Rules,
                _config: &Config,
            ) -> Result<crate::impact::packet::RiskImpact> {
                self.n.fetch_add(1, Ordering::SeqCst);
                Ok(crate::impact::packet::RiskImpact::default())
            }
        }

        let (storage, temp) = memory_storage();
        let config = Config::default();
        let mut packet = dirty_one_file_packet();
        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(LastSpy {
            called: Arc::clone(&called),
        }));
        orchestrator.register_analysis_provider(Box::new(CountAnalysis {
            n: Arc::clone(&analyze_count),
        }));
        let opts = ImpactHistoryOpts {
            overall_budget_secs: Some(8),
            overall_deadline: Some(Instant::now() + Duration::from_millis(40)),
            ..ImpactHistoryOpts::default()
        };
        orchestrator
            .run_with_history_opts(&mut packet, &storage, &config, temp.path(), opts)
            .expect("last-provider over deadline should return Ok");
        assert!(called.load(Ordering::SeqCst));
        let c = packet
            .completeness
            .expect("overall completeness after last provider");
        assert_eq!(c.stop, CompletenessStop::Budget);
        assert_eq!(c.scope, Some(CompletenessScope::Overall));
        assert_eq!(c.stage.as_deref(), Some("hotspots"));
        assert!(is_overall_stop(&c));
        assert_eq!(analyze_count.load(Ordering::SeqCst), 1);
        assert!(
            packet
                .risk_reasons
                .iter()
                .any(|r| r == "last-provider-seed"),
            "partial enrichment must be retained: {:?}",
            packet.risk_reasons
        );
    }

    /// 0257 DoD-1: non-empty impact map excludes uninvolved `project_files` rows.
    #[test]
    fn test_non_empty_impact_file_id_map_excludes_uninvolved_paths() {
        use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
        use std::collections::HashMap;
        use std::path::PathBuf;

        let seen = Arc::new(Mutex::new(HashMap::new()));
        struct MapSpy {
            seen: Arc<Mutex<HashMap<PathBuf, i64>>>,
        }
        impl EnrichmentProvider for MapSpy {
            fn name(&self) -> &'static str {
                "MapSpy"
            }
            fn enrich(
                &self,
                context: &EnrichmentContext,
                _packet: &mut ImpactPacket,
            ) -> Result<()> {
                *self.seen.lock().expect("map spy mutex") = context.file_id_map.clone();
                Ok(())
            }
        }

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let storage = StorageManager::init_from_conn(conn);
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/lib.rs', 'OK', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/extra.rs', 'OK', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/gone.rs', 'DELETED', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO project_files (file_path, parse_status, last_indexed_at) VALUES ('src/old.rs', 'OK', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        let config = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let mut packet = ImpactPacket {
            tree_clean: false,
            changes: vec![ChangedFile {
                path: std::path::PathBuf::from("src/lib.rs"),
                status: "Renamed".to_string(),
                old_path: Some(std::path::PathBuf::from("src/old.rs")),
                ..ChangedFile::default()
            }],
            ..ImpactPacket::default()
        };

        let mut orchestrator = ImpactOrchestrator::new();
        orchestrator.register_enrichment_provider(Box::new(MapSpy {
            seen: Arc::clone(&seen),
        }));

        orchestrator
            .run(&mut packet, &storage, &config, temp.path())
            .expect("non-empty impact should return Ok");

        let map = seen.lock().expect("map spy mutex");
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&PathBuf::from("src/lib.rs")));
        assert!(
            !map.contains_key(&PathBuf::from("src/extra.rs")),
            "uninvolved project_files row must be absent from the impact file-id map"
        );
        assert!(!map.contains_key(&PathBuf::from("src/gone.rs")));
        assert!(
            !map.contains_key(&PathBuf::from("src/old.rs")),
            "rename old_path must not be IN-listed"
        );
    }

    #[test]
    fn test_apply_empty_tree_impact_defaults() {
        use crate::impact::packet::RiskLevel;

        let mut packet = ImpactPacket {
            tree_clean: true,
            changes: Vec::new(),
            risk_level: RiskLevel::Medium,
            risk_reasons: Vec::new(),
            ..ImpactPacket::default()
        };
        apply_empty_tree_impact_defaults(&mut packet);
        assert_eq!(packet.risk_level, RiskLevel::Low);
        assert_eq!(packet.risk_reasons, vec!["No changes detected".to_string()]);

        // No-op when not empty-tree.
        let mut dirty = ImpactPacket {
            tree_clean: false,
            changes: Vec::new(),
            risk_level: RiskLevel::Medium,
            risk_reasons: Vec::new(),
            ..ImpactPacket::default()
        };
        apply_empty_tree_impact_defaults(&mut dirty);
        assert_eq!(dirty.risk_level, RiskLevel::Medium);
        assert!(dirty.risk_reasons.is_empty());
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
