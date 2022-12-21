use std::time::{Duration, Instant};

/// Manages timers and triggers timeouts.
#[derive(Debug)]
pub struct TimeoutManager<K = ()> {
    timeouts: Vec<(K, Instant)>,
    threshold: Duration,
}

impl<K> TimeoutManager<K> {
    /// Create a new timeout manager.
    ///
    /// Takes a threshold below which two timeouts cannot overlap.
    pub fn new(threshold: Duration) -> Self {
        Self {
            timeouts: vec![],
            threshold,
        }
    }

    /// Return the number of timeouts being tracked.
    pub fn len(&self) -> usize {
        self.timeouts.len()
    }

    /// Check whether there are timeouts being tracked.
    pub fn is_empty(&self) -> bool {
        self.timeouts.is_empty()
    }

    /// Register a new timeout with an associated key and wake-up time.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    /// use reactor::TimeoutManager;
    ///
    /// let mut tm = TimeoutManager::new(Duration::from_secs(1));
    /// let now = Instant::now();
    ///
    /// let registered = tm.register(0xA, now + Duration::from_secs(8));
    /// assert!(registered);
    ///
    /// let registered = tm.register(0xB, now + Duration::from_secs(9));
    /// assert!(registered);
    /// assert_eq!(tm.len(), 2);
    ///
    /// let registered = tm.register(0xC, now + Duration::from_millis(9541));
    /// assert!(!registered);
    ///
    /// let registered = tm.register(0xC, now + Duration::from_millis(9999));
    /// assert!(!registered);
    /// assert_eq!(tm.len(), 2);
    /// ```
    pub fn register(&mut self, key: K, time: Instant) -> bool {
        // If this timeout is too close to a pre-existing timeout,
        // don't register it.
        if self.timeouts.iter().any(|(_, t)| {
            if *t < time {
                time.duration_since(*t) < self.threshold
            } else {
                t.duration_since(time) < self.threshold
            }
        }) {
            return false;
        }

        self.timeouts.push((key, time));
        self.timeouts.sort_unstable_by(|(_, a), (_, b)| b.cmp(a));

        true
    }

    /// Get the minimum time duration we should wait for at least one timeout
    /// to be reached.  Returns `None` if there are no timeouts.
    ///
    /// ```
    /// # use std::time::{Duration, Instant};
    /// use reactor::TimeoutManager;
    ///
    /// let mut tm = TimeoutManager::new(Duration::from_secs(0));
    /// let mut now = Instant::now();
    ///
    /// tm.register(0xA, now + Duration::from_millis(16));
    /// tm.register(0xB, now + Duration::from_millis(8));
    /// tm.register(0xC, now + Duration::from_millis(64));
    ///
    /// // We need to wait 8 millis to trigger the next timeout (1).
    /// assert!(tm.next(now) <= Some(Duration::from_millis(8)));
    ///
    /// // ... sleep for a millisecond ...
    /// now += Duration::from_millis(1);
    ///
    /// // Now we don't need to wait as long!
    /// assert!(tm.next(now).unwrap() <= Duration::from_millis(7));
    /// ```
    pub fn next(&self, now: impl Into<Instant>) -> Option<Duration> {
        let now = now.into();

        self.timeouts.last().map(|(_, t)| {
            if *t >= now {
                *t - now
            } else {
                Duration::from_secs(0)
            }
        })
    }

    /// Given a specific time, add to the input vector keys that
    /// have timed out by that time. Returns the number of keys that timed out.
    pub fn check(&mut self, time: Instant, fired: &mut Vec<K>) -> usize {
        let before = fired.len();

        while let Some((k, t)) = self.timeouts.pop() {
            if time >= t {
                fired.push(k);
            } else {
                self.timeouts.push((k, t));
                break;
            }
        }
        fired.len() - before
    }

    /// Given the current time, add to the input vector keys that
    /// have timed out. Returns the number of keys that timed out.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    /// use reactor::TimeoutManager;
    ///
    /// let mut tm = TimeoutManager::new(Duration::from_secs(0));
    /// let now = Instant::now();
    ///
    /// tm.register(0xA, now + Duration::from_millis(8));
    /// tm.register(0xB, now + Duration::from_millis(16));
    /// tm.register(0xC, now + Duration::from_millis(64));
    /// tm.register(0xD, now + Duration::from_millis(72));
    ///
    /// let mut timeouts = Vec::new();
    ///
    /// assert_eq!(tm.check(now + Duration::from_millis(21), &mut timeouts), 2);
    /// assert_eq!(timeouts, vec![0xA, 0xB]);
    /// assert_eq!(tm.len(), 2);
    /// ```
    pub fn check_now(&mut self, fired: &mut Vec<K>) -> usize {
        self.check(Instant::now(), fired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wake() {
        let mut tm = TimeoutManager::new(Duration::from_secs(0));
        let now = Instant::now();

        tm.register(0xA, now + Duration::from_millis(8));
        tm.register(0xB, now + Duration::from_millis(16));
        tm.register(0xC, now + Duration::from_millis(64));
        tm.register(0xD, now + Duration::from_millis(72));

        let mut timeouts = Vec::new();

        assert_eq!(tm.check(now, &mut timeouts), 0);
        assert_eq!(timeouts, vec![]);
        assert_eq!(tm.len(), 4);
        assert_eq!(tm.check(now + Duration::from_millis(9), &mut timeouts), 1);
        assert_eq!(timeouts, vec![0xA]);
        assert_eq!(tm.len(), 3, "one timeout has expired");

        timeouts.clear();

        assert_eq!(tm.check(now + Duration::from_millis(66), &mut timeouts), 2);
        assert_eq!(timeouts, vec![0xB, 0xC]);
        assert_eq!(tm.len(), 1, "another two timeouts have expired");

        timeouts.clear();

        assert_eq!(tm.check(now + Duration::from_millis(96), &mut timeouts), 1);
        assert_eq!(timeouts, vec![0xD]);
        assert!(tm.is_empty(), "all timeouts have expired");
    }
}
