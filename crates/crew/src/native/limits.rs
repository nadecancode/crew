use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

struct Bucket {
    since: Instant,
    count: usize,
}

pub(crate) struct Attempts {
    sources: BTreeMap<IpAddr, Bucket>,
    global: Bucket,
    per_source: usize,
    per_global: usize,
}

impl Attempts {
    pub fn new(per_source: usize, per_global: usize) -> Self {
        Self {
            sources: BTreeMap::new(),
            global: Bucket {
                since: Instant::now(),
                count: 0,
            },
            per_source,
            per_global,
        }
    }
    pub fn admit(&mut self, source: IpAddr) -> bool {
        let now = Instant::now();
        let period = Duration::from_secs(60);
        if now.duration_since(self.global.since) >= period {
            self.global = Bucket {
                since: now,
                count: 0,
            };
        }
        self.sources
            .retain(|_, b| now.duration_since(b.since) < period);
        if self.global.count >= self.per_global
            || (!self.sources.contains_key(&source) && self.sources.len() >= 1024)
        {
            return false;
        }
        let bucket = self.sources.entry(source).or_insert(Bucket {
            since: now,
            count: 0,
        });
        if bucket.count >= self.per_source {
            return false;
        }
        bucket.count += 1;
        self.global.count += 1;
        true
    }
    pub fn allowed(&self, source: IpAddr) -> bool {
        let now = Instant::now();
        let period = Duration::from_secs(60);
        (now.duration_since(self.global.since) >= period || self.global.count < self.per_global)
            && self
                .sources
                .get(&source)
                .is_none_or(|b| now.duration_since(b.since) >= period || b.count < self.per_source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attempts_are_bounded_across_addresses() {
        let mut attempts = Attempts::new(2, 3);
        let a = "127.0.0.1".parse().unwrap();
        let b = "127.0.0.2".parse().unwrap();
        assert!(attempts.admit(a));
        assert!(attempts.admit(a));
        assert!(!attempts.admit(a));
        assert!(attempts.admit(b));
        assert!(!attempts.admit(b));
    }
}
