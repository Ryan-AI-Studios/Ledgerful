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
