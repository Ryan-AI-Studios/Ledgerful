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
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ledgerful"))
        .args(args)
        .current_dir(cwd)
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .env_remove("LEDGERFUL_STATE_DIR")
        .output()
        .expect("spawn ledgerful")
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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

#[test]
#[ignore = "0549 D9: real cargo/nextest qualification; run manually per platform (owner: 0549)"]
fn embedded_qualification_mutations_fail_and_repairs_pass() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_fixture(root);

    // Template mutation (clean-tree `.txt` shape): fast must run the failing test.
    write(root, "templates/message.txt", "bye\n");
    let fast = run(root, &["verify", "--scope", "fast"]);
    assert!(
        !fast.status.success(),
        "template mutation must fail fast verify: {}",
        text(&fast)
    );
    assert!(
        text(&fast).contains("contracts_template"),
        "{}",
        text(&fast)
    );

    // Full verification fails in the test step on the same mutation.
    let full = run(root, &["verify", "--scope", "full"]);
    assert!(!full.status.success(), "{}", text(&full));
    assert!(
        text(&full).contains("contracts_template"),
        "{}",
        text(&full)
    );

    // Repair: template still contains the asserted text, tests run and pass.
    write(root, "templates/message.txt", "hi there\n");
    let repaired = run(root, &["verify", "--scope", "fast"]);
    assert!(repaired.status.success(), "{}", text(&repaired));

    // Document mutation (docs/** used to be fmt+clippy only).
    git_add_and_commit(root, "repair");
    write(root, "docs/contract.md", "v2\n");
    let doc = run(root, &["verify", "--scope", "fast"]);
    assert!(
        !doc.status.success(),
        "document mutation must fail fast verify: {}",
        text(&doc)
    );
    assert!(text(&doc).contains("contracts_document"), "{}", text(&doc));
}
