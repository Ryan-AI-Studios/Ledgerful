use chrono::{DateTime, Duration, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    fn relative_time(&self, past: DateTime<Utc>) -> String {
        let now = self.now();
        let diff = now.signed_duration_since(past);

        if diff < Duration::seconds(60) {
            "just now".to_string()
        } else if diff < Duration::minutes(60) {
            let mins = diff.num_minutes();
            format!("{}m ago", mins)
        } else if diff < Duration::hours(24) {
            let hours = diff.num_hours();
            format!("{}h ago", hours)
        } else {
            let days = diff.num_days();
            format!("{}d ago", days)
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Clone)]
pub struct FixedClock {
    fixed: DateTime<Utc>,
}

impl FixedClock {
    pub fn new(fixed: DateTime<Utc>) -> Self {
        Self { fixed }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.fixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_clock_returns_fixed_timestamp() {
        let fixed = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let clock = FixedClock::new(fixed);
        assert_eq!(clock.now(), fixed);
    }

    #[test]
    fn test_relative_time() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let clock = FixedClock::new(now);

        assert_eq!(clock.relative_time(now - Duration::seconds(30)), "just now");
        assert_eq!(clock.relative_time(now - Duration::minutes(5)), "5m ago");
        assert_eq!(clock.relative_time(now - Duration::hours(3)), "3h ago");
        assert_eq!(clock.relative_time(now - Duration::days(2)), "2d ago");
    }
}
