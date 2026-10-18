//! 0544: the `truncate_for_context` meter counts the prompt text, not compact JSON.
//!
//! Lives under `metadata` so it can reach the private debit helpers. Every bound
//! is asserted against `reference_prompt_len`, an inline copy of the pre-0544
//! `build_user_prompt` pipeline, so a wrong production meter cannot grade itself.

use super::*;
use crate::index::symbols::{Symbol, SymbolKind};
use rstest::rstest;
use std::path::PathBuf;

/// Old `build_user_prompt` packet pipeline: fence, pretty, escape.
fn reference_prompt_len(packet: &ImpactPacket) -> usize {
    let mut view = packet.clone();
    for insight in &mut view.ai_insights {
        insight.content = crate::ai::fence_bridge_insight(&insight.content);
    }
    let json = serde_json::to_string_pretty(&view).expect("serialize reference packet");
    crate::ai::escape_code_chunk(&json).len()
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.into(),
        kind: SymbolKind::Function,
        is_public: true,
        cognitive_complexity: None,
        cyclomatic_complexity: None,
        line_start: None,
        line_end: None,
        qualified_name: None,
        byte_start: None,
        byte_end: None,
        entrypoint_kind: None,
        metadata: std::collections::BTreeMap::new(),
    }
}

fn change(path: &str, staged: bool, symbols: usize) -> super::super::ChangedFile {
    super::super::ChangedFile {
        path: PathBuf::from(path),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: staged,
        symbols: Some(
            (0..symbols)
                .map(|i| sym(&format!("function_{i}")))
                .collect(),
        ),
        imports: None,
        runtime_usage: None,
        analysis_status: Default::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    }
}

fn verification(stdout: String) -> super::super::VerificationResult {
    super::super::VerificationResult {
        name: "verify".to_string(),
        command: "ledgerful verify".to_string(),
        exit_code: 0,
        stdout,
        stderr: String::new(),
        duration_ms: 1,
        truncated: false,
    }
}

fn hotspot() -> super::super::Hotspot {
    super::super::Hotspot {
        path: PathBuf::from("a.rs"),
        score: 1.0,
        display_score: 1.0,
        complexity: 3,
        frequency: 1.0,
        centrality: None,
    }
}

fn decision(excerpt: String) -> super::super::RelevantDecision {
    super::super::RelevantDecision {
        file_path: PathBuf::from("docs/a.md"),
        heading: Some("Intro".to_string()),
        excerpt,
        similarity: 0.9,
        rerank_score: None,
        staleness_days: None,
        staleness_tier: None,
    }
}

fn insight(content: String) -> super::super::AiInsight {
    super::super::AiInsight {
        memory_id: "m1".to_string(),
        relevance: 0.5,
        content,
    }
}

fn empty_blast() -> super::super::BlastRadius {
    super::super::BlastRadius::default()
}

/// Tick-heavy text, so the 1-byte to 2-byte escape is exercised.
fn ticks(n: usize) -> String {
    "`".repeat(n)
}

// ---- DoD-3: field estimates ----

type Strip = fn(&mut ImpactPacket, &mut usize);

fn strip_stdout(p: &mut ImpactPacket, running: &mut usize) {
    let res = &mut p.verification_results[0];
    let old = std::mem::replace(&mut res.stdout, "[TRUNCATED]".to_string());
    debit_running(running, &old, &res.stdout, 3);
}

fn strip_stderr(p: &mut ImpactPacket, running: &mut usize) {
    let res = &mut p.verification_results[0];
    let old = std::mem::replace(&mut res.stderr, "[TRUNCATED]".to_string());
    debit_running(running, &old, &res.stderr, 3);
}

fn strip_truncated_flag(p: &mut ImpactPacket, running: &mut usize) {
    let res = &mut p.verification_results[0];
    let old = std::mem::replace(&mut res.truncated, true);
    debit_running(running, &old, &res.truncated, 3);
}

fn strip_temporal(p: &mut ImpactPacket, running: &mut usize) {
    debit_clear_vec(running, &mut p.temporal_couplings, 1);
}

fn strip_hotspots(p: &mut ImpactPacket, running: &mut usize) {
    debit_clear_vec(running, &mut p.hotspots, 1);
}

fn strip_symbols(p: &mut ImpactPacket, running: &mut usize) {
    let change = &mut p.changes[0];
    let old = change.symbols.take();
    debit_running(running, &old, &change.symbols, 3);
}

fn strip_service_map(p: &mut ImpactPacket, running: &mut usize) {
    debit_take_opt(running, &mut p.service_map_delta, 1);
}

fn strip_decisions(p: &mut ImpactPacket, running: &mut usize) {
    debit_clear_vec(running, &mut p.relevant_decisions, 1);
}

fn strip_insights(p: &mut ImpactPacket, running: &mut usize) {
    debit_clear_ai_insights(running, &mut p.ai_insights, 1);
}

fn strip_blast(p: &mut ImpactPacket, running: &mut usize) {
    debit_take_blast_radius(running, &mut p.blast_radius, 1);
}

fn packet_for(case: &str) -> ImpactPacket {
    let mut p = ImpactPacket::default();
    match case {
        "stdout" | "stderr" | "truncated" => {
            let mut res = verification(format!("x{}y", ticks(40)));
            res.stderr = format!("e{}", ticks(25));
            p.verification_results = vec![res];
        }
        "temporal" => {
            p.temporal_couplings = vec![super::super::TemporalCoupling {
                file_a: PathBuf::from("src/a.rs"),
                file_b: PathBuf::from("src/b.rs"),
                score: 0.9,
            }]
        }
        "hotspots" => p.hotspots = vec![hotspot(), hotspot()],
        "symbols" => p.changes = vec![change("src/a.rs", false, 4)],
        "service_map" => {
            p.service_map_delta = Some(super::super::ServiceMapDelta {
                services: Vec::new(),
                affected_services: vec!["users".to_string()],
                cross_service_edges: Vec::new(),
                total_services: 1,
            })
        }
        "decisions" => p.relevant_decisions = vec![decision(format!("see {}", ticks(10)))],
        "insights_ticks" => p.ai_insights = vec![insight(ticks(300))],
        "insights_over_cap" => {
            p.ai_insights = vec![insight(format!(
                "{}{}",
                ticks(100),
                "z".repeat(crate::ai::BRIDGE_INSIGHT_MAX_CHARS * 3)
            ))]
        }
        "blast_empty" => p.blast_radius = Some(empty_blast()),
        other => panic!("unknown fixture {other}"),
    }
    p
}

#[rstest]
#[case::stdout("stdout", strip_stdout as Strip, true)]
#[case::stderr("stderr", strip_stderr as Strip, true)]
#[case::truncated_flag("truncated", strip_truncated_flag as Strip, true)]
#[case::temporal("temporal", strip_temporal as Strip, true)]
#[case::hotspots("hotspots", strip_hotspots as Strip, true)]
#[case::symbols("symbols", strip_symbols as Strip, true)]
#[case::service_map("service_map", strip_service_map as Strip, true)]
#[case::decisions_skip_if_empty("decisions", strip_decisions as Strip, false)]
#[case::insights_ticks("insights_ticks", strip_insights as Strip, false)]
#[case::insights_over_cap("insights_over_cap", strip_insights as Strip, false)]
#[case::blast_some_empty("blast_empty", strip_blast as Strip, false)]
fn field_prompt_debit_tracks_the_reference_prompt(
    #[case] fixture: &str,
    #[case] strip: Strip,
    #[case] exact: bool,
) {
    let mut packet = packet_for(fixture);
    let before = reference_prompt_len(&packet);
    let mut running = before;
    strip(&mut packet, &mut running);
    let after = reference_prompt_len(&packet);
    let delta = before - after;
    let debited = before - running;
    if exact {
        assert_eq!(
            debited, delta,
            "{fixture}: debit must equal the prompt delta"
        );
    } else {
        // Value-only debit: a key emptied by a skip_serializing_if field keeps
        // its key line in the estimate (conservative), never the reverse.
        assert!(
            debited <= delta,
            "{fixture}: debit {debited} exceeds true delta {delta}"
        );
        assert!(
            delta - debited <= 40,
            "{fixture}: residual {} is more than one key line",
            delta - debited
        );
    }
    assert!(
        running >= after,
        "{fixture}: running {running} fell below the true length {after}"
    );
    // The same invariant through the confirm path the strip phases use.
    reset_truncate_serialize_count();
    confirm_prompt_len(&packet, running);
    let (confirmed_running, confirmed_actual) = take_last_confirm().expect("confirm recorded");
    assert_eq!(confirmed_actual, after);
    assert!(
        confirmed_running >= confirmed_actual,
        "{fixture}: confirm drift"
    );
}

#[test]
fn blast_radius_some_empty_is_not_in_the_measure() {
    let packet = packet_for("blast_empty");
    let none = ImpactPacket::default();
    assert_eq!(
        reference_prompt_len(&packet),
        reference_prompt_len(&none),
        "Some(empty) blast radius must be omitted by the serializer"
    );
}

// ---- DoD-4 / DoD-8: end to end bound and drift ----

fn nested_symbols_packet() -> ImpactPacket {
    ImpactPacket {
        changes: (0..6)
            .map(|i| change(&format!("src/module_{i}.rs"), false, 6))
            .collect(),
        hotspots: vec![hotspot()],
        ..ImpactPacket::default()
    }
}

fn tick_stdout_packet() -> ImpactPacket {
    ImpactPacket {
        verification_results: vec![verification(ticks(2_048))],
        hotspots: vec![hotspot()],
        ..ImpactPacket::default()
    }
}

fn tick_insight_packet() -> ImpactPacket {
    ImpactPacket {
        ai_insights: vec![insight(format!(
            "{}{}",
            ticks(2_100),
            "q".repeat(crate::ai::BRIDGE_INSIGHT_MAX_CHARS)
        ))],
        hotspots: vec![hotspot()],
        ..ImpactPacket::default()
    }
}

fn blast_hotspot_packet() -> ImpactPacket {
    ImpactPacket {
        blast_radius: Some(empty_blast()),
        relevant_decisions: vec![decision("d".repeat(600))],
        hotspots: vec![hotspot()],
        ..ImpactPacket::default()
    }
}

/// Prompt length of `packet` with every hotspot removed (the Phase 4 result
/// when earlier phases already emptied the rest).
fn without_hotspots(mut packet: ImpactPacket) -> usize {
    packet.hotspots.clear();
    reference_prompt_len(&packet)
}

#[rstest]
#[case::nested_symbols(nested_symbols_packet as fn() -> ImpactPacket)]
#[case::tick_stdout(tick_stdout_packet as fn() -> ImpactPacket)]
#[case::tick_insight(tick_insight_packet as fn() -> ImpactPacket)]
fn truncate_for_context_holds_the_prompt_to_the_target(#[case] build: fn() -> ImpactPacket) {
    let mut packet = build();
    let pre = reference_prompt_len(&packet);
    let compact_pre = serde_json::to_string(&packet).expect("compact").len();
    // One byte under the pre-strip prompt: the compact length still fits, the
    // prompt does not. The old meter stopped here without stripping.
    let target = pre - 1;
    assert!(
        compact_pre <= target,
        "fixture must be one where the compact meter says it fits ({compact_pre} vs {target})"
    );

    let (truncated, serializes) = packet.truncate_for_context_metered(target);

    assert!(truncated, "prompt {pre} is over target {target}");
    assert_eq!(serializes, 2);
    let actual = reference_prompt_len(&packet);
    assert!(
        actual <= target,
        "prompt {actual} still over target {target}"
    );
    assert_eq!(
        packet.hotspots.len(),
        1,
        "an earlier phase suffices, hotspots must survive"
    );
    let (running, confirmed) = take_last_confirm().expect("a strip ran, so confirm ran");
    assert_eq!(confirmed, actual, "confirm measures the real prompt text");
    assert!(
        running >= confirmed,
        "estimate {running} under-counted the prompt {confirmed}"
    );
}

#[test]
fn truncate_for_context_ignores_a_skipped_blast_radius_in_the_estimate() {
    let mut packet = blast_hotspot_packet();
    let blast_bytes = field_prompt_len(&packet.blast_radius, 1).expect("serialize blast");
    let hotspot_bytes = field_prompt_len(&packet.hotspots, 1).expect("serialize hotspots");
    assert!(
        blast_bytes > hotspot_bytes,
        "fixture needs the phantom blast debit ({blast_bytes}) to exceed the hotspots ({hotspot_bytes})"
    );
    // Reachable only by Phase 4: decisions and blast gone, hotspots gone.
    let mut stripped = packet.clone();
    stripped.relevant_decisions.clear();
    stripped.blast_radius = None;
    let phase3_len = reference_prompt_len(&stripped);
    let target = without_hotspots(stripped);
    assert!(phase3_len > target);

    let (truncated, _) = packet.truncate_for_context_metered(target);

    assert!(truncated);
    assert!(
        packet.hotspots.is_empty(),
        "a phantom blast debit ended Phase 3 early"
    );
    assert!(reference_prompt_len(&packet) <= target);
    let (running, confirmed) = take_last_confirm().expect("confirm ran");
    assert!(
        running >= confirmed,
        "estimate {running} < prompt {confirmed}"
    );
}
