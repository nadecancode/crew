use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crew_client::api::ClockSample;

/// Every channel, deadline and core call in a native session uses this origin.
/// Tests inject a clock; platform UI code does not provide monotonic samples.
pub trait Clock: Send + Sync {
    fn sample(&self) -> ClockSample;
}

pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn sample(&self) -> ClockSample {
        let wall_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => {
                i64::try_from(error.duration().as_millis()).map_or(i64::MIN, |value| -value)
            }
        };
        ClockSample {
            wall_ms,
            monotonic_ms: u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}
