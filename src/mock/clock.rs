//! A clock that only moves when told to, so deadlines can be tested deterministically.

use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Duration, Utc};

use crate::clock::Clock;

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct MockClock {
    now: Mutex<DateTime<Utc>>,
}

impl MockClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = now;
    }

    pub fn advance(&self, by: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clock_only_moves_when_told() {
        let start = DateTime::from_timestamp(1_000, 0).unwrap();
        let clock = MockClock::new(start);
        assert_eq!(clock.now(), start);
        clock.advance(Duration::seconds(30));
        assert_eq!(clock.now(), start + Duration::seconds(30));
        clock.set(start);
        assert_eq!(clock.now(), start);
    }
}
