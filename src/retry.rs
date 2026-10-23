use std::time::{Duration, Instant};

/// Automatic STA recovery only. Explicit AP selection cancels this schedule.
pub struct Retry {
    due: Option<Instant>,
    delay: u64,
}

impl Retry {
    pub fn new() -> Self {
        Self {
            due: None,
            delay: 3,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn failed(&mut self, now: Instant) {
        self.due = Some(now + Duration::from_secs(self.delay));
        self.delay = (self.delay * 2).min(60);
    }

    pub fn ready(&self, now: Instant) -> bool {
        self.due.is_some_and(|due| now >= due)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_backoff_and_reset() {
        let mut retry = Retry::new();
        let mut now = Instant::now();
        assert!(!retry.ready(now + Duration::from_secs(1000)));
        for seconds in [3, 6, 12, 24, 48, 60, 60] {
            retry.failed(now);
            assert!(!retry.ready(now + Duration::from_secs(seconds - 1)));
            now += Duration::from_secs(seconds);
            assert!(retry.ready(now));
        }
        retry.reset();
        assert!(!retry.ready(now + Duration::from_secs(1000)));
        retry.failed(now);
        assert!(retry.ready(now + Duration::from_secs(3)));
    }

    #[test]
    fn manual_ap_selection_cancels_pending_attempt() {
        let mut retry = Retry::new();
        let now = Instant::now();
        retry.failed(now);
        retry.reset();
        assert!(!retry.ready(now + Duration::from_secs(60)));
    }

    #[test]
    fn delay_is_measured_after_the_attempt_finishes() {
        let mut retry = Retry::new();
        let now = Instant::now();
        retry.failed(now);
        let finished = now + Duration::from_secs(28);
        retry.failed(finished);
        assert!(!retry.ready(finished + Duration::from_secs(5)));
        assert!(retry.ready(finished + Duration::from_secs(6)));
    }
}
