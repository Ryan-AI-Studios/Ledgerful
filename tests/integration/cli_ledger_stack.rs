use crate::common::{git_add_and_commit, non_interactive, run_cli, setup_git_repo};
use ledgerful::commands::init::execute_init;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn init_repo(root: &Path) {
    setup_git_repo(root);
    fs::write(root.join("dummy.txt"), "content").unwrap();
    git_add_and_commit(root, "initial");
    let _guard = crate::common::DirGuard::new(root);
    execute_init(false, false).unwrap();
}

#[test]
#[serial(cwd)]
fn ledger_stack_empty_human_names_register_rule_and_validator() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack"]);
    assert_eq!(code, 0, "empty stack should exit 0: {err}");
    assert!(
        out.contains("ledgerful ledger register rule"),
        "missing register rule next: {out}"
    );
    assert!(
        out.contains("ledgerful ledger register validator"),
        "missing register validator next: {out}"
    );
    assert!(
        !out.contains("register mapping"),
        "must not invent mapping CLI: {out}"
    );
    assert!(
        !out.contains("config set"),
        "must not put config set in next: {out}"
    );
    assert!(
        out.contains("enforcement_enabled"),
        "must mention enforcement_enabled: {out}"
    );
    assert!(out.contains("rules.toml"), "must mention rules.toml: {out}");
    assert!(
        out.contains("policy check") || out.contains("policy.toml"),
        "must mention policy check: {out}"
    );
    assert!(
        out.contains("auto-policy") || out.contains("verify"),
        "must mention verify auto-policy: {out}"
    );
    assert!(
        out.contains("TECH STACK RULES"),
        "missing rules heading: {out}"
    );
    assert!(
        out.contains("COMMIT VALIDATORS"),
        "missing validators heading: {out}"
    );
    assert!(
        out.contains("CATEGORY MAPPINGS"),
        "missing mappings heading: {out}"
    );
    assert!(out.contains("None."), "empty stays valid with None.: {out}");
    assert!(
        out.contains("category_mappings") || out.contains("stack-rule"),
        "empty mappings honesty line missing: {out}"
    );
    assert!(
        out.contains("TERM")
            && out.contains("--category")
            && out.contains("--reason")
            && out.contains("-x"),
        "empty next must name clap required flags: {out}"
    );
    assert!(
        out.contains("Blocking at start_change: no"),
        "empty catalog is not blocking: {out}"
    );
    assert!(
        !out.contains("defaults off"),
        "must not unconditionally claim defaults off: {out}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_stack_empty_json_envelope() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(code, 0, "empty stack --json should exit 0: {err}");
    assert!(
        out.trim_start().starts_with('{'),
        "json stdout must start with '{{': {out}"
    );
    let v: Value = serde_json::from_str(out.trim()).expect("ledger stack --json parse");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "ledgerStack");
    assert_eq!(v["empty"], true);
    assert_eq!(v["enforcementEnabled"], false);
    let next = v["next"]
        .as_array()
        .expect("next array")
        .iter()
        .map(|x| x.as_str().expect("next string"))
        .collect::<Vec<_>>();
    assert_eq!(
        next,
        vec![
            "ledgerful ledger register rule",
            "ledgerful ledger register validator"
        ]
    );
    assert!(
        next.iter()
            .all(|s| !s.contains("mapping") && !s.contains("config set")),
        "next must not mention mapping or config set: {next:?}"
    );
    assert_eq!(v["rules"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(v["validators"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(v["mappings"].as_array().map(|a| a.len()), Some(0));
    assert!(
        v.get("rulesNotEnforced").is_none() || v["rulesNotEnforced"] == false,
        "empty catalog omits rulesNotEnforced: {v}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_stack_help_names_sections_and_empty_next() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack", "--help"]);
    assert_eq!(code, 0, "stack --help should exit 0: {err}");
    let combined = format!("{out}{err}");
    assert!(
        combined.contains("TECH STACK RULES"),
        "help must name TECH STACK RULES: {combined}"
    );
    assert!(
        combined.contains("COMMIT VALIDATORS"),
        "help must name COMMIT VALIDATORS: {combined}"
    );
    assert!(
        combined.contains("CATEGORY MAPPINGS"),
        "help must name CATEGORY MAPPINGS: {combined}"
    );
    assert!(
        combined.contains("ledgerful ledger register rule"),
        "help must name register rule: {combined}"
    );
    assert!(
        combined.contains("ledgerful ledger register validator"),
        "help must name register validator: {combined}"
    );
    assert!(
        combined.contains("TERM") && combined.contains("-x"),
        "help after_help must name required flags: {combined}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_stack_populated_omits_next_keeps_mappings_honesty() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (reg_out, reg_err, reg_code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "register",
            "rule",
            "--category",
            "BUGFIX",
            "--reason",
            "test",
            "forbiddenterm",
        ],
    );
    assert_eq!(reg_code, 0, "register rule failed: {reg_err} {reg_out}");
    assert!(
        reg_out.contains("Next: ledgerful ledger stack"),
        "register rule success must name stack: {reg_out}"
    );

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack"]);
    assert_eq!(code, 0, "populated stack should exit 0: {err}");
    assert!(
        out.contains("forbiddenterm"),
        "populated human should print the rule: {out}"
    );
    assert!(
        !out.contains("Next:"),
        "populated path must not print Next block: {out}"
    );
    assert!(
        out.contains("CATEGORY MAPPINGS"),
        "missing mappings heading: {out}"
    );
    assert!(
        out.contains("stack-rule") || out.contains("category_mappings"),
        "empty mappings honesty line missing: {out}"
    );

    let (json_out, json_err, json_code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(json_code, 0, "populated --json failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("populated json");
    assert_eq!(v["empty"], false);
    assert_eq!(v["next"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(v["rules"].as_array().map(|a| a.len()), Some(1));
}

#[test]
#[serial(cwd)]
fn ledger_stack_json_enforcement_enabled_tracks_config() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let cfg_path = tmp.path().join(".ledgerful").join("config.toml");
    let mut cfg = fs::read_to_string(&cfg_path).expect("read config.toml");
    cfg.push_str("\n[ledger]\nenforcement_enabled = true\n");
    fs::write(&cfg_path, cfg).expect("write config.toml");

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(code, 0, "toggle --json failed: {err}");
    let v: Value = serde_json::from_str(out.trim()).expect("toggle json");
    assert_eq!(v["enforcementEnabled"], true);
}

#[test]
#[serial(cwd)]
fn ledger_stack_category_filter_drives_empty_and_next() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());

    let (reg_out, reg_err, reg_code) = run_cli(
        tmp.path(),
        &[
            "ledger",
            "register",
            "rule",
            "--category",
            "BUGFIX",
            "--reason",
            "test",
            "forbiddenterm",
        ],
    );
    assert_eq!(reg_code, 0, "register rule failed: {reg_err} {reg_out}");
    assert!(
        reg_out.contains("Next: ledgerful ledger stack"),
        "register rule success must name stack: {reg_out}"
    );

    let (bug_out, bug_err, bug_code) =
        run_cli(tmp.path(), &["ledger", "stack", "BUGFIX", "--json"]);
    assert_eq!(bug_code, 0, "BUGFIX --json failed: {bug_err}");
    let bug: Value = serde_json::from_str(bug_out.trim()).expect("BUGFIX json");
    assert_eq!(bug["empty"], false);
    assert_eq!(bug["rules"].as_array().map(|a| a.len()), Some(1));
    assert_eq!(bug["next"].as_array().map(|a| a.len()), Some(0));

    let (arch_out, arch_err, arch_code) =
        run_cli(tmp.path(), &["ledger", "stack", "ARCHITECTURE", "--json"]);
    assert_eq!(arch_code, 0, "ARCHITECTURE --json failed: {arch_err}");
    let arch: Value = serde_json::from_str(arch_out.trim()).expect("ARCHITECTURE json");
    assert_eq!(arch["empty"], true);
    assert_eq!(arch["rules"].as_array().map(|a| a.len()), Some(0));
    let next = arch["next"]
        .as_array()
        .expect("next")
        .iter()
        .map(|x| x.as_str().expect("str"))
        .collect::<Vec<_>>();
    assert_eq!(
        next,
        vec![
            "ledgerful ledger register rule",
            "ledgerful ledger register validator"
        ]
    );
}

fn register_forbidden_term(root: &Path) {
    let (reg_out, reg_err, reg_code) = run_cli(
        root,
        &[
            "ledger",
            "register",
            "rule",
            "--category",
            "BUGFIX",
            "--reason",
            "test",
            "forbiddenterm",
        ],
    );
    assert_eq!(reg_code, 0, "register rule failed: {reg_err} {reg_out}");
    assert!(
        reg_out.contains("Next: ledgerful ledger stack"),
        "register rule success must name stack: {reg_out}"
    );
}

#[test]
#[serial(cwd)]
fn ledger_stack_populated_default_config_is_not_blocking() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());
    register_forbidden_term(tmp.path());

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack"]);
    assert_eq!(code, 0, "populated stack failed: {err}");
    assert!(
        out.contains("Blocking at start_change: no"),
        "default config must not block: {out}"
    );
    assert!(
        out.contains("enforcement_enabled=false"),
        "open factor must name enforcement switch: {out}"
    );
    assert!(
        !out.contains("defaults off"),
        "must not unconditionally claim defaults off: {out}"
    );

    let (json_out, json_err, json_code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(json_code, 0, "populated --json failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("json");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "ledgerStack");
    assert_eq!(v["enforcementEnabled"], false);
    assert_eq!(v["empty"], false);
    assert_eq!(v["rulesNotEnforced"], true);
    assert_eq!(v["next"].as_array().map(|a| a.len()), Some(0));
}

#[test]
#[serial(cwd)]
fn ledger_stack_enforcement_on_observe_is_still_not_blocking() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());
    register_forbidden_term(tmp.path());

    let cfg_path = tmp.path().join(".ledgerful").join("config.toml");
    let mut cfg = fs::read_to_string(&cfg_path).expect("read config.toml");
    cfg.push_str("\n[ledger]\nenforcement_enabled = true\n");
    fs::write(&cfg_path, cfg).expect("write config.toml");

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack"]);
    assert_eq!(code, 0, "observe+enabled stack failed: {err}");
    assert!(
        out.contains("Blocking at start_change: no"),
        "observe mode must not claim blocking: {out}"
    );
    assert!(
        out.contains("gate.mode=observe"),
        "open factor must name observe gate: {out}"
    );
    assert!(
        !out.contains("defaults off"),
        "enabled config must not say defaults off: {out}"
    );

    let (json_out, json_err, json_code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(json_code, 0, "observe+enabled --json failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("json");
    assert_eq!(v["enforcementEnabled"], true);
    assert_eq!(v["rulesNotEnforced"], true);
}

#[test]
#[serial(cwd)]
fn ledger_stack_enforcement_on_and_gate_enforce_is_blocking() {
    let _ni = non_interactive();
    let tmp = tempdir().unwrap();
    init_repo(tmp.path());
    register_forbidden_term(tmp.path());

    let cfg_path = tmp.path().join(".ledgerful").join("config.toml");
    let mut cfg = fs::read_to_string(&cfg_path).expect("read config.toml");
    cfg = cfg.replace("mode = \"observe\"", "mode = \"enforce\"");
    cfg.push_str("\n[ledger]\nenforcement_enabled = true\n");
    fs::write(&cfg_path, cfg).expect("write config.toml");

    let (out, err, code) = run_cli(tmp.path(), &["ledger", "stack"]);
    assert_eq!(code, 0, "enforce stack failed: {err}");
    assert!(
        out.contains("Blocking at start_change: yes"),
        "enforce+enabled should block: {out}"
    );
    assert!(
        !out.contains("Blocking at start_change: no"),
        "must not also print no: {out}"
    );
    assert!(
        !out.contains("defaults off"),
        "enabled config must not say defaults off: {out}"
    );

    let (json_out, json_err, json_code) = run_cli(tmp.path(), &["ledger", "stack", "--json"]);
    assert_eq!(json_code, 0, "enforce --json failed: {json_err}");
    let v: Value = serde_json::from_str(json_out.trim()).expect("json");
    assert_eq!(v["enforcementEnabled"], true);
    assert!(
        v.get("rulesNotEnforced").is_none() || v["rulesNotEnforced"] == false,
        "blocking catalog omits rulesNotEnforced: {v}"
    );
}
