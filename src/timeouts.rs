// Library for concurrent I/O resource management using reactor pattern.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2021-2025 by
//     Dr. Maxim Orlovsky <orlovsky@ubideco.org>
//     Alexis Sellier <alexis@cloudhead.io>
//
// Copyright 2022-2025 UBIDECO Labs, InDCS, Lugano, Switzerland. All Rights reserved.
// Copyright 2021-2023 Alexis Sellier <alexis@cloudhead.io>. All Rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except
// in compliance with the License. You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software distributed under the License
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express
// or implied. See the License for the specific language governing permissions and limitations under
// the License.

use std::collections::BTreeSet;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::{Duration, SystemTime};

/// UNIX timestamp which helps working with absolute time.
#[derive(Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, From)]
#[wrapper(Display, LowerHex, UpperHex, Octal, Add, Sub)]
#[wrapper_mut(AddAssign, SubAssign)]
pub struct Timestamp(u128);

impl Timestamp {
    /// Creates timestamp matching the current moment.
    pub fn now() -> Self {
        let duration =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).expect("system time");
        Self(duration.as_millis())
    }

    /// Constructs timestamp from a given number of seconds since [`SystemTime::UNIX_EPOCH`].
    pub fn from_secs(secs: u64) -> Timestamp { Timestamp(secs as u128 * 1000) }

    /// Constructs timestamp from a given number of milliseconds since [`SystemTime::UNIX_EPOCH`].
    pub fn from_millis(millis: u128) -> Timestamp { Timestamp(millis) }

    #[deprecated(note = "use Timestamp::as_secs")]
    /// Returns number of seconds since UNIX epoch.
    pub fn into_secs(self) -> u64 { self.as_secs() }

    /// Returns number of seconds since UNIX epoch.
    pub fn as_secs(&self) -> u64 { (self.0 / 1000) as u64 }

    /// Returns number of milliseconds since UNIX epoch.
    pub fn as_millis(&self) -> u64 {
        // Nb. We have enough space in a `u64` to store a unix timestamp in millisecond
        // precision for millions of years.
        self.0 as u64
    }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, rhs: Duration) -> Self::Output { Timestamp(self.0 + rhs.as_millis()) }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, rhs: Duration) -> Self::Output { Timestamp(self.0 - rhs.as_millis()) }
}

impl AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.as_millis() }
}

impl SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: Duration) { self.0 -= rhs.as_millis() }
}

/// Manages timers and triggers timeouts.
#[derive(Debug, Default)]
pub struct Timer {
    /// Timeouts are durations since the UNIX epoch.
    timeouts: BTreeSet<Timestamp>,
}

impl Timer {
    /// Create a new timer containing no timeouts.
    pub fn new() -> Self { Self { timeouts: bset! {} } }

    /// Return the number of timeouts being tracked.
    pub fn count(&self) -> usize { self.timeouts.len() }

    /// Check whether there are timeouts being tracked.
    pub fn has_timeouts(&self) -> bool { !self.timeouts.is_empty() }

    /// Register a new timeout relative to a certain point in time.
    pub fn set_timeout(&mut self, timeout: Duration, after: Timestamp) {
        let time = after + Timestamp(timeout.as_millis());
        self.timeouts.insert(time);
    }

    /// Get the first timeout expiring right at or after certain moment of time.
    /// Returns `None` if there are no timeouts.
    ///
    /// ```
    /// # use std::time::{Duration};
    /// use reactor::{Timer, Timestamp};
    ///
    /// let mut tm = Timer::new();
    ///
    /// let now = Timestamp::now();
    /// tm.set_timeout(Duration::from_secs(16), now);
    /// tm.set_timeout(Duration::from_secs(8), now);
    /// tm.set_timeout(Duration::from_secs(64), now);
    ///
    /// let mut now = Timestamp::now();
    /// // We need to wait 8 secs to trigger the next timeout (1).
    /// assert!(tm.next_expiring_from(now) <= Some(Duration::from_secs(8)));
    ///
    /// // ... sleep for a sec ...
    /// now += Duration::from_secs(1);
    ///
    /// // Now we don't need to wait as long!
    /// assert!(tm.next_expiring_from(now).unwrap() <= Duration::from_secs(7));
    /// ```
    pub fn next_expiring_from(&self, time: impl Into<Timestamp>) -> Option<Duration> {
        let time = time.into();
        let last = *self.timeouts.first()?;
        Some(if last >= time {
            Duration::from_millis(last.as_millis() - time.as_millis())
        } else {
            Duration::from_secs(0)
        })
    }

    /// Removes timeouts which expire by a certain moment of time (inclusive),
    /// returning total number of timeouts which were removed.
    pub fn remove_expired_by(&mut self, time: Timestamp) -> usize {
        // Since `split_off` returns everything *after* the given key, including the key,
        // if a timer is set for exactly the given time, it would remain in the "after"
        // set of unexpired keys. This isn't what we want, therefore we add `1` to the
        // given time value so that it is put in the "before" set that gets expired
        // and overwritten.
        let at = time + Timestamp::from_millis(1);
        let unexpired = self.timeouts.split_off(&at);
        let fired = self.timeouts.len();
        self.timeouts = unexpired;
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wake_exact() {
        let mut tm = Timer::new();

        let now = Timestamp::now();
        tm.set_timeout(Duration::from_secs(8), now);
        tm.set_timeout(Duration::from_secs(9), now);
        tm.set_timeout(Duration::from_secs(10), now);

        assert_eq!(tm.remove_expired_by(now + Duration::from_secs(9)), 2);
        assert_eq!(tm.count(), 1);
    }

    #[test]
    fn test_wake() {
        let mut tm = Timer::new();

        let now = Timestamp::now();
        tm.set_timeout(Duration::from_secs(8), now);
        tm.set_timeout(Duration::from_secs(16), now);
        tm.set_timeout(Duration::from_secs(64), now);
        tm.set_timeout(Duration::from_secs(72), now);

        assert_eq!(tm.remove_expired_by(now), 0);
        assert_eq!(tm.count(), 4);

        assert_eq!(tm.remove_expired_by(now + Duration::from_secs(9)), 1);
        assert_eq!(tm.count(), 3, "one timeout has expired");

        assert_eq!(tm.remove_expired_by(now + Duration::from_secs(66)), 2);
        assert_eq!(tm.count(), 1, "another two timeouts have expired");

        assert_eq!(tm.remove_expired_by(now + Duration::from_secs(96)), 1);
        assert!(!tm.has_timeouts(), "all timeouts have expired");
    }

    #[test]
    fn test_next() {
        let mut tm = Timer::new();

        let mut now = Timestamp::now();
        tm.set_timeout(Duration::from_secs(3), now);
        assert_eq!(tm.next_expiring_from(now), Some(Duration::from_secs(3)));

        now += Duration::from_secs(2);
        assert_eq!(tm.next_expiring_from(now), Some(Duration::from_secs(1)));

        now += Duration::from_secs(1);
        assert_eq!(tm.next_expiring_from(now), Some(Duration::from_secs(0)));

        now += Duration::from_secs(1);
        assert_eq!(tm.next_expiring_from(now), Some(Duration::from_secs(0)));

        assert_eq!(tm.remove_expired_by(now), 1);
        assert_eq!(tm.count(), 0);
        assert_eq!(tm.next_expiring_from(now), None);
    }
}
