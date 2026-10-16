//! Impact Analysis Module
//!
//! This module is the core of Ledgerful's change intelligence pipeline. It is responsible
//! for transforming raw repository changes into a structured, enriched `ImpactPacket`.
//!
//! ## Architecture
//!
//! The pipeline follows an **Orchestrator-Provider pattern**:
//!
//! 1. **Snapshot → Packet** (`orchestrator::map_snapshot_to_packet`):
//!    A `git::RepoSnapshot` is converted into an `ImpactPacket`. During this phase,
//!    file-level analysis (symbol extraction, import parsing, runtime usage detection)
//!    is performed via `index::analysis::analyze_file`.
//!
//! 2. **Orchestration** (`orchestrator::ImpactOrchestrator`):
//!    The orchestrator manages the lifecycle of enrichment providers. It builds an
//!    `EnrichmentContext` (database handle, file ID map, project root, config) and
//!    executes each registered provider in sequence.
//!
//! 3. **Enrichment** (`enrichment::*`):
//!    Modular providers contribute domain-specific data to the `ImpactPacket`:
//!    - **API** — route and contract enrichment
//!    - **Observability** — Prometheus and log-scraping signals
//!    - **Coupling** — structural, temporal, and data-flow coupling
//!    - **Services** — service map derivation and cross-service edges
//!    - **Infrastructure** — topology and deployment manifest detection
//!    - ...and others registered by `with_builtins` (KG, ADR, hotspots, …).
//!      Knowledge enrichment is implemented and unregistered. Risk scoring is
//!      `analysis_registry` (`AnalysisRegistry`), not an enrichment provider.
//!
//!    Each provider implements the `EnrichmentProvider` trait. Failures are isolated:
//!    if one provider fails, the orchestrator logs a warning and continues with the
//!    remaining providers (graceful degradation).
//!
//! 4. **Finalization** (`packet::ImpactPacket::finalize`):
//!    After all providers complete, the packet is finalized: collections are sorted,
//!    truncation budgets are applied, and deterministic ordering is enforced.
//!
//! 5. **Redaction** (`redact::redact_secrets`):
//!    Sensitive values are removed from the packet before persistence.
//!
//! 6. **Persistence** (`state::storage`):
//!    The finalized packet is saved to SQLite and optionally to the ledger.
//!
//! ## Determinism Contract
//!
//! All collections emitted by the impact pipeline are sorted. Risk levels, temporal
//! couplings, and hotspot rankings use stable, comparable ordering to ensure that
//! repeated runs on identical inputs produce identical outputs.
//!
//! ## Testing
//!
//! - Unit tests for individual enrichment providers use in-memory SQLite with
//!   mocked storage states.
//! - Integration tests (`tests/risk_analysis.rs`, `tests/temporal_coupling.rs`)
//!   verify end-to-end correctness.
//! - The orchestrator's resilient execution is tested by verifying that a failing
//!   provider does not prevent other providers from completing.

pub mod analysis;
pub mod budget;
pub mod enrichment;
pub mod hotspots;
pub mod lead;
pub mod orchestrator;
pub mod packet;
pub mod path_class;
pub mod redact;
pub mod scope;
pub mod temporal;

#[cfg(test)]
mod docs_honesty {
    #[test]
    fn rustdoc_risk_is_analysis_registry_not_enrichment() {
        let source = include_str!("mod.rs");
        let rustdoc = source
            .split("pub mod analysis")
            .next()
            .expect("module rustdoc precedes pub mod analysis");
        let risk_bullet = format!("- **{}** —", "Risk");
        assert!(
            !rustdoc.contains(&risk_bullet),
            "Risk must not be an enrichment-provider bullet"
        );
        assert!(
            rustdoc.contains("analysis_registry") || rustdoc.contains("AnalysisRegistry"),
            "rustdoc must name analysis_registry as the risk-scoring owner"
        );
    }

    #[test]
    fn contract_escalate_omits_impact_agent_summary() {
        let contract = include_str!("../../docs/agent-output-contract.md");
        let idx = contract
            .find("scan --impact --json")
            .expect("escalate sentence names scan --impact --json");
        let end = (idx + 280).min(contract.len());
        let neighborhood = &contract[idx..end];
        assert!(
            !neighborhood.contains("risk/blast/agentSummary"),
            "escalate must not put agentSummary on the impact packet: {neighborhood}"
        );
        let cc = contract
            .find("`change-context --json`")
            .expect("change-context table row");
        let row_end = (cc + 900).min(contract.len());
        let row = &contract[cc..row_end];
        assert!(
            row.contains("base-ref") || row.contains("base_ref"),
            "change-context timeout row must name --base-ref: {row}"
        );
    }
}
