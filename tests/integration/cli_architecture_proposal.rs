//! Normal CLI subprocesses for the pinned real-source proposal oracle.
#![cfg(any(
    windows,
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]

use crate::common::isolated_command;
use rstest::rstest;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{collections::BTreeMap, fs};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_ledgerful");
const POLICY: &str = ".ledgerful/architecture.toml";
const DB: &str = ".ledgerful/state/ledger.db";
const ORACLE_DIGEST: &str = "f1a202285ba15d1731b9c5da6e8fe64a0606999195344c591475428d2e1fd65f";

fn oracle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/architecture-proposal")
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn fixture_bytes() -> BTreeMap<String, Vec<u8>> {
    let root = oracle_root();
    let manifest = fs::read(root.join("manifest.json")).unwrap();
    assert_eq!(sha256(&manifest), ORACLE_DIGEST);
    let manifest: Value = serde_json::from_slice(&manifest).unwrap();
    let files = manifest["files"].as_object().unwrap();
    assert_eq!(files.len(), 25);
    files
        .iter()
        .map(|(path, pin)| {
            let raw = fs::read(root.join(path)).unwrap();
            assert_eq!(Value::from(sha256(&raw)), pin["sha256"], "{path}");
            assert_eq!(Value::from(raw.len()), pin["bytes"], "{path}");
            (path.clone(), raw)
        })
        .collect()
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    profile: PathBuf,
    inputs: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(temp.path()).unwrap();
        let root = base.join("proposal fixture repository");
        let profile = base.join("profile");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&profile).unwrap();
        let fixture = Self {
            _temp: temp,
            root,
            profile,
            inputs: fixture_bytes(),
        };
        fixture.git(&["init", "--initial-branch=main"]);
        fixture.git(&["config", "user.email", "ledgerful-fixture@example.test"]);
        fixture.git(&["config", "user.name", "Ledgerful Fixture"]);
        fixture.git(&["config", "core.autocrlf", "false"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.write(".gitignore", b".ledgerful/\n");
        fixture.overlay("base/");
        fixture
    }

    fn command(&self, bin: &str) -> Command {
        let mut command = isolated_command(bin, &self.profile);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let candidate = Path::new(BIN).parent().unwrap().to_path_buf();
        command
            .current_dir(&self.root)
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(candidate).chain(std::env::split_paths(&path)),
                )
                .unwrap(),
            )
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_SYSTEM")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LEDGERFUL_NON_INTERACTIVE", "1");
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self
            .command("git")
            .env("GIT_AUTHOR_DATE", "2025-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2025-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert_exit(&output, 0);
        String::from_utf8(output.stdout).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command(BIN).args(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert_exit(&output, 0);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn write(&self, file: &str, contents: &[u8]) {
        let path = self.root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn overlay(&self, prefix: &str) {
        let mut count = 0;
        for (file, contents) in &self.inputs {
            if let Some(path) = file.strip_prefix(prefix) {
                self.write(
                    if path == "architecture.toml" {
                        POLICY
                    } else {
                        path
                    },
                    contents,
                );
                count += 1;
            }
        }
        assert!(count > 0, "unpinned overlay {prefix}");
    }

    fn commit(&self, name: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", name]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn index_initial(&self, path: &Path) -> String {
        let head = self.commit("oracle pre-edit");
        let path = path.to_str().unwrap();
        assert_exit(&self.cli(&["-C", path, "init"]), 0);
        assert_exit(&self.cli(&["-C", path, "index", "--full"]), 0);
        head
    }

    fn packet(&self, from: &str, to: &str, head: &str, token: Option<&str>) -> Value {
        self.packet_with(from, to, head, token, |_| {})
    }

    fn packet_with(
        &self,
        from: &str,
        to: &str,
        head: &str,
        token: Option<&str>,
        configure: impl FnOnce(&mut Command),
    ) -> Value {
        let mut command = self.command(BIN);
        command.env("RUST_LOG", "trace");
        configure(&mut command);
        command.args([
            "architecture",
            "propose",
            "--from",
            from,
            "--to",
            to,
            "--kind",
            "fileCall",
            "--head",
            head,
        ]);
        if let Some(token) = token {
            command.args(["--context", token]);
        }
        let output = command.output().unwrap();
        assert_exit(&output, 0);
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let packet: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(packet["kind"], "architectureProposal");
        assert_eq!(packet["schemaVersion"], 1);
        assert_eq!(packet["permission"], "unknown");
        check_packet_keys(&packet);
        packet
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, (Option<String>, Option<std::time::SystemTime>)> {
        fn walk(
            path: &Path,
            all: &mut BTreeMap<PathBuf, (Option<String>, Option<std::time::SystemTime>)>,
        ) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    all.insert(entry.path(), (None, None));
                    walk(&entry.path(), all);
                } else {
                    all.insert(
                        entry.path(),
                        (
                            Some(sha256(&fs::read(entry.path()).unwrap())),
                            metadata.modified().ok(),
                        ),
                    );
                }
            }
        }
        let mut result = BTreeMap::new();
        walk(&self.root, &mut result);
        walk(&self.profile, &mut result);
        result
    }
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn check_packet_keys(packet: &Value) {
    match packet {
        Value::Object(fields) => {
            for (name, value) in fields {
                assert!(
                    !["allowed", "permitted", "ok", "safe", "approved", "clean"]
                        .contains(&name.as_str())
                );
                check_packet_keys(value);
            }
        }
        Value::Array(items) => items.iter().for_each(check_packet_keys),
        _ => {}
    }
}

fn refuse(packet: &Value, reason: &str) {
    assert_eq!(packet["status"], "unknown", "{packet}");
    assert_eq!(packet["reasonCodes"], json!([reason]), "{packet}");
    assert!(packet.get("observedContext").is_none());
    assert!(packet.get("ruleDecision").is_none());
    assert!(packet.get("cycleEffect").is_none());
}

fn prepared(packet: &Value) -> &str {
    assert_eq!(packet["status"], "unknown", "{packet}");
    assert_eq!(
        packet["reasonCodes"],
        json!(["CONTEXT_REQUIRED"]),
        "{packet}"
    );
    assert_eq!(
        packet["observedContext"]["head"],
        packet["observedContext"]["indexHead"]
    );
    assert!(packet.get("ruleDecision").is_none());
    assert!(packet.get("cycleEffect").is_none());
    packet["observedContext"]["token"].as_str().unwrap()
}

#[cfg(feature = "mcp")]
fn assert_mcp_parity(
    fixture: &Fixture,
    from: &str,
    to: &str,
    head: &str,
    prepare: &Value,
    evaluated: Option<&Value>,
) {
    use crate::mcp_server::tests::structured_harness::{McpSession, assert_structured_success};
    let mut command = fixture.command(BIN);
    command.arg("mcp");
    let mut session = McpSession::spawn_command(&mut command, Default::default());
    session.initialize();
    let request = json!({ "from": from, "to": to, "kind": "fileCall", "head": head });
    assert_eq!(
        &assert_structured_success(&session.call_tool("architecture_propose", request.clone())),
        prepare
    );
    if let Some(evaluated) = evaluated {
        let mut request = request;
        request["context"] = json!(prepared(prepare));
        assert_eq!(
            &assert_structured_success(&session.call_tool("architecture_propose", request)),
            evaluated
        );
    }
    assert!(session.finish().is_empty());
}

struct ExpectedProjection {
    rule: (&'static str, Option<&'static str>),
    cycle: (&'static str, &'static [&'static str]),
    weight: u32,
}

#[rstest]
#[case::introduced__slow("e-to-d", "", "e", "d", ExpectedProjection {
    rule: ("unconstrained", None), cycle: ("introduced", &["d", "e"]), weight: 1
})]
#[case::grown__slow("c-to-a", "", "c", "a", ExpectedProjection {
    rule: ("forbid", Some("forbid-ca")), cycle: ("grown", &["a", "b", "c"]), weight: 1
})]
#[case::no_change__slow("a-to-d", "", "a", "d", ExpectedProjection {
    rule: ("allow", Some("allow-ad")), cycle: ("noChangeObserved", &[]), weight: 1
})]
#[case::duplicate__slow("a-to-b-again", "", "a", "b", ExpectedProjection {
    rule: ("unconstrained", None), cycle: ("noChangeObserved", &[]), weight: 2
})]
#[case::tie__slow("c-to-a", "tie", "c", "a", ExpectedProjection {
    rule: ("forbid", Some("forbid-to-a")), cycle: ("grown", &["a", "b", "c"]), weight: 1
})]
#[case::exact__slow("c-to-a", "exact", "c", "a", ExpectedProjection {
    rule: ("allow", Some("allow-ca")), cycle: ("grown", &["a", "b", "c"]), weight: 1
})]
#[allow(non_snake_case)]
fn architecture_proposal_matches_independent_post_edit(
    #[case] edit: &str,
    #[case] policy: &str,
    #[case] source: &str,
    #[case] target: &str,
    #[case] expected: ExpectedProjection,
) {
    let ExpectedProjection {
        rule: (rule, rule_id),
        cycle: (effect, members),
        weight: projected_weight,
    } = expected;
    let fixture = Fixture::new();
    if !policy.is_empty() {
        fixture.overlay(&format!("policy/{policy}/"));
    }
    let head = fixture.index_initial(&fixture.root);
    let baseline = fixture.json(&["baseline", "create", "oracle_base", "--json"]);
    assert_eq!(
        baseline["identity"]["inputCompleteness"]["state"],
        "COMPLETE"
    );
    assert_eq!(baseline["identity"]["edgeSources"], json!(["native"]));
    let original = fixture.json(&["architecture", "check", "--json"]);
    assert_eq!(original["evidence"]["coverage"]["edgesAmbiguous"], 0);
    assert_eq!(original["evidence"]["coverage"]["edgesUnresolved"], 0);
    assert_eq!(original["mapping"]["crossComponentEdges"], 4);
    assert_eq!(original["mapping"]["unmappedEdges"], 0);
    assert_eq!(original["mapping"]["ambiguousMappingEdges"], 0);
    assert_eq!(original["cycleGroups"][0]["members"], json!(["a", "b"]));
    let (from, to) = (format!("src/{source}.rs"), format!("src/{target}.rs"));
    let snapshot = fixture.snapshot();
    let prepare = fixture.packet(&from, &to, &head, None);
    let token = prepared(&prepare);
    assert_eq!(prepare, fixture.packet(&from, &to, &head, None));
    let evaluated = fixture.packet(&from, &to, &head, Some(token));
    #[cfg(feature = "mcp")]
    assert_mcp_parity(&fixture, &from, &to, &head, &prepare, Some(&evaluated));
    assert_eq!(evaluated["status"], "evaluated", "{evaluated}");
    assert_eq!(evaluated["sourceComponent"], source);
    assert_eq!(evaluated["targetComponent"], target);
    assert_eq!(evaluated["evidenceQualification"], "unqualified");
    assert_eq!(evaluated["ruleDecision"], rule);
    assert!(evaluated.get("ruleId").is_some());
    assert_eq!(evaluated["ruleId"], json!(rule_id));
    assert_eq!(evaluated["cycleEffect"], effect);
    assert_eq!(evaluated["reasonCodes"], json!(["UNQUALIFIED_EVIDENCE"]));
    if members.is_empty() {
        assert!(evaluated.get("members").is_none());
    } else {
        assert_eq!(evaluated["members"], json!(members));
    }
    assert_eq!(
        snapshot,
        fixture.snapshot(),
        "proposal changed repository or profile bytes"
    );
    fixture.overlay(&format!("edits/{edit}/"));
    let post_head = fixture.commit("preregistered actual edit");
    assert_ne!(post_head, head);
    assert_exit(&fixture.cli(&["index", "--full"]), 0);
    let baseline = fixture.json(&["baseline", "compare", "oracle_base", "--json"]);
    assert_eq!(baseline["status"], "comparable");
    assert_eq!(baseline["currentIdentity"]["indexHeadHash"], post_head);
    assert_eq!(
        baseline["items"],
        json!([{
            "edgeKey": {"srcPath": from, "dstPath": to},
            "class": if projected_weight == 2 {"worsened"} else {"added"},
            "baselineWeight": if projected_weight == 2 {Some(1)} else {None},
            "currentWeight": projected_weight,
        }])
    );
    let actual = fixture.json(&[
        "architecture",
        "check",
        "--baseline",
        "oracle_base",
        "--json",
    ]);
    let coverage = &actual["evidence"]["coverage"];
    assert_eq!(coverage["edgesAmbiguous"], 0);
    assert_eq!(coverage["edgesUnresolved"], 0);
    assert_eq!(coverage["edgesResolvedCrossFile"], 5);
    let groups = actual["cycleGroups"].as_array().unwrap();
    let actual_group = groups
        .iter()
        .find(|group| matches!(group["class"].as_str(), Some("introduced" | "grown")));
    let actual_scc = if members.is_empty() {
        assert!(actual_group.is_none());
        assert_eq!(groups[0]["members"], json!(["a", "b"]));
        assert_eq!(groups[0]["class"], "unchanged");
        &groups[0]
    } else {
        let group = actual_group.unwrap();
        assert_eq!(group["members"], json!(members));
        assert_eq!(group["class"], effect);
        group
    };
    let scc_pairs: Vec<_> = actual_scc["componentEdges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair["src"].as_str().unwrap(),
                pair["dst"].as_str().unwrap(),
                pair["weight"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected_scc = match effect {
        "introduced" => vec![("d", "e", 1), ("e", "d", 1)],
        "grown" => vec![("a", "b", 1), ("b", "a", 1), ("b", "c", 1), ("c", "a", 1)],
        _ => vec![("a", "b", u64::from(projected_weight)), ("b", "a", 1)],
    };
    assert_eq!(scc_pairs, expected_scc);
    let actual_violation = actual["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["srcPath"] == from && violation["dstPath"] == to);
    if rule == "forbid" {
        let row = actual_violation.unwrap();
        assert_eq!(row["ruleId"], json!(rule_id));
        assert_eq!(row["class"], "new");
        assert_eq!(row["weight"], projected_weight);
    } else {
        assert!(actual_violation.is_none());
    }
}

#[rstest]
#[case::unmapped__slow("all-unmapped", "a", "b", &["NO_MAPPED_CROSS_COMPONENT_EDGES", "UNMAPPED_COMPONENTS", "UNQUALIFIED_EVIDENCE"])]
#[case::no_cross_edge__slow("intra-only", "a", "d", &["NO_MAPPED_CROSS_COMPONENT_EDGES", "UNQUALIFIED_EVIDENCE"])]
#[case::unresolved__slow("unresolved", "a", "d", &["UNQUALIFIED_EVIDENCE", "UNRESOLVED_PRESENT"])]
#[allow(non_snake_case)]
fn architecture_proposal_observed_negative_controls(
    #[case] control: &str,
    #[case] source: &str,
    #[case] target: &str,
    #[case] codes: &[&str],
) {
    let fixture = Fixture::new();
    fixture.overlay(&format!("controls/{control}/"));
    let head = fixture.index_initial(&fixture.root);
    let (from, to) = (format!("src/{source}.rs"), format!("src/{target}.rs"));
    let prepare = fixture.packet(&from, &to, &head, None);
    let replay = fixture.packet(&from, &to, &head, Some(prepared(&prepare)));
    #[cfg(feature = "mcp")]
    assert_mcp_parity(&fixture, &from, &to, &head, &prepare, Some(&replay));
    assert_eq!(replay["status"], "evaluated", "{replay}");
    assert_eq!(replay["cycleEffect"], "unknown");
    assert_eq!(replay["reasonCodes"], json!(codes));
    assert!(replay.get("members").is_none());
}

#[test]
#[allow(non_snake_case)]
fn architecture_proposal_rejects_unsupported_source_without_a_projection__slow() {
    let fixture = Fixture::new();
    let head = fixture.index_initial(&fixture.root);
    fixture.json(&["baseline", "create", "oracle_base", "--json"]);
    let unsupported = fixture.packet("notes.txt", "src/a.rs", &head, None);
    refuse(&unsupported, "ENDPOINT_UNKNOWN");
    #[cfg(feature = "mcp")]
    assert_mcp_parity(&fixture, "notes.txt", "src/a.rs", &head, &unsupported, None);
    fixture.overlay("edits/unsupported/");
    fixture.commit("preregistered unsupported source");
    assert_exit(&fixture.cli(&["index", "--full"]), 0);
    let delta = fixture.json(&["baseline", "compare", "oracle_base", "--json"]);
    assert_eq!(delta["items"], json!([]));
    assert_eq!(delta["counts"]["unchanged"], 4);
}

#[test]
#[allow(non_snake_case)]
fn architecture_proposal_refuses_stale_context_and_unsafe_storage__slow() {
    let fixture = Fixture::new();
    let head = fixture.index_initial(&fixture.root);
    let (from, to) = ("src/a.rs", "src/d.rs");
    let prepare = fixture.packet(from, to, &head, None);
    let token = prepared(&prepare);
    refuse(
        &fixture.packet(to, from, &head, Some(token)),
        "CONTEXT_MISMATCH",
    );
    refuse(
        &fixture.packet("src/A.rs", to, &head, None),
        "ENDPOINT_UNKNOWN",
    );
    refuse(
        &fixture.packet(from, to, &"f".repeat(40), None),
        "HEAD_MISMATCH",
    );
    for redirect in [
        "",
        "../state",
        fixture.root.to_str().unwrap(),
        "custom-state",
    ] {
        refuse(
            &fixture.packet_with(from, to, &head, Some(token), |command| {
                command.env("LEDGERFUL_STATE_DIR", redirect);
            }),
            "STORAGE_UNAVAILABLE",
        );
    }
    let config = fs::read(fixture.root.join(POLICY)).unwrap();
    let mut changed = config.clone();
    changed.extend_from_slice(b"\n# Context change at the same HEAD\n");
    fixture.write(POLICY, &changed);
    refuse(
        &fixture.packet(from, to, &head, Some(token)),
        "CONTEXT_MISMATCH",
    );
    fixture.write(POLICY, &config);
    let dirty = b"pub fn a_target() {}\n";
    fixture.write(from, dirty);
    refuse(
        &fixture.packet(from, to, &head, Some(token)),
        "CONTENT_DRIFT",
    );
    fixture.write(from, &fixture.inputs["base/src/a.rs"]);
    fixture.write(&format!("{DB}-wal"), b"must never be consumed");
    fixture.write(&format!("{DB}-shm"), b"must never be changed");
    let snapshot = fixture.snapshot();
    refuse(
        &fixture.packet(from, to, &head, Some(token)),
        "STORAGE_UNAVAILABLE",
    );
    assert_eq!(snapshot, fixture.snapshot());
}

#[rstest]
#[case::removed__slow(false)]
#[case::masked__slow(true)]
#[allow(non_snake_case)]
fn architecture_proposal_requires_all_committed_sources_after_reindex(#[case] mask: bool) {
    let fixture = Fixture::new();
    let head = fixture.index_initial(&fixture.root);
    let prepared_packet = fixture.packet("src/a.rs", "src/d.rs", &head, None);
    let token = prepared(&prepared_packet);
    if mask {
        fixture.write(".gitignore", b".ledgerful/\nsrc/c.rs\n");
    } else {
        fs::remove_file(fixture.root.join("src/c.rs")).unwrap();
    }
    assert_exit(&fixture.cli(&["index", "--full"]), 0);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]).trim(), head);
    refuse(
        &fixture.packet("src/a.rs", "src/d.rs", &head, Some(token)),
        "CONTENT_DRIFT",
    );
}

#[test]
#[allow(non_snake_case)]
fn architecture_proposal_admits_only_proved_committed_ignore_exclusion__slow() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", b".ledgerful/\nsrc/excluded.rs\n");
    fixture.write("src/excluded.rs", b"pub fn exclude_me() {}\n");
    fixture.git(&["add", "--force", "src/excluded.rs"]);
    let head = fixture.index_initial(&fixture.root);
    let (from, to) = ("src/a.rs", "src/d.rs");
    let prepare = fixture.packet(from, to, &head, None);
    let token = prepared(&prepare);
    assert_eq!(
        fixture.packet(from, to, &head, Some(token))["status"],
        "evaluated"
    );

    // Unmasking only the live policy and re-indexing cannot introduce a
    // committed source that HEAD's policy excluded from the baseline.
    fixture.write(".gitignore", b".ledgerful/\n");
    assert_exit(&fixture.cli(&["index", "--full"]), 0);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]).trim(), head);
    refuse(
        &fixture.packet(from, to, &head, Some(token)),
        "CONTENT_DRIFT",
    );
    fixture.write(".gitignore", b".ledgerful/\nsrc/excluded.rs\n");

    // A new nearer policy reintroduces the path into the intended source
    // scope. With that file missing, full indexing cannot establish coverage.
    fixture.write("src/.ignore", b"!excluded.rs\n");
    fs::remove_file(fixture.root.join("src/excluded.rs")).unwrap();
    assert_exit(&fixture.cli(&["index", "--full"]), 0);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]).trim(), head);
    refuse(
        &fixture.packet(from, to, &head, Some(token)),
        "CONTENT_DRIFT",
    );
}

#[test]
#[allow(non_snake_case)]
fn architecture_proposal_binds_same_head_across_distinct_loose_and_packed_checkouts__slow() {
    let first = Fixture::new();
    let second = Fixture::new();
    let head = first.index_initial(&first.root);
    assert_eq!(second.index_initial(&second.root), head);
    second.git(&["repack", "-a", "-d"]);
    second.git(&["multi-pack-index", "write"]);
    assert!(
        second
            .root
            .join(".git/objects/pack/multi-pack-index")
            .is_file()
    );
    let (from, to) = ("src/a.rs", "src/d.rs");
    let first_prepare = first.packet(from, to, &head, None);
    let second_prepare = second.packet(from, to, &head, None);
    assert_eq!(
        first_prepare["observedContext"]["inputDigest"],
        second_prepare["observedContext"]["inputDigest"]
    );
    assert_ne!(prepared(&first_prepare), prepared(&second_prepare));
    refuse(
        &second.packet(from, to, &head, Some(prepared(&first_prepare))),
        "CONTEXT_MISMATCH",
    );
    assert_eq!(
        second.packet(from, to, &head, Some(prepared(&second_prepare)))["status"],
        "evaluated"
    );
}

#[test]
fn architecture_proposal_rejects_bad_ingress_before_repository_access() {
    let fixture = Fixture::new();
    let head = "f".repeat(40);
    let before = fixture.snapshot();
    let mut unsupported = fixture.command(BIN);
    unsupported
        .current_dir(&fixture.profile)
        .args([
            "architecture",
            "propose",
            "--from",
            "src/a.rs",
            "--to",
            "src/b.rs",
            "--kind",
            "symbolCall",
            "--head",
            &head,
        ])
        .env("LEDGERFUL_STATE_DIR", "invalid_state_tripwire");
    let output = unsupported.output().unwrap();
    assert_exit(&output, 0);
    assert!(output.stderr.is_empty());
    refuse(
        &serde_json::from_slice(&output.stdout).unwrap(),
        "UNSUPPORTED_KIND",
    );
    let bad = fixture.cli(&[
        "architecture",
        "propose",
        "--from",
        "../redact-this-input",
        "--to",
        "src/b.rs",
        "--kind",
        "fileCall",
        "--head",
        "abc",
        "--json",
    ]);
    assert!(!bad.status.success());
    assert!(bad.stdout.is_empty());
    let diagnostic = String::from_utf8(bad.stderr).unwrap();
    assert!(
        diagnostic.contains("ARCHITECTURE_PROPOSAL_INVALID_REQUEST"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("redact-this-input"), "{diagnostic}");
    assert_eq!(before, fixture.snapshot());
}

#[cfg(windows)]
#[test]
#[allow(non_snake_case)]
fn architecture_proposal_native_windows_short_alias__slow() {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let required = std::env::var_os("LEDGERFUL_TEST_REQUIRE_WINDOWS_SHORT_ALIAS").is_some();
    let fixture = Fixture::new();
    let path: Vec<u16> = fixture
        .root
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut short = [0_u16; 4096];
    // SAFETY: both buffers are valid for the API's synchronous call. The input
    // is NUL-terminated and the output capacity exactly matches the slice.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let len = unsafe { GetShortPathNameW(path.as_ptr(), short.as_mut_ptr(), short.len() as u32) }
        as usize;
    let alias = (0 < len && len < short.len())
        .then(|| PathBuf::from(std::ffi::OsString::from_wide(&short[..len])))
        .filter(|alias| {
            alias != &fixture.root
                && alias.components().any(|part| {
                    part.as_os_str()
                        .to_string_lossy()
                        .rsplit_once('~')
                        .is_some_and(|(_, tail)| {
                            tail.split('.').next().is_some_and(|tail| {
                                !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())
                            })
                        })
                })
        });
    let Some(alias) = alias else {
        assert!(
            !required,
            "hosted short-folder witness requires a real 8.3 name for {:?}",
            fixture.root
        );
        eprintln!("8.3 folder names are unavailable on this local test volume");
        return;
    };
    assert_eq!(dunce::canonicalize(&alias).unwrap(), fixture.root);
    let head = fixture.index_initial(&alias);
    let (from, to) = ("src/a.rs", "src/d.rs");
    let before = fixture.snapshot();
    let prepare = fixture.packet(from, to, &head, None);
    let replay = fixture.packet(from, to, &head, Some(prepared(&prepare)));
    assert_eq!(replay["ruleDecision"], "allow", "{replay}");
    assert_eq!(replay["ruleId"], "allow-ad");
    assert_eq!(replay["cycleEffect"], "noChangeObserved");
    let refused = fixture.packet_with(from, to, &head, None, |command| {
        command.args(["-C", alias.to_str().unwrap()]);
    });
    refuse(&refused, "STORAGE_UNAVAILABLE");
    assert_eq!(before, fixture.snapshot());
    println!(
        "Windows 8.3 witness alias={} canonical={} same_object=true init_exit=0 index_exit=0 canonical_prepare_exit=0 canonical_replay_exit=0 alias_proposal_exit=0 alias_reason=STORAGE_UNAVAILABLE",
        alias.display(),
        fixture.root.display()
    );
}
