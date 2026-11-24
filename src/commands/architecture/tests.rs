use super::test_support::{COMPONENTS, INCOMPLETE_REASONS, edge_set, fixture_config};
use super::*;
use crate::commands::baseline::InputCompleteness;
use serde_json::Value;
use std::collections::BTreeSet;

const FORWARD: (&str, &str, u32) = ("src/a/x.rs", "src/b/x.rs", 1);
const REVERSE: (&str, &str, u32) = ("src/b/x.rs", "src/a/x.rs", 2);
const EVALUATED_KEYS: &[&str] = &[
    "mapping",
    "unconstrainedCrossEdges",
    "violations",
    "cycleGroups",
    "removedViolations",
    "removedCycleGroups",
    "counts",
];

fn envelope(current: FileEdgeSet, baseline: Option<BaselineInput>) -> ArchitectureEnvelope {
    let config = fixture_config();
    let map = ComponentMap::build(&config).unwrap();
    let metadata = ConfigMetadata {
        path: DEFAULT_CONFIG_REL.to_owned(),
        source: ConfigSource::Local,
        digest: config_digest(COMPONENTS.as_bytes()),
        component_count: config.components.len(),
        rule_count: config.rules.len(),
    };
    build_envelope(EnvelopeInputs {
        loaded: LoadedConfig { config, metadata },
        map: &map,
        current,
        baseline,
    })
}

fn json(current: FileEdgeSet, baseline: Option<BaselineInput>) -> Value {
    serde_json::to_value(envelope(current, baseline)).unwrap()
}

fn comparable(set: FileEdgeSet, current: &FileEdgeSet) -> BaselineInput {
    baseline_from_verified("before".to_owned(), set, current)
}

fn incomparable(reason: &str) -> BaselineInput {
    BaselineInput {
        metadata: BaselineMetadata {
            name: "before".to_owned(),
            status: BaselineStatus::Incomparable,
            incomparable_reasons: vec![reason.to_owned()],
            notes: Vec::new(),
        },
        comparable_set: None,
    }
}

#[test]
fn architecture_envelope_evaluated_no_baseline_shape() {
    let value = json(edge_set(&[FORWARD]), None);
    let keys: BTreeSet<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "schemaVersion",
            "kind",
            "config",
            "evidence",
            "status",
            "statusReasons",
            "observation",
            "observationReasons",
            "regression",
            "regressionReasons",
            "mapping",
            "unconstrainedCrossEdges",
            "violations",
            "cycleGroups",
        ])
    );
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["kind"], "architectureCheck");
    assert_eq!(value["status"], "evaluated");
    assert_eq!(value["observation"], "noneObserved");
    assert_eq!(value["regression"], "notEvaluated");
    assert_eq!(value["evidence"]["indexHeadHash"], "fixture-head");
    assert!(value.get("baseline").is_none());
}

#[test]
fn architecture_envelope_unknown_status_omits_findings_keys() {
    for &reason in INCOMPLETE_REASONS {
        let mut current = edge_set(&[FORWARD, REVERSE]);
        current.identity.input_completeness =
            InputCompleteness::incomplete(vec![reason.to_owned()]);
        for baseline in [None, Some(incomparable("CURRENT_INCOMPLETE"))] {
            let value = json(current.clone(), baseline);
            assert_eq!(value["status"], "unknown");
            assert_eq!(value["statusReasons"], serde_json::json!([reason]));
            assert_eq!(value["observation"], "unknown");
            assert_eq!(value["evidence"]["state"], "INCOMPLETE");
            assert_eq!(value["evidence"]["reasons"], serde_json::json!([reason]));
            for key in EVALUATED_KEYS {
                assert!(value.get(key).is_none(), "{reason}: {key} should be absent");
            }
        }
    }
}

#[test]
fn architecture_envelope_comparable_shape_has_class_counts_removed() {
    let current = edge_set(&[FORWARD, REVERSE]);
    let value = json(current.clone(), Some(comparable(current.clone(), &current)));
    assert_eq!(value["baseline"]["status"], "comparable");
    assert_eq!(
        value["baseline"]["incomparableReasons"],
        serde_json::json!([])
    );
    assert_eq!(value["regression"], "none");
    assert_eq!(value["violations"][0]["class"], "existing");
    assert_eq!(value["violations"][0]["baselineWeight"], 2);
    assert_eq!(value["cycleGroups"][0]["class"], "unchanged");
    assert_eq!(value["counts"]["violations"]["existing"], 1);
    assert_eq!(value["counts"]["cycleGroups"]["unchanged"], 1);
    assert_eq!(value["removedViolations"], serde_json::json!([]));
    assert_eq!(value["removedCycleGroups"], serde_json::json!([]));
    assert!(value["baseline"].get("payloadDigest").is_none());
}

#[test]
fn architecture_envelope_incomparable_omits_class_counts_removed_but_lists_current_findings() {
    let current = edge_set(&[FORWARD, REVERSE]);
    let mut earlier = edge_set(&[FORWARD, REVERSE, ("src/b/extra.rs", "src/a/x.rs", 1)]);
    earlier.identity.ledgerful_version = "older-analyzer".to_owned();
    assert!(
        compare_edge_sets(&earlier, &current)
            .notes
            .iter()
            .any(|note| note == "RESOLVED_EDGES_REMOVED")
    );
    let note = json(current.clone(), Some(comparable(earlier, &current)));
    assert_eq!(
        note["baseline"]["notes"],
        serde_json::json!(["analyzerBuildDiffers"])
    );
    assert_eq!(note["baseline"]["status"], "comparable");

    for reason in [
        "BASELINE_DIGEST_MISMATCH",
        "BASELINE_INCOMPLETE",
        "BASELINE_CORRUPT",
        "EDGE_SOURCES_MISMATCH",
        "PARSER_VERSION_MISMATCH",
        "SCHEMA_MISMATCH",
        "SCOPE_MISMATCH",
    ] {
        let mut earlier = current.clone();
        let baseline = match reason {
            "BASELINE_INCOMPLETE" => {
                earlier.identity.input_completeness =
                    InputCompleteness::incomplete(vec!["INDEX_EMPTY".to_owned()]);
                comparable(earlier, &current)
            }
            "EDGE_SOURCES_MISMATCH" => {
                earlier.identity.edge_sources = vec!["scip".to_owned()];
                comparable(earlier, &current)
            }
            "PARSER_VERSION_MISMATCH" => {
                earlier.identity.parser_version = "older".to_owned();
                comparable(earlier, &current)
            }
            "SCHEMA_MISMATCH" => {
                earlier.identity.baseline_schema += 1;
                comparable(earlier, &current)
            }
            "SCOPE_MISMATCH" => {
                earlier.identity.scope.kind = "different-scope".to_owned();
                comparable(earlier, &current)
            }
            _ => incomparable(reason),
        };
        let value = json(current.clone(), Some(baseline));
        assert_eq!(value["baseline"]["status"], "incomparable");
        assert_eq!(
            value["baseline"]["incomparableReasons"],
            serde_json::json!([reason])
        );
        assert_eq!(value["regression"], "unknown");
        assert_eq!(value["observation"], "findings");
        assert_eq!(value["violations"][0]["ruleId"], "b-to-a");
        assert_eq!(
            value["cycleGroups"][0]["members"],
            serde_json::json!(["a", "b"])
        );
        for key in ["counts", "removedViolations", "removedCycleGroups"] {
            assert!(value.get(key).is_none(), "{reason}: {key} should be absent");
        }
        assert!(value["violations"][0].get("class").is_none());
        assert!(value["violations"][0].get("baselineWeight").is_none());
        assert!(value["cycleGroups"][0].get("class").is_none());
    }

    let mut stale = current.clone();
    stale.identity.input_completeness =
        InputCompleteness::incomplete(vec!["CONTENT_DRIFT".to_owned()]);
    let comparison = comparable(current, &stale);
    let value = json(stale, Some(comparison));
    assert_eq!(
        value["baseline"]["incomparableReasons"],
        serde_json::json!(["CURRENT_INCOMPLETE"])
    );
    assert!(value.get("violations").is_none());
}

#[test]
fn architecture_envelope_arrays_sorted_bytewise() {
    let edges = [
        FORWARD,
        REVERSE,
        ("src/b/A.rs", "src/a/x.rs", 4),
        ("src/b/z.rs", "src/a/x.rs", 9),
    ];
    let mut flipped = edges;
    flipped.reverse();
    let a = serde_json::to_vec(&envelope(edge_set(&edges), None)).unwrap();
    let b = serde_json::to_vec(&envelope(edge_set(&flipped), None)).unwrap();
    assert_eq!(a, b);
    let value = json(edge_set(&edges), None);
    assert_eq!(value["violations"][0]["srcPath"], "src/b/A.rs");
    assert_eq!(value["violations"][2]["srcPath"], "src/b/z.rs");
    assert_eq!(
        value["cycleGroups"][0]["componentEdges"][1]["witnesses"][0]["srcPath"],
        "src/b/A.rs"
    );
}

#[test]
fn architecture_envelope_has_no_forbidden_keys() {
    fn check(value: &Value) {
        for key in [
            "ok",
            "clean",
            "passed",
            "noRegression",
            "compliant",
            "regressed",
        ] {
            assert!(value.get(key).is_none(), "unexpected key {key}");
        }
        if let Some(object) = value.as_object() {
            object.values().for_each(check);
        }
        if let Some(array) = value.as_array() {
            array.iter().for_each(check);
        }
    }

    let current = edge_set(&[FORWARD, REVERSE]);
    check(&json(current.clone(), None));
    check(&json(
        current.clone(),
        Some(comparable(current.clone(), &current)),
    ));
    check(&json(current, Some(incomparable("BASELINE_CORRUPT"))));
}

#[test]
fn architecture_envelope_evidence_qualification_is_constant_unqualified() {
    let mut current = edge_set(&[]);
    current.identity.index_head_hash = None;
    current.identity.input_completeness =
        InputCompleteness::incomplete(vec!["INDEX_EMPTY".to_owned()]);
    let value = json(current, None);
    assert_eq!(value["evidence"]["evidenceQualification"], "unqualified");
    assert!(value["evidence"].get("indexHeadHash").is_none());
}

#[test]
fn architecture_envelope_baseline_weight_absent_for_new() {
    let current = edge_set(&[FORWARD, REVERSE]);
    let baseline = comparable(edge_set(&[FORWARD]), &current);
    let value = json(current, Some(baseline));
    assert_eq!(value["violations"][0]["class"], "new");
    assert!(value["violations"][0].get("baselineWeight").is_none());
}

#[test]
fn architecture_human_summary_never_prints_pass_words() {
    let current = edge_set(&[FORWARD, REVERSE]);
    let views = [
        envelope(edge_set(&[FORWARD]), None),
        envelope(current.clone(), Some(comparable(current.clone(), &current))),
        envelope(current, Some(incomparable("BASELINE_CORRUPT"))),
    ];
    for view in views {
        let text = render_human(&view);
        assert!(text.contains("Evidence qualification unqualified"));
        assert!(text.contains("Observation "));
        assert!(text.contains("Regression "));
        for token in text.split(|character: char| !character.is_ascii_alphanumeric()) {
            assert!(!matches!(
                token.to_ascii_lowercase().as_str(),
                "clean" | "passed" | "compliant" | "ok"
            ));
        }
    }
}

#[test]
fn architecture_fail_decision_matrix() {
    for (observation, no_baseline_fails) in [
        (Observation::Findings, true),
        (Observation::Unknown, true),
        (Observation::NoneObserved, false),
    ] {
        for (regression, with_baseline_fails) in [
            (Regression::Present, true),
            (Regression::Unknown, true),
            (Regression::None, false),
            (Regression::NotEvaluated, true),
        ] {
            assert_eq!(
                fail_requested(false, regression, observation),
                no_baseline_fails
            );
            assert_eq!(
                fail_requested(true, regression, observation),
                with_baseline_fails
            );
        }
    }
}

#[test]
fn architecture_reader_missing_default_path_is_not_found() {
    let repo = tempfile::tempdir().unwrap();
    let error = read_config(repo.path(), None).err().unwrap().to_string();
    assert!(error.contains("ARCHITECTURE_CONFIG_NOT_FOUND"));
    assert!(error.contains("architecture.toml"));
}

#[test]
fn architecture_reader_missing_explicit_path_is_not_found() {
    let repo = tempfile::tempdir().unwrap();
    let missing = repo.path().join("missing.toml");
    let error = read_config(repo.path(), Some(&missing))
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("ARCHITECTURE_CONFIG_NOT_FOUND"));
    assert!(error.contains("missing.toml"));
}

#[test]
fn architecture_reader_explicit_path_reports_trusted_source() {
    let repo = tempfile::tempdir().unwrap();
    let config_path = repo.path().join("architecture.toml");
    std::fs::write(&config_path, COMPONENTS).unwrap();
    let loaded = read_config(repo.path(), Some(&config_path)).unwrap();
    assert_eq!(loaded.metadata.source, ConfigSource::TrustedPath);
    assert_eq!(loaded.metadata.path, config_path.to_str().unwrap());
    assert_eq!(loaded.metadata.digest, config_digest(COMPONENTS.as_bytes()));
}

#[test]
fn architecture_reader_local_path_reports_local_source_and_relative_path() {
    let repo = tempfile::tempdir().unwrap();
    let config_path = repo.path().join(DEFAULT_CONFIG_REL);
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, COMPONENTS).unwrap();
    let loaded = read_config(repo.path(), None).unwrap();
    assert_eq!(loaded.metadata.source, ConfigSource::Local);
    assert_eq!(loaded.metadata.path, ".ledgerful/architecture.toml");
    assert_eq!(loaded.metadata.component_count, 3);
    assert_eq!(loaded.metadata.rule_count, 0);
}
