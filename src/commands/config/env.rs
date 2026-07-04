pub(crate) const INTERNAL_ENV_PREFIX: &str = "LEDGERFUL_";

/// Internal environment variables that are NOT prefixed with `LEDGERFUL_`.
/// These are convention-based or tool-internal variables that should be
/// categorized as "internal" rather than "missing from declarations".
pub(crate) const NON_PREFIXED_INTERNAL_ENV_VARS: &[&str] = &[
    // Verify dry-run diagnostic flag — internal to ledgerful's verify path.
    "VERBOSE_DRY_RUN",
    // Standard terminal color convention — not user-facing ledgerful config.
    "NO_COLOR",
    // Non-interactive mode convention — used by ledgerful's hook path.
    "NON_INTERACTIVE",
    // Provider API keys/models — internal to ledgerful's LLM backend selection.
    "OLLAMA_CLOUD_API_KEY",
    "OLLAMA_API_KEY",
    "OLLAMA_CLOUD_URL",
    "OLLAMA_CLOUD_MODEL",
    "OPENROUTER_API_KEY",
    "OPENROUTER_MODEL",
];

/// CG-F35 (requirement #4): is this reference path a test or example file
/// rather than production code?
///
/// This is intentionally a narrow, purpose-built check rather than a reuse
/// of `index::topology::is_test_file` (substring matching, e.g.
/// `path.contains("test_")`). That helper was designed for soft/statistical
/// directory classification where false positives are tolerable; reused
/// here as a hard binary filter it is too loose — it would, for example,
/// classify the genuine production files `src/commands/test_mapping.rs` and
/// `src/index/test_mapping.rs` as "test/example" purely because their
/// filename contains the substring `test_`, silently downgrading any real
/// env-var declaration gap they have. That is exactly the failure mode
/// requirement #7 warns against ("real dependencies are not hidden
/// accidentally").
///
/// Instead this matches on path *segments* (split on `/` after normalizing
/// `\` to `/`) being exactly `tests`, `test`, `examples`, or `example`, or
/// the file's *basename* matching an anchored `*_test.rs` / `*_tests.rs`
/// suffix pattern — never a bare substring search over the full path. Note
/// a `test_*.rs` *prefix* pattern is deliberately excluded: it would match
/// `test_mapping.rs` (the exact production file this check must not
/// misclassify), and no convention in this codebase names a single
/// production-adjacent file that way to mean "this file is a test".
pub(crate) fn is_test_or_example_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let mut segments = normalized.split('/').filter(|s| !s.is_empty());

    if segments.any(|seg| matches!(seg, "tests" | "test" | "examples" | "example")) {
        return true;
    }

    let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
    let stem = basename.strip_suffix(".rs").unwrap_or(basename);
    stem.ends_with("_test") || stem.ends_with("_tests")
}

pub(crate) fn is_ignored_env_var(var: &str) -> bool {
    // Standard OS/shell environment variables — never configurable via ledgerful.
    let ignored = [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "TERM",
        "CI",
        "PSModulePath",
        "XDG_CACHE_HOME",
        "LOCALAPPDATA",
        "TARGET",
        "USERNAME",
        "USERPROFILE",
        // Standard Rust/Cargo ecosystem variables — convention-based, not user-facing
        // configuration for ledgerful. CARGO_* is already covered by the starts_with
        // check below; these are the non-CARGO-prefixed ones.
        "RUST_LOG",
        "RUST_BACKTRACE",
        "RUSTC_WRAPPER",
        // Standard OpenTelemetry convention variable.
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        // Standard terminal color convention variables.
        "CLICOLOR",
        "CLICOLOR_FORCE",
    ];
    ignored.contains(&var) || var.starts_with("CARGO_")
}

pub(crate) fn is_internal_env_var(var: &str) -> bool {
    var.starts_with(INTERNAL_ENV_PREFIX) || NON_PREFIXED_INTERNAL_ENV_VARS.contains(&var)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the CG-F35 review finding: `is_test_or_example_path`
    /// must not misclassify genuine production files whose name happens to
    /// contain the substring `test_` (e.g. `test_mapping.rs`). It must still
    /// correctly classify real test/example paths via path segments or an
    /// anchored `*_test(s).rs` filename suffix.
    #[test]
    fn is_test_or_example_path_does_not_misclassify_production_test_mapping_files() {
        assert!(
            !is_test_or_example_path("src/commands/test_mapping.rs"),
            "src/commands/test_mapping.rs is a real production file and must not be \
             classified as test/example-only"
        );
        assert!(
            !is_test_or_example_path("src/index/test_mapping.rs"),
            "src/index/test_mapping.rs is a real production file and must not be \
             classified as test/example-only"
        );
    }

    #[test]
    fn is_test_or_example_path_classifies_real_test_and_example_paths() {
        assert!(is_test_or_example_path("tests/integration/cli_config.rs"));
        assert!(is_test_or_example_path("examples/foo.rs"));
    }

    #[test]
    fn is_test_or_example_path_handles_windows_separators_and_anchored_suffixes() {
        // Backslash-separated paths normalize the same as forward-slash ones.
        assert!(!is_test_or_example_path("src\\commands\\test_mapping.rs"));
        assert!(!is_test_or_example_path("src\\index\\test_mapping.rs"));

        // Filename-anchored `_test.rs` / `_tests.rs` suffixes still match,
        // even outside a tests/examples directory.
        assert!(is_test_or_example_path("src/foo_test.rs"));
        assert!(is_test_or_example_path("src/foo_tests.rs"));

        // Plain production files are unaffected.
        assert!(!is_test_or_example_path("src/main.rs"));
    }

    // TA27: internal env var classification
    #[test]
    fn is_internal_env_var_verbose_dry_run() {
        assert!(
            is_internal_env_var("VERBOSE_DRY_RUN"),
            "VERBOSE_DRY_RUN must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_no_color() {
        assert!(
            is_internal_env_var("NO_COLOR"),
            "NO_COLOR must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_non_interactive() {
        assert!(
            is_internal_env_var("NON_INTERACTIVE"),
            "NON_INTERACTIVE must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_ledgerful_prefix() {
        assert!(
            is_internal_env_var("LEDGERFUL_SOME_INTERNAL_VAR"),
            "LEDGERFUL_-prefixed vars must be classified as internal"
        );
    }

    #[test]
    fn is_internal_env_var_rejects_unrelated_var() {
        assert!(
            !is_internal_env_var("SOME_PUBLIC_API_KEY"),
            "Unrelated vars must not be classified as internal"
        );
    }

    #[test]
    fn is_ignored_env_var_rust_ecosystem() {
        assert!(
            is_ignored_env_var("RUST_LOG"),
            "RUST_LOG must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("RUST_BACKTRACE"),
            "RUST_BACKTRACE must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("CARGO_TARGET_DIR"),
            "CARGO_TARGET_DIR must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("CARGO_HOME"),
            "CARGO_HOME must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("CARGO_INCREMENTAL"),
            "CARGO_INCREMENTAL must be ignored (starts with CARGO_)"
        );
        assert!(
            is_ignored_env_var("RUSTC_WRAPPER"),
            "RUSTC_WRAPPER must be ignored as a standard ecosystem var"
        );
        assert!(
            is_ignored_env_var("OTEL_EXPORTER_OTLP_ENDPOINT"),
            "OTEL_EXPORTER_OTLP_ENDPOINT must be ignored as a standard convention var"
        );
        assert!(
            is_ignored_env_var("CLICOLOR"),
            "CLICOLOR must be ignored as a standard terminal color convention var"
        );
        assert!(
            is_ignored_env_var("CLICOLOR_FORCE"),
            "CLICOLOR_FORCE must be ignored as a standard terminal color convention var"
        );
    }
}
