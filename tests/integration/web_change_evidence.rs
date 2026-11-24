//! Native, real-daemon change-evidence transport qualification.
#![cfg(all(feature = "web", any(windows, target_os = "linux")))]

use crate::common::{isolated_command, nested_repo_root};
use ledgerful::commands::web::auth::generate_token;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_ledgerful");
const UNKNOWN_SCOPE: &str = "00000000-0000-0000-0000-00000000002a";

struct Checkout {
    temp: tempfile::TempDir,
    root: PathBuf,
}

impl Checkout {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("private fixture");
        let root = nested_repo_root(temp.path());
        fs::create_dir(temp.path().join("home")).expect("fixture home");
        fs::create_dir(temp.path().join("template")).expect("empty Git template");
        fs::write(
            temp.path().join("home/.gitconfig"),
            "[user]\nname = Evidence Fixture\nemail = fixture@example.invalid\n",
        )
        .expect("local synthetic Git home");
        let checkout = Self { temp, root };
        let template = format!(
            "--template={}",
            checkout.temp.path().join("template").display()
        );
        checkout.git(&[
            "init",
            "--quiet",
            "-b",
            "main",
            "--object-format=sha1",
            &template,
        ]);
        checkout.git(&["config", "core.autocrlf", "false"]);
        checkout.write(".gitignore", ".ledgerful/\n.ignore\n");
        checkout
    }

    fn command(&self, program: &str) -> Command {
        let home = self.temp.path().join("home");
        let mut command = isolated_command(program, &home);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("GIT_") || name == "XDG_CONFIG_HOME" {
                command.env_remove(key);
            }
        }
        command
            .current_dir(&self.root)
            .env("LEDGERFUL_CONFIG_HOME", home.join("ledgerful-config"))
            .env("LEDGERFUL_NO_NETWORK", "1")
            .env("LEDGERFUL_NON_INTERACTIVE", "1")
            .env("GIT_AUTHOR_NAME", "Evidence Fixture")
            .env("GIT_COMMITTER_NAME", "Evidence Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        command
    }

    fn git(&self, args: &[&str]) -> String {
        Self::run(self.command("git"), args, 0)
    }

    fn cli(&self, args: &[&str], exit: i32) -> String {
        Self::run(self.command(BIN), args, exit)
    }

    fn cli_json(&self, args: &[&str], exit: i32) -> Value {
        serde_json::from_str(&self.cli(args, exit)).expect("CLI JSON")
    }

    fn run(mut command: Command, args: &[&str], expected: i32) -> String {
        let output = command.args(args).output().expect("run fixture command");
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("fixture command emits UTF-8")
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture path parent"))
            .expect("fixture directories");
        fs::write(path, contents).expect("write fixture input");
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "--", "."]);
        let hooks = format!(
            "core.hooksPath={}",
            self.temp.path().join("template").display()
        );
        self.git(&[
            "-c",
            &hooks,
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }
}

/// Use the public foreground startup so state origin and peer information come
/// from production. Port zero avoids a reserve-and-release loopback race.
struct Daemon<'a> {
    child: Child,
    repo: &'a Path,
    stdout: Option<thread::JoinHandle<()>>,
    addr: SocketAddr,
    token: String,
    http: ureq::Agent,
}

struct Packet {
    status: u16,
    cache_control: Option<String>,
    body: String,
}

impl Daemon<'_> {
    fn start(checkout: &Checkout, disabled: bool) -> Daemon<'_> {
        let token = generate_token();
        let mut command = checkout.command(BIN);
        command
            .args(["web", "start", "--bind", "127.0.0.1", "--port", "0"])
            .env("LEDGERFUL_WEB_TOKEN", &token)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                File::create(checkout.temp.path().join("daemon.stderr")).expect("daemon log"),
            ));
        if disabled {
            command.arg("--disable-change-evidence");
        }
        let mut daemon = Daemon {
            child: command.spawn().expect("spawn fixture daemon"),
            repo: &checkout.root,
            stdout: None,
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            token,
            http: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build(),
        };
        let stdout = daemon.child.stdout.take().expect("daemon stdout");
        let (send, ready) = mpsc::channel();
        daemon.stdout = Some(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(addr) = line.strip_prefix("ledgerful web listening on ") {
                    let _ = send.send(addr.parse::<SocketAddr>());
                }
            }
        }));
        daemon.addr = ready
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| {
                let log = fs::read_to_string(checkout.temp.path().join("daemon.stderr"));
                panic!("fixture daemon failed to bind: {error}, {log:?}");
            })
            .expect("valid loopback address from daemon");
        assert_eq!(daemon.addr.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert_ne!(daemon.addr.port(), 0);
        daemon
    }

    fn get(&self, path: &str, authenticated: bool, origin: Option<&str>) -> Packet {
        let mut request = self.http.get(&format!("http://{}{path}", self.addr));
        if authenticated {
            request = request.set("Authorization", &format!("Bearer {}", self.token));
        }
        if let Some(origin) = origin {
            request = request.set("Origin", origin);
        }
        let response = match request.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => panic!("loopback request failed: {error}"),
        };
        Packet {
            status: response.status(),
            cache_control: response.header("Cache-Control").map(str::to_owned),
            body: response.into_string().expect("HTTP body"),
        }
    }
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        // The stop CLI can run its own startup against absent or old stores.
        // Terminate only this disposable child when observing route passivity.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        let _ = fs::remove_file(self.repo.join(".ledgerful/tmp/web.pid"));
    }
}

impl Packet {
    fn evidence(&self, status: u16) -> Value {
        assert_eq!(self.status, status, "{}", self.body);
        assert_eq!(self.cache_control.as_deref(), Some("no-store"));
        assert!(self.body.len() <= 256 * 1024);
        serde_json::from_str(&self.body).expect("evidence JSON")
    }

    fn problem(&self, status: u16, reason: &str) {
        let body = self.evidence(status);
        assert_eq!(body["reasonCode"], reason);
        assert!(body.get("repoScope").is_none());
        assert!(body.get("verification").is_none());
    }
}

fn query(scope: &str, head: &str, base: &str) -> String {
    format!("/api/change-evidence?repoScope={scope}&head={head}&base={base}")
}

#[test]
fn protected_change_evidence_does_not_create_a_missing_store() {
    let checkout = Checkout::new();
    checkout.write("sample.rs", "pub fn sample() -> u32 { 1 }\n");
    let base = checkout.commit("parent");
    checkout.write("sample.rs", "pub fn sample() -> u32 { 2 }\n");
    let head = checkout.commit("head");
    let sqlite = checkout.root.join(".ledgerful/state");
    fs::create_dir_all(&sqlite).expect("empty local state path");
    let key = query(UNKNOWN_SCOPE, &head, &base);
    {
        let daemon = Daemon::start(&checkout, false);
        daemon.get(&key, false, None).evidence(403);
        daemon
            .get(&key, true, Some("https://foreign.invalid"))
            .evidence(403);
        let invalid = daemon.get("/api/change-evidence?private-query=hidden", true, None);
        invalid.problem(400, "invalid-query");
        assert!(!invalid.body.contains("private-query"));
        for _ in 0..2 {
            daemon.get(&key, true, None).problem(503, "uninitialized");
        }
        let legacy = daemon.get("/api/session", true, None);
        assert_eq!(legacy.status, 200);
    }
    assert_eq!(fs::read_dir(&sqlite).expect("state listing").count(), 0);
    {
        let daemon = Daemon::start(&checkout, true);
        daemon.get(&key, true, None).problem(503, "disabled");
    }
    assert_eq!(fs::read_dir(&sqlite).expect("state listing").count(), 0);
}

#[test]
#[allow(non_snake_case)]
fn ordinary_prepared_pair_transitions_from_incomparable_to_comparable__slow() {
    let checkout = Checkout::new();
    checkout.write("src/lib.rs", "mod target;\nmod calls;\nmod remove;\n");
    checkout.write("src/target.rs", "pub fn evidence_target() {}\n");
    checkout.write(
        "src/calls.rs",
        "pub fn evidence_first() { crate::target::evidence_target(); }\n",
    );
    checkout.write(
        "src/remove.rs",
        "pub fn evidence_removed() { crate::target::evidence_target(); }\n",
    );
    checkout.cli(&["init"], 0);
    let base = checkout.commit("baseline");
    checkout.cli(&["index", "--full"], 0);
    checkout.cli_json(&["baseline", "create", "parent", "--json"], 0);
    let attested = checkout.cli_json(
        &["change-evidence", "attest-baseline", "parent", "--json"],
        0,
    );
    assert_eq!(attested["status"], "attested");
    assert_eq!(attested["base"], base);
    let scope = attested["repoScope"].as_str().expect("attested scope");
    let digest = attested["baselineDigest"]
        .as_str()
        .expect("attested digest");

    checkout.write("src/lib.rs", "mod target;\nmod calls;\nmod add;\n");
    checkout.write(
        "src/calls.rs",
        "pub fn evidence_first() { crate::target::evidence_target(); crate::target::evidence_target(); }\n",
    );
    checkout.write(
        "src/add.rs",
        "pub fn evidence_added() { crate::target::evidence_target(); }\n",
    );
    fs::remove_file(checkout.root.join("src/remove.rs")).expect("fixture removes a tracked edge");
    let head = checkout.commit("changed graph");
    let bare = query(scope, &head, &base);
    let selected = format!("{bare}&baselineName=parent&baselineDigest={digest}");
    let negative = checkout.cli_json(&["change-evidence", "prepare", "parent", "--json"], 1);
    assert_eq!(negative["status"], "incomparable");
    {
        let daemon = Daemon::start(&checkout, false);
        let packet = daemon.get(&selected, true, None).evidence(200);
        let comparison = &packet["comparison"];
        assert_eq!(comparison["status"], "incomparable");
        assert!(
            comparison["incomparableReasons"]
                .as_array()
                .unwrap()
                .contains(&json!("CURRENT_INCOMPLETE"))
        );
        assert!(comparison.get("counts").is_none());
        assert!(comparison.get("items").is_none());
        assert_eq!(packet["verification"]["status"], "not-recorded");
    }
    checkout.cli(&["index", "--full"], 0);
    let prepared = checkout.cli_json(&["change-evidence", "prepare", "parent", "--json"], 0);
    assert_eq!(prepared["status"], "prepared");
    let captured = checkout.cli_json(
        &[
            "verify",
            "--capture-change-evidence",
            "--scope",
            "full",
            "--json",
            "git --version",
        ],
        0,
    );
    let ack = &captured["changeEvidenceCapture"];
    assert_eq!(ack["status"], "recorded");
    let daemon = Daemon::start(&checkout, false);
    let packet = daemon.get(&selected, true, None).evidence(200);
    assert_eq!(packet["head"], head);
    assert_eq!(packet["base"], base);
    assert_eq!(packet["repoScope"], scope);
    assert_eq!(packet["comparison"]["status"], "comparable");
    assert_eq!(
        packet["comparison"]["counts"],
        json!({
            "added": 1, "removed": 1, "worsened": 1, "improved": 0, "unchanged": 0,
        })
    );
    assert!(
        packet["comparison"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["edgeKey"] == json!({"srcPath": "src/calls.rs", "dstPath": "src/target.rs"})
                    && item["class"] == "worsened"
                    && item["baselineWeight"] == 1
                    && item["currentWeight"] == 2
            })
    );
    assert!(
        packet["verification"]["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|attempt| {
                attempt["attemptId"] == ack["attemptId"] && attempt["runId"] == ack["runId"]
            })
    );
    let no_baseline = daemon.get(&bare, true, None).evidence(200);
    assert_eq!(no_baseline["comparison"]["status"], "selection-required");
    assert_eq!(no_baseline["git"], packet["git"]);
    let wrong_digest = format!(
        "{bare}&baselineName=parent&baselineDigest=blake3:{}",
        "0".repeat(64)
    );
    let refused = daemon.get(&wrong_digest, true, None).evidence(200);
    assert_eq!(
        refused["comparison"],
        json!({"status": "unavailable", "reasonCode": "baseline-digest-mismatch"})
    );
    assert_eq!(refused["git"], packet["git"]);
    let foreign = daemon.get(&query(UNKNOWN_SCOPE, &head, &base), true, None);
    let wrong_parent = daemon.get(&query(scope, &head, &"1".repeat(40)), true, None);
    foreign.problem(404, "pair-not-found");
    wrong_parent.problem(404, "pair-not-found");
    assert_eq!(foreign.body, wrong_parent.body);
    let replacement = if head.ends_with('0') { '1' } else { '0' };
    let unknown_head = format!("{}{replacement}", &head[..head.len() - 1]);
    assert_eq!(&unknown_head[..8], &head[..8]);
    let same_prefix = daemon.get(&query(scope, &unknown_head, &base), true, None);
    same_prefix.problem(404, "pair-not-found");
    assert_eq!(foreign.body, same_prefix.body);

    let snapshot = daemon.get("/api/snapshot", true, None);
    assert_eq!(snapshot.status, 200, "{}", snapshot.body);
    let snapshot: Value = serde_json::from_str(&snapshot.body).expect("legacy snapshot");
    assert!(
        snapshot["recent_changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|change| { change.get("evidenceKey").is_none() })
    );

    drop(daemon);
    let db = checkout.root.join(".ledgerful/state/ledger.db");
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .expect("fixture index database already exists");
    let updated = conn
        .execute(
            "UPDATE project_files SET content_hash = ?1 WHERE file_path = 'src/calls.rs'",
            ["0".repeat(64)],
        )
        .expect("replace a fixture index hash while leaving the verified Git bytes in place");
    assert_eq!(updated, 1);
    drop(conn);
    let failed = checkout.cli_json(&["change-evidence", "prepare", "parent", "--json"], 1);
    assert_eq!(failed["status"], "unavailable");
    assert_eq!(failed["reasonCodes"], json!(["index-input-hash-mismatch"]));
    let daemon = Daemon::start(&checkout, false);
    let refused = daemon.get(&selected, true, None).evidence(200);
    assert_eq!(
        refused["comparison"],
        json!({"status": "unavailable", "reasonCode": "index-input-hash-mismatch"})
    );
}

#[test]
#[allow(non_snake_case)]
fn prepared_head_with_an_unsupported_name_preserves_safe_git_rows__slow() {
    let checkout = Checkout::new();
    checkout.write(
        "src/lib.rs",
        "mod other;\npub fn caller() { other::target(); }\n",
    );
    checkout.write("src/other.rs", "pub fn target() {}\n");
    checkout.cli(&["init"], 0);
    let base = checkout.commit("baseline");
    checkout.cli(&["index", "--full"], 0);
    checkout.cli_json(&["baseline", "create", "parent", "--json"], 0);
    let attestation = checkout.cli_json(
        &["change-evidence", "attest-baseline", "parent", "--json"],
        0,
    );
    let scope = attestation["repoScope"].as_str().unwrap();
    checkout.write(
        "docs/working notes.md",
        "fixture for a path outside the evidence grammar\n",
    );
    checkout.write("docs/summary.md", "safe changed path\n");
    let head = checkout.commit("add two documents");
    let preparation = checkout.cli_json(&["change-evidence", "prepare", "parent", "--json"], 1);
    assert_eq!(preparation["status"], "unavailable");
    assert_eq!(preparation["reasonCodes"], json!(["unsupported-tree-path"]));
    assert_eq!(preparation["head"], head);
    assert_eq!(preparation["base"], base);

    let daemon = Daemon::start(&checkout, false);
    let body = daemon
        .get(&query(scope, &head, &base), true, None)
        .evidence(200);
    let git = &body["git"];
    assert_eq!(git["status"], "partial");
    assert_eq!(git["reasonCode"], "unsupported-tree-path");
    assert_eq!(git["total"], 2);
    assert_eq!(git["omitted"], 1);
    let rows = git["files"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], "docs/summary.md");
    assert_ne!(body["comparison"]["status"], "comparable");
}
