pub const DEFAULT_AUTO_TIMEOUT_SECS: u64 = 400;

/// Planned seconds for an auto-plan test step when `[verify] suite_timeout_secs` is absent.
/// 0494 measured the ci and slow commands as non-zero exits, so they were not formula inputs.
/// The doctest wall was 11 seconds (`Keep`). No `Set` value was produced. The constant stays 400.
pub const DEFAULT_TEST_SUITE_TIMEOUT_SECS: u64 = 400;

/// `timeout-minutes: 60` on the CI `test` and `test-slow` jobs.
/// The helper does not read `ci.yml` at runtime.
const TEST_SUITE_CI_JOB_CAP_SECS: u64 = 3600;

/// Result of the 0494 median-plus-25% rule. Runtime verify does not call this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSuiteBudget {
    /// Input is at or under the auto ceiling. Do not raise the built-in.
    Keep,
    /// Rounded budget to ship, in seconds.
    Set(u64),
    /// Rounded figure exceeds the CI job cap. Do not substitute the cap.
    TooBig,
}

/// `Keep` when `median_secs` is at or under 400. Otherwise pad 25%, round up
/// to 30 seconds, and return `TooBig` when that figure exceeds 3600.
pub fn test_suite_budget_from_median(median_secs: u64) -> TestSuiteBudget {
    if median_secs <= DEFAULT_AUTO_TIMEOUT_SECS {
        return TestSuiteBudget::Keep;
    }
    let padded = median_secs.saturating_mul(5).div_ceil(4);
    let rounded = padded.div_ceil(30).saturating_mul(30);
    if rounded > TEST_SUITE_CI_JOB_CAP_SECS {
        TestSuiteBudget::TooBig
    } else {
        TestSuiteBudget::Set(rounded)
    }
}

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
        assert_eq!(
            cap_auto_step_timeout(DEFAULT_TEST_SUITE_TIMEOUT_SECS, None),
            DEFAULT_TEST_SUITE_TIMEOUT_SECS
        );
        assert_eq!(
            cap_auto_step_timeout(DEFAULT_TEST_SUITE_TIMEOUT_SECS, Some(0)),
            DEFAULT_AUTO_TIMEOUT_SECS
        );
        assert_eq!(manual_timeout_from_cli(None), 600);
        assert_eq!(manual_timeout_from_cli(Some(0)), 0);
        assert_eq!(manual_timeout_from_cli(Some(25)), 25);
    }

    #[test]
    fn test_suite_budget_from_median_cases() {
        assert_eq!(test_suite_budget_from_median(400), TestSuiteBudget::Keep);
        assert_eq!(
            test_suite_budget_from_median(401),
            TestSuiteBudget::Set(510)
        );
        assert_eq!(
            test_suite_budget_from_median(1600),
            TestSuiteBudget::Set(2010)
        );
        assert_eq!(
            test_suite_budget_from_median(2880),
            TestSuiteBudget::Set(3600)
        );
        assert_eq!(test_suite_budget_from_median(2881), TestSuiteBudget::TooBig);
        assert_eq!(test_suite_budget_from_median(3500), TestSuiteBudget::TooBig);
    }
}
