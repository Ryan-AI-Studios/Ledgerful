//! Track DX7: Global Proactive Config Enablement.
//!
//! Verifies that config-gated empty states emit the standardized
//! `ledgerful config set <key>=true` hint when the gate is OFF, and keep the
//! existing NoIndexedData/NoMatches message when the gate is ON. Tests call
//! the public `empty_state_message`-style functions directly â€” no LLM, no
//! binary invocation, no git repo required.

use ledgerful::commands::deploy::deploy_empty_state_message;
use ledgerful::commands::services_diff::empty_state_message;
use ledgerful::config::model::Config;
use ledgerful::output::empty::config_enable_hint;
use ledgerful::state::storage::StorageManager;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use crate::common::{git_cmd, setup_git_repo};

// ---------------------------------------------------------------------------
// Shared helper phrasing (src/output/empty.rs)
// ---------------------------------------------------------------------------

#[test]
fn hint_single_key_phrasing() {
    let hint = config_enable_hint(&["coverage.deploy.enabled"]);
    assert_eq!(
        hint,
        "To enable, run: `ledgerful config set coverage.deploy.enabled=true`."
    );
}

#[test]
fn hint_two_keys_parenthetical_phrasing() {
    let hint = config_enable_hint(&["coverage.enabled", "coverage.deploy.enabled"]);
    assert_eq!(
        hint,
        "To enable, run: `ledgerful config set coverage.enabled=true` (then \
         `ledgerful config set coverage.deploy.enabled=true`)."
    );
}

#[test]
fn hint_empty_keys_is_empty_string() {
    assert!(config_enable_hint(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// services diff (regression: still uses the shared helper)
// ---------------------------------------------------------------------------

#[test]
fn services_diff_disabled_globally_emits_two_key_hint() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = false;
    config.coverage.services.enabled = true;

    let (reason, msg) = empty_state_message(&storage, &config);
    assert_eq!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(msg.contains("coverage.enabled"), "got: {msg}");
    assert!(msg.contains("coverage.services.enabled"), "got: {msg}");
    assert!(msg.contains("config set"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing: {msg}"
    );
}

#[test]
fn services_diff_disabled_for_services_emits_one_key_hint() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = false;

    let (reason, msg) = empty_state_message(&storage, &config);
    assert_eq!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(msg.contains("coverage.services.enabled"), "got: {msg}");
    assert!(msg.contains("config set"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing: {msg}"
    );
}

#[test]
fn services_diff_enabled_does_not_emit_config_hint() {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();

    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.services.enabled = true;

    let (reason, msg) = empty_state_message(&storage, &config);
    // When enabled, the reason should NOT be DisabledByConfig.
    assert_ne!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(
        !msg.contains("config set"),
        "should not emit config hint when enabled: {msg}"
    );
}

// ---------------------------------------------------------------------------
// deploy impact (new: previously had no config hint at all)
// ---------------------------------------------------------------------------

#[test]
fn deploy_disabled_globally_emits_two_key_hint() {
    let mut config = Config::default();
    config.coverage.enabled = false;
    config.coverage.deploy.enabled = true;

    let (reason, msg) = deploy_empty_state_message(&config);
    assert_eq!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(msg.contains("coverage.enabled"), "got: {msg}");
    assert!(msg.contains("coverage.deploy.enabled"), "got: {msg}");
    assert!(msg.contains("config set"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing: {msg}"
    );
}

#[test]
fn deploy_disabled_for_deploy_specifically_emits_one_key_hint() {
    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.deploy.enabled = false;

    let (reason, msg) = deploy_empty_state_message(&config);
    assert_eq!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(msg.contains("coverage.deploy.enabled"), "got: {msg}");
    assert!(msg.contains("config set"), "got: {msg}");
    assert!(
        msg.contains("not change"),
        "should not suggest reindexing: {msg}"
    );
    // Should NOT mention the global key when only the section gate is off.
    assert!(
        !msg.contains("coverage.enabled = false"),
        "should not blame global gate when only deploy is disabled: {msg}"
    );
}

#[test]
fn deploy_enabled_does_not_emit_config_hint() {
    let mut config = Config::default();
    config.coverage.enabled = true;
    config.coverage.deploy.enabled = true;

    let (reason, msg) = deploy_empty_state_message(&config);
    assert_ne!(
        reason,
        ledgerful::output::empty::EmptyReason::DisabledByConfig
    );
    assert!(
        !msg.contains("config set"),
        "should not emit config hint when enabled: {msg}"
    );
    assert!(
        msg.contains("No deployment impact detected"),
        "should keep the existing no-matches message: {msg}"
    );
}

// ---------------------------------------------------------------------------
// End-to-end via the `ledgerful` binary (exercises execute_impact_silent).
// No LLM is contacted by `deploy impact` â€” it is pure impact analysis.
// ---------------------------------------------------------------------------

fn ledgerful_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ledgerful")
}

/// Write a `.ledgerful/config.toml` in `root` with the given coverage flags.
/// Only ever writes inside the temp repo â€” never touches the real repo config.
fn write_temp_config(root: &std::path::Path, coverage_enabled: bool, deploy_enabled: bool) {
    let cg_dir = root.join(".ledgerful");
    fs::create_dir_all(&cg_dir).unwrap();
    let content = format!(
        "[coverage]\nenabled = {coverage_enabled}\n\n[coverage.deploy]\nenabled = \
         {deploy_enabled}\n"
    );
    fs::write(cg_dir.join("config.toml"), content).unwrap();
}

fn run_deploy_impact(root: &std::path::Path, json: bool) -> (bool, String, String) {
    let mut cmd = Command::new(ledgerful_bin());
    cmd.arg("deploy").arg("impact");
    if json {
        cmd.arg("--json");
    }
    let output = cmd.current_dir(root).output().expect("binary should run");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Seed the temp repo with an initial commit so subsequent working-tree
/// changes show up in the diff. `ledgerful init` is then run via the binary.
fn seed_repo_and_init(root: &std::path::Path) {
    setup_git_repo(root);
    fs::write(root.join("README.md"), "# temp\n").unwrap();
    git_cmd(root, &["add", "-A"]);
    git_cmd(root, &["commit", "-m", "initial"]);
    let init_status = Command::new(ledgerful_bin())
        .arg("init")
        .current_dir(root)
        .status()
        .expect("ledgerful init should run");
    assert!(init_status.success(), "ledgerful init should succeed");
}

#[test]
fn deploy_impact_binary_detects_dockerfile_when_enabled() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    seed_repo_and_init(root);
    write_temp_config(root, true, true);

    // Create an untracked Dockerfile so it appears in the working-tree diff.
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.20\nCOPY src/ ./src/\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "deploy impact --json should succeed; stderr: {stderr}");
    // The populated path returns a bare JSON array of manifest objects.
    assert!(
        stdout.contains("Dockerfile"),
        "JSON should contain the changed Dockerfile; got: {stdout}"
    );
}

#[test]
fn deploy_impact_binary_emits_disabled_empty_state_when_deploy_gate_off() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    seed_repo_and_init(root);

    // Gate ON first, create the Dockerfile so a manifest would otherwise match.
    write_temp_config(root, true, true);
    fs::write(root.join("Dockerfile"), "FROM alpine:3.20\n").unwrap();

    // Now disable the deploy-specific gate and verify the disabled empty state.
    write_temp_config(root, true, false);

    let (ok, stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "deploy impact --json should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("disabledByConfig"),
        "JSON empty state should cite disabledByConfig; got: {stdout}"
    );
    assert!(
        stdout.contains("coverage.deploy.enabled=true"),
        "JSON message should contain the config set hint; got: {stdout}"
    );
    assert!(
        stdout.contains("\"results\":"),
        "empty JSON state should be an object with a results key; got: {stdout}"
    );
}

#[test]
fn deploy_impact_binary_human_path_prints_hint_when_disabled() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    seed_repo_and_init(root);

    // Disable the global coverage gate so the two-key hint is emitted.
    write_temp_config(root, false, true);

    let (ok, stdout, stderr) = run_deploy_impact(root, false);
    assert!(ok, "human deploy impact should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("config set coverage.enabled=true"),
        "human output should contain the global enable hint; got: {stdout}"
    );
    assert!(
        stdout.contains("coverage.deploy.enabled=true"),
        "human output should contain the deploy enable hint; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// BLOCKER 1 regression: nested-subdirectory invocation must use the repo-root
// config + storage consistently. Git discovery walks up from CWD to the repo
// root, so running from a subdir must still detect a root-level Dockerfile
// under the repo-root config's gating.
//
// SHOULD-FIX (subdir YAML): a Dockerfile alone does NOT pin subdir correctness
// because `classify_deploy_manifest` classifies Dockerfiles by basename without
// a content read, so the CWD-based helper still detects `Dockerfile` even when
// `project_root` is wrongly set to the subdir (the read of
// `subdir/Dockerfile` fails but content is `unwrap_or_default()`'d to empty).
// The root-level `docker-compose.yml` DOES require a content read to classify
// (YAML arm), so with the CWD-based helper from a subdir the read of
// `subdir/docker-compose.yml` fails -> `classify_deploy_manifest` returns None
// -> not detected -> this test fails. With `compute_impact_in_memory_at` the
// `project_root` is the resolved repo workdir, `repo_root/docker-compose.yml`
// reads successfully -> detected -> test passes. This is the assertion that
// genuinely pins the subdir fix.
// ---------------------------------------------------------------------------

#[test]
fn deploy_impact_from_subdir_uses_repo_root_config_and_storage() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    seed_repo_and_init(root);
    write_temp_config(root, true, true);

    // Place a Dockerfile AND a docker-compose.yml at the repo root (where the
    // repo-root config lives). The compose file matches the default deploy
    // pattern `**/docker-compose*.yml` and requires a content read to classify.
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.20\nCOPY src/ ./src/\n",
    )
    .unwrap();
    fs::write(
        root.join("docker-compose.yml"),
        "services:\n  app:\n    image: nginx\n",
    )
    .unwrap();

    // Invoke the binary from a subdirectory of the repo.
    let subdir = root.join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    let mut cmd = Command::new(ledgerful_bin());
    cmd.arg("deploy").arg("impact").arg("--json");
    let output = cmd
        .current_dir(&subdir)
        .output()
        .expect("binary should run");
    let ok = output.status.success();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        ok,
        "deploy impact --json from subdir should succeed; stderr: {stderr}"
    );
    assert!(
        stdout.contains("Dockerfile"),
        "subdir invocation should still detect the root-level Dockerfile via \
         repo-root config + storage; got: {stdout}"
    );
    // The YAML assertion is the one that genuinely pins the subdir fix: a
    // root-level docker-compose.yml can only be detected if `project_root` is
    // the repo root (so the content read resolves). Reverting to the CWD-based
    // helper makes this assertion fail.
    assert!(
        stdout.contains("docker-compose.yml"),
        "subdir invocation should detect the root-level docker-compose.yml \
         (requires repo-root project_root for the content read); got: {stdout}"
    );
    assert!(
        stdout.contains("DockerCompose"),
        "subdir invocation should classify docker-compose.yml as DockerCompose; \
         got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// BLOCKER 2 regression: `deploy impact` must distinguish "not a git repo"
// (`RepoDiscoveryFailed` -> config-gated empty state, exit 0) from a broken
// repo (`RepoOpenFailed`/other -> propagate the error, exit non-zero).
//
// NOTE on the broken-repo arm: on the gix version currently pinned,
// `gix::discover` itself validates repo structure (HEAD/objects/refs) and
// propagates any structural open failure as `NoGitRepository`, which
// `open_repo` maps to `RepoDiscoveryFailed`. Empirically (probed across
// no-HEAD, no-objects, objects-is-file, empty-.git, gitdir-nowhere,
// pack-is-file, refs-is-file, head-is-dir, etc.) EVERY filesystem corruption
// surfaces as `RepoDiscoveryFailed`, and `gix::open` is lenient enough that
// missing/invalid `.git/config` still opens OK. So `RepoOpenFailed` is
// defensive on this gix version â€” it fires only for errors `discover` lets
// through but `open` rejects (permission races, future gix versions), which
// cannot be reliably synthesized on Windows without POSIX permission tricks.
//
// The BLOCKER 2 fix's `Err(e) => return Err(e.into())` propagation arm is
// therefore verified by code inspection plus the existing
// `git::repo::tests::test_discover_fail` unit test, which pins the
// `RepoDiscoveryFailed` variant the discrimination keys on. Below we pin the
// REACHABLE behavioral contract end-to-end: a non-repo directory succeeds with
// the config-gated empty state (the `RepoDiscoveryFailed -> false` path the fix
// preserves), proving the discrimination does not regress the no-repo fallback.
// ---------------------------------------------------------------------------

#[test]
fn deploy_impact_non_repo_dir_succeeds_with_empty_state() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    // Intentionally NO `git init` / `seed_repo_and_init` here: this directory is
    // not a git repo, so `open_repo` returns `RepoDiscoveryFailed`, which the
    // BLOCKER 2 fix maps to `in_git_repo = false` -> config-gated empty state.
    // Write a config so the deploy-specific gate is OFF and the hint is emitted.
    write_temp_config(root, false, true);

    let (ok, stdout, stderr) = run_deploy_impact(root, false);
    assert!(
        ok,
        "non-repo dir should exit 0 with the config-gated empty state, not \
         error; stderr: {stderr}"
    );
    assert!(
        stdout.contains("config set coverage.enabled=true"),
        "non-repo dir should emit the global coverage enable hint; got: {stdout}"
    );
    assert!(
        stdout.contains("coverage.deploy.enabled=true"),
        "non-repo dir should emit the deploy enable hint; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// SHOULD-FIX 1 / non-persistence regression: `deploy impact` must NOT write
// `.ledgerful/reports/latest-impact.json`. It routes through
// `compute_impact_in_memory` which skips `write_impact_report` and
// `storage.save_packet`. We snapshot report existence before the run and
// assert it is unchanged after.
// ---------------------------------------------------------------------------

#[test]
fn deploy_impact_does_not_write_latest_impact_report() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    seed_repo_and_init(root);
    write_temp_config(root, true, true);
    fs::write(
        root.join("Dockerfile"),
        "FROM alpine:3.20\nCOPY src/ ./src/\n",
    )
    .unwrap();

    let report_path = root
        .join(".ledgerful")
        .join("reports")
        .join("latest-impact.json");
    // Snapshot existence + content before the run (robust to `init` having
    // created it or not).
    let before = report_path
        .exists()
        .then(|| fs::read_to_string(&report_path).ok());

    let (ok, _stdout, stderr) = run_deploy_impact(root, true);
    assert!(ok, "deploy impact --json should succeed; stderr: {stderr}");

    let after = report_path
        .exists()
        .then(|| fs::read_to_string(&report_path).ok());
    assert_eq!(
        before, after,
        "deploy impact must not create or modify latest-impact.json (it uses \
         the non-persisting compute_impact_in_memory); before={before:?} \
         after={after:?}"
    );
}
