//! `ledgerful daemon --help` honesty (0328). Do not spawn the LSP.

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_ledgerful");

#[test]
fn daemon_help_names_lsp_stdio_and_unused_interval() {
    let output = Command::new(BINARY)
        .args(["daemon", "--help"])
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("daemon --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LSP"), "help={stdout}");
    assert!(stdout.contains("stdio"), "help={stdout}");
    assert!(
        !stdout.to_lowercase().contains("background daemon"),
        "help={stdout}"
    );
    assert!(stdout.contains("unused"), "interval help={stdout}");

    let root = Command::new(BINARY)
        .args(["--help"])
        .env("LEDGERFUL_NON_INTERACTIVE", "1")
        .output()
        .expect("root --help");
    assert!(
        root.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&root.stderr)
    );
    let root_out = String::from_utf8_lossy(&root.stdout);
    assert!(root_out.contains("daemon"), "root help={root_out}");
    assert!(
        root_out.contains("LSP") && root_out.contains("stdio"),
        "root --help must name LSP and stdio (wrapped lines ok), got={root_out}"
    );
}
