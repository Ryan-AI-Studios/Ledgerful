use std::io::{BufRead, IsTerminal, Write};

/// Returns true if the current environment is interactive (STDIN is a terminal
/// and no non-interactive overrides are set).
pub fn is_interactive() -> bool {
    // Check for explicit non-interactive flag
    if std::env::var("LEDGERFUL_NON_INTERACTIVE").is_ok() {
        return false;
    }

    // Bare `NON_INTERACTIVE` (no `LEDGERFUL_` prefix) is also honored for
    // consistency with `hook_commit_msg.rs`'s TUI-skip check, which already
    // treats both spellings as equivalent (Track TA31 R3).
    if std::env::var("NON_INTERACTIVE").is_ok() {
        return false;
    }

    // Check for common CI environments
    if std::env::var("CI").is_ok() {
        return false;
    }

    // Default to checking if stdin is a terminal
    std::io::stdin().is_terminal()
}

/// Prompts the user with `msg` (expected to follow the `[Y/n]` convention) and
/// reads a single line of input, defaulting to YES: empty input is treated as
/// "yes", as is any response starting with `y`/`Y`.
///
/// This is the interactive-surface bootstrapping entry point (Track DX1). When
/// the environment is non-interactive (CI, piped stdin, or
/// `LEDGERFUL_NON_INTERACTIVE=1`) it returns `false` WITHOUT reading stdin, so
/// CI/pipe flows degrade to the existing read-only empty-state output and never
/// block waiting on a TTY that will never answer. Any read error also returns
/// `false` rather than panicking.
pub fn prompt_yes_no(msg: &str) -> bool {
    if !is_interactive() {
        return false;
    }
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    prompt_yes_no_with(msg, true, &mut reader)
}

/// Testable core of [`prompt_yes_no`]: performs the interactive read against the
/// supplied `BufRead` when `interactive` is true, without touching environment
/// variables or real stdin. Exposed as `pub(crate)` so in-crate unit tests can
/// drive deterministic inputs without TTY or env races (the `serial_test`
/// crate is not a project dependency, so the env-gated [`is_interactive`] path
/// is deliberately kept out of the unit test surface).
pub(crate) fn prompt_yes_no_with(msg: &str, interactive: bool, input: &mut impl BufRead) -> bool {
    if !interactive {
        return false;
    }
    print!("{msg}");
    if std::io::stdout().flush().is_err() {
        return false;
    }
    let mut line = String::new();
    match input.read_line(&mut line) {
        // EOF / no data read: treat as no answer, not a panic.
        Ok(0) => false,
        Ok(_) => {
            let trimmed = line.trim();
            // Default-YES: empty input or any string starting with 'y'/'Y'.
            trimmed.is_empty() || trimmed.starts_with('y') || trimmed.starts_with('Y')
        }
        Err(_) => false,
    }
}

pub(crate) fn prompt_yes_no_stderr_with(
    msg: &str,
    interactive: bool,
    input: &mut impl BufRead,
) -> bool {
    if !interactive {
        return false;
    }
    eprint!("{msg}");
    if std::io::stderr().flush().is_err() {
        return false;
    }
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) => false,
        Ok(_) => {
            let trimmed = line.trim();
            trimmed.is_empty() || trimmed.starts_with('y') || trimmed.starts_with('Y')
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_interactive, prompt_yes_no_with};
    use std::io::Cursor;

    /// Minimal RAII env-var guard for the single `is_interactive()` test below
    /// that needs to set/unset an env var. nextest runs each test in its own
    /// process (see `.config/nextest.toml`), so unlike plain `cargo test`
    /// there is no cross-test race to guard against here; this still restores
    /// the prior value on drop to avoid surprising a future test added to
    /// this module.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: nextest isolates each test in its own process, so there
            // is no concurrent mutation of the environment within a process.
            // Legitimate: TempEnv RAII for edition-2024 set_var in tests.
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: see `set` above.
            // Legitimate: restore env on Drop (edition-2024 set_var/remove_var).
            // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
            unsafe {
                match &self.original {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn bare_non_interactive_env_var_disables_interactive() {
        let _guard = EnvVarGuard::set("NON_INTERACTIVE", "1");
        assert!(!is_interactive());
    }

    fn run(input: &str, interactive: bool) -> bool {
        let mut cursor = Cursor::new(input.as_bytes());
        prompt_yes_no_with("prompt? [Y/n] ", interactive, &mut cursor)
    }

    #[test]
    fn non_interactive_returns_false_without_reading_stdin() {
        // No input cursor at all is fine: the function must not read from it
        // when interactive is false. Empty/closed stdin therefore never blocks.
        let mut empty = Cursor::new(Vec::<u8>::new());
        assert!(!prompt_yes_no_with("x? [Y/n] ", false, &mut empty));
    }

    #[test]
    fn interactive_yes_lowercase_is_true() {
        assert!(run("y\n", true));
    }

    #[test]
    fn interactive_yes_uppercase_is_true() {
        assert!(run("Y\n", true));
    }

    #[test]
    fn interactive_empty_line_is_default_yes() {
        assert!(run("\n", true));
    }

    #[test]
    fn interactive_no_is_false() {
        assert!(!run("n\n", true));
    }

    #[test]
    fn interactive_no_word_is_false() {
        assert!(!run("no\n", true));
    }

    #[test]
    fn interactive_unrecognized_is_false() {
        assert!(!run("x\n", true));
    }

    #[test]
    fn interactive_eof_returns_false_without_panic() {
        // EOF before any newline: read_line returns Ok(0).
        assert!(!run("", true));
    }

    #[test]
    fn interactive_yes_prefix_words_are_true() {
        // "yes" and "yeah" start with 'y' → default-YES semantics.
        assert!(run("yes\n", true));
        assert!(run("yeah sure\n", true));
    }

    #[test]
    fn prompt_message_is_emitted_to_stdout_before_read() {
        // The message is printed to stdout via print! before reading; we cannot
        // easily capture stdout in a unit test without redirecting it, but we
        // can at least assert the interactive path completes normally and
        // returns the expected value for a "y" answer.
        assert!(run("y\n", true));
    }
}
