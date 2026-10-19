//! 0549 D9 — real `cargo fmt` / `clippy` / `nextest` evidence for embedded
//! documents and templates. Heavy (compiles a tiny crate several times), so
//! it is `#[ignore]`d and not a required CI check. Run on Windows, then WSL:
//!
//! ```text
//! cargo nextest run --test integration --run-ignored only -E 'test(embedded_qualification)'
//! ```
//!
//! Expected wall-clock: roughly 1-3 minutes per platform (five verify runs).

use crate::common::{git_add_and_commit, setup_git_repo};
use rstest::rstest;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn run(cwd: &Path, args: &[&str]) -> Output {
    let start = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("LEDGERFUL_STATE_DIR")
        .output()
        .expect("spawn ledgerful");
    eprintln!(
        "0549 receipt {}",
        serde_json::json!({
            "args": args, "platform": std::env::consts::OS,
            "exitCode": output.status.code(), "elapsedMs": start.elapsed().as_millis(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)
        })
    );
    output
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn commit_if_dirty(root: &Path, message: &str) {
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(status.status.success());
    if !status.stdout.is_empty() {
        git_add_and_commit(root, message);
    }
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    fs::write(p, body).expect("write");
}

const ACCESSOR: &str = r#"pub fn contract() -> &'static str {
    include_str!("../docs/contract.md")
}

pub fn message() -> &'static str {
    include_str!("../templates/message.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_document() {
        assert!(contract().contains("v1"));
    }

    #[test]
    fn contracts_template() {
        assert!(message().contains("hi"));
    }
}
"#;

fn init_fixture(root: &Path) {
    setup_git_repo(root);
    write(root, ".gitignore", "target/\n.ledgerful/\n");
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "src/lib.rs", "pub mod contracts;\n");
    write(root, "src/contracts.rs", ACCESSOR);
    write(root, "docs/contract.md", "v1\n");
    write(root, "templates/message.txt", "hi\n");
    git_add_and_commit(root, "init");
}

#[rstest]
#[case("document", "docs/contract.md", "v2\n", "contracts_document", false)]
#[case(
    "template",
    "templates/message.txt",
    "bye\n",
    "contracts_template",
    false
)]
#[case("mixed", "templates/message.txt", "bye\n", "contracts_template", false)]
#[case("bytes", "data/value.bin", "bye\n", "material_value", false)]
#[case("include", "data/value.inc", "2\n", "material_value", false)]
#[case("manifest", "data/value.txt", "bye\n", "material_value", false)]
#[case("runtime", "data/value.txt", "bye\n", "material_value", false)]
#[case("nested", "docs/contract.md", "v2\n", "contracts_document", true)]
#[ignore = "0549 D9: real cargo/nextest qualification; run manually per platform (owner: 0549)"]
fn embedded_qualification_mutations_fail_and_repairs_pass(
    #[case] kind: &str,
    #[case] material: &str,
    #[case] mutation: &str,
    #[case] failing_test: &str,
    #[case] nested: bool,
) {
    let tmp = tempdir().unwrap();
    let root = &tmp.path().join("space ü fixture");
    fs::create_dir_all(root).unwrap();
    assert!(root.to_string_lossy().contains('ü'));
    assert!(!root.to_string_lossy().contains('\u{fffd}'));
    init_fixture(root);
    let original = match kind {
        "bytes" => {
            write(
                root,
                "src/lib.rs",
                "#[test]\nfn material_value() { assert_eq!(include_bytes!(\"../data/value.bin\").as_slice(), b\"hi\\n\"); }\n",
            );
            "hi\n"
        }
        "include" => {
            write(
                root,
                "src/lib.rs",
                "#[cfg(test)]\nconst VALUE: u32 = include!(\"../data/value.inc\");\n#[test]\nfn material_value() { assert_eq!(VALUE, 1); }\n",
            );
            "1\n"
        }
        "manifest" => {
            write(
                root,
                "src/lib.rs",
                "#[test]\nfn material_value() { assert_eq!(include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/data/value.txt\")), \"hi\\n\"); }\n",
            );
            "hi\n"
        }
        "runtime" => {
            write(
                root,
                "src/lib.rs",
                "#[test]\nfn material_value() { assert_eq!(std::fs::read_to_string(\"data/value.txt\").unwrap(), \"hi\\n\"); }\n",
            );
            "hi\n"
        }
        "document" | "nested" => "v1\n",
        _ => "hi\n",
    };
    write(root, material, original);
    if nested {
        fs::create_dir_all(root.join("backend")).unwrap();
        fs::rename(root.join("Cargo.toml"), root.join("backend/Cargo.toml")).unwrap();
        fs::rename(root.join("src"), root.join("backend/src")).unwrap();
        write(
            root,
            "backend/src/contracts.rs",
            &ACCESSOR
                .replace("../docs/", "../../docs/")
                .replace("../templates/", "../../templates/"),
        );
        git_add_and_commit(root, "nested-only parent");
        write(root, material, mutation);
        let refused = run(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
        let nested_plan: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
        assert_eq!(refused.status.code(), Some(1), "{nested_plan}");
        assert_eq!(nested_plan["refused"], true, "{nested_plan}");
        assert!(
            nested_plan["fallbackReason"]
                .as_str()
                .unwrap()
                .contains("root workspace"),
            "{nested_plan}"
        );
        eprintln!("0549 nested-only refusal: {nested_plan}");
        write(root, material, original);
        // Qualify the same nested crate through the specified recovery:
        // a root workspace manifest, with no command or profile rewriting.
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"backend\"]\nresolver = \"2\"\n",
        );
    }
    let cargo_root = if nested {
        root.join("backend")
    } else {
        root.to_path_buf()
    };
    let fmt = Command::new("cargo")
        .args(["fmt", "--all"])
        .current_dir(&cargo_root)
        .output()
        .unwrap();
    assert!(fmt.status.success(), "{}", text(&fmt));
    commit_if_dirty(root, "qualification parent");
    let parent = run(root, &["verify", "--scope", "full"]);
    assert!(parent.status.success(), "parent {kind}: {}", text(&parent));
    // Cargo creates its lockfile during the oracle. Include it in the
    // passing parent so the mutation does not become a shared-infra edit.
    commit_if_dirty(root, "qualification built parent");
    write(root, material, mutation);
    if kind == "mixed" {
        // A real source edit beside the template; index before editing so
        // the existing SAME_FILE route can scope it.
        let init = run(root, &["init"]);
        assert!(init.status.success(), "{}", text(&init));
        let index = run(root, &["index", "--incremental"]);
        assert!(index.status.success(), "{}", text(&index));
        let source = fs::read_to_string(root.join("src/contracts.rs")).unwrap();
        write(
            root,
            "src/contracts.rs",
            &(source + "\n// qualification mixed source edit\n"),
        );
    }
    let plan = run(root, &["verify", "--scope", "fast", "--dry-run", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(value["refused"], false, "{kind}: {value}");
    assert_eq!(value["scopeExecuted"], "fast", "{kind}: {value}");
    assert!(
        text(&plan).contains(&format!("test({failing_test})")),
        "{}",
        text(&plan)
    );
    for scope in ["fast", "full"] {
        let failed = run(root, &["verify", "--scope", scope]);
        assert_eq!(
            failed.status.code(),
            Some(1),
            "{kind}/{scope}: {}",
            text(&failed)
        );
        assert!(
            text(&failed).contains(failing_test),
            "{kind}/{scope}: {}",
            text(&failed)
        );
        assert!(
            text(&failed).contains("FAIL"),
            "must observe test failure: {}",
            text(&failed)
        );
        eprintln!("0549 qualification {kind}/{scope}: exit=1 failing_test={failing_test}");
    }
    write(root, material, original);
    let repaired = run(root, &["verify", "--scope", "full"]);
    assert!(
        repaired.status.success(),
        "repair {kind}: {}",
        text(&repaired)
    );
    eprintln!("0549 qualification {kind}: parent=pass repair=pass plan={value}");
}
