use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Elapsed;

pub fn wait_for_condition<F: Fn() -> bool>(
    check: F,
    timeout: Duration,
    interval: Duration,
) -> Result<(), Elapsed> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
    Err(Elapsed)
}
