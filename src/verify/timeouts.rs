pub const DEFAULT_AUTO_TIMEOUT_SECS: u64 = 400;

/// Manual `-c` when `--timeout` is omitted. Matches the pre-0414 Clap default.
pub const MANUAL_OMITTED_TIMEOUT_SECS: u64 = 600;

pub fn manual_timeout(timeout_secs: u64) -> u64 {
    timeout_secs
}

/// Seconds for a manual command. `None` keeps 600. `Some(0)` stays immediate.
pub fn manual_timeout_from_cli(cli: Option<u64>) -> u64 {
    match cli {
        None => MANUAL_OMITTED_TIMEOUT_SECS,
        Some(seconds) => seconds,
    }
}

pub fn auto_timeout(timeout_secs: u64) -> u64 {
    if timeout_secs == 0 {
        DEFAULT_AUTO_TIMEOUT_SECS
    } else {
        timeout_secs
    }
}

/// Omitted CLI (`None`) leaves `planned`. `Some(0)` is the 400 ceiling.
/// Any other `Some` is `min(planned, cap)`.
pub fn cap_auto_step_timeout(planned: u64, cli: Option<u64>) -> u64 {
    match cli {
        None => planned,
        Some(seconds) => planned.min(auto_timeout(seconds)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_auto_step_timeout_cases() {
        assert_eq!(cap_auto_step_timeout(60, None), 60);
        assert_eq!(cap_auto_step_timeout(400, None), 400);
        assert_eq!(cap_auto_step_timeout(900, None), 900);
        assert_eq!(cap_auto_step_timeout(60, Some(600)), 60);
        assert_eq!(cap_auto_step_timeout(400, Some(600)), 400);
        assert_eq!(cap_auto_step_timeout(900, Some(600)), 600);
        assert_eq!(cap_auto_step_timeout(60, Some(25)), 25);
        assert_eq!(cap_auto_step_timeout(400, Some(25)), 25);
        assert_eq!(cap_auto_step_timeout(60, Some(0)), 60);
        assert_eq!(cap_auto_step_timeout(400, Some(0)), 400);
        assert_eq!(cap_auto_step_timeout(900, Some(0)), 400);
        assert_eq!(manual_timeout_from_cli(None), 600);
        assert_eq!(manual_timeout_from_cli(Some(0)), 0);
        assert_eq!(manual_timeout_from_cli(Some(25)), 25);
    }
}
