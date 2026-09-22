pub const DEFAULT_AUTO_TIMEOUT_SECS: u64 = 400;

pub fn manual_timeout(timeout_secs: u64) -> u64 {
    timeout_secs
}

pub fn auto_timeout(timeout_secs: u64) -> u64 {
    if timeout_secs == 0 {
        DEFAULT_AUTO_TIMEOUT_SECS
    } else {
        timeout_secs
    }
}

/// `min(planned, auto_timeout(cli))`. CLI `0` is the 400 ceiling, not off.
pub fn cap_auto_step_timeout(planned: u64, cli: u64) -> u64 {
    planned.min(auto_timeout(cli))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_auto_step_timeout_cases() {
        assert_eq!(cap_auto_step_timeout(60, 600), 60);
        assert_eq!(cap_auto_step_timeout(400, 600), 400);
        assert_eq!(cap_auto_step_timeout(60, 25), 25);
        assert_eq!(cap_auto_step_timeout(400, 25), 25);
        assert_eq!(cap_auto_step_timeout(60, 0), 60);
        assert_eq!(cap_auto_step_timeout(400, 0), 400);
    }
}
