//! Integration tests for scripts/require-secret.sh (0098 DoD-5).
//!
//! Hermetic: does not touch live repository secrets. Asserts non-zero exit when
//! the named env var is empty/unset, and zero when set.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bash_command() -> Command {
    // Prefer Git Bash on Windows (WSL bash may not inherit the same env layout).
    if cfg!(windows) {
        let candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for c in candidates {
            if std::path::Path::new(c).is_file() {
                return Command::new(c);
            }
        }
    }
    Command::new("bash")
}

fn run_require_secret(env_pairs: &[(&str, Option<&str>)]) -> std::process::Output {
    let script = repo_root().join("scripts/require-secret.sh");
    let mut cmd = bash_command();
    cmd.arg(script.as_os_str());
    cmd.arg("MANIFEST_PUSH_TOKEN");
    cmd.current_dir(repo_root());
    // Clear then optionally set so the child does not inherit a host secret.
    cmd.env_remove("MANIFEST_PUSH_TOKEN");
    for (key, value) in env_pairs {
        match value {
            Some(v) => {
                cmd.env(key, v);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.output().unwrap_or_else(|e| {
        panic!("failed to launch bash for require-secret.sh: {e}");
    })
}

#[test]
fn require_secret_fails_when_empty() {
    let output = run_require_secret(&[("MANIFEST_PUSH_TOKEN", Some(""))]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "require-secret must fail when secret is empty\nstatus: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains("MANIFEST_PUSH_TOKEN"),
        "stderr must name the secret\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("package-distribution.md"),
        "stderr must point at docs/package-distribution.md\nstderr:\n{stderr}"
    );
}

#[test]
fn require_secret_fails_when_unset() {
    let output = run_require_secret(&[("MANIFEST_PUSH_TOKEN", None)]);
    assert!(
        !output.status.success(),
        "require-secret must fail when secret is unset\nstatus: {:?}",
        output.status.code()
    );
}

#[test]
fn require_secret_ok_when_set() {
    let output = run_require_secret(&[("MANIFEST_PUSH_TOKEN", Some("test-not-a-real-secret"))]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "require-secret must succeed when secret is non-empty\nstatus: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
}
