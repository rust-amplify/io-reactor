// Library for concurrent I/O resource management using reactor pattern.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2021-2023 by
//     Dr. Maxim Orlovsky <orlovsky@ubideco.org>
//     Alexis Sellier <alexis@cloudhead.io>
//
// Copyright 2022-2023 UBIDECO Institute, Switzerland
// Copyright 2021 Alexis Sellier <alexis@cloudhead.io>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::BTreeSet;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::{Duration, SystemTime};

/// UNIX timestamp which helps working with absolute time.
#[derive(Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, From)]
#[wrapper(Display, LowerHex, UpperHex, Octal, Add, Sub)]
#[wrapper_mut(AddAssign, SubAssign)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Creates timestamp matching the current moment.
    pub fn now() -> Self {
        let duration =
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).expect("system time");
        Self(duration.as_secs())
    }

    /// Converts into number of seconds since UNIX epoch.
    pub fn into_secs(self) -> u64 { self.0 }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;

    fn add(self, rhs: Duration) -> Self::Output { Timestamp(self.0 + rhs.as_secs()) }
}

impl Sub<Duration> for Timestamp {
    type Output = Timestamp;

    fn sub(self, rhs: Duration) -> Self::Output { Timestamp(self.0 - rhs.as_secs()) }
}

impl AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) { self.0 += rhs.as_secs() }
}

impl SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: Duration) { self.0 -= rhs.as_secs() }
}

/// Manages timers and triggers timeouts.
#[derive(Debug, Default)]
pub struct Timer {
    /// Timeouts are durations since the UNIX epoch.
    timeouts: BTreeSet<Timestamp>,
}

impl Timer {
    /// Create a new timeout manager.
    ///
    /// Takes a threshold below which two timeouts cannot overlap.
    pub fn new() -> Self { Self { timeouts: bset! {} } }

    /// Return the number of timeouts being tracked.
    pub fn len(&self) -> usize { self.timeouts.len() }

    /// Check whether there are timeouts being tracked.
    pub fn is_empty(&self) -> bool { self.timeouts.is_empty() }

    /// Register a new timeout with an associated key and wake-up time from a
    /// UNIX time epoch.
    pub fn set_timer(&mut self, span: Duration, after: Timestamp) {
        let time = after + Timestamp(span.as_secs());
        self.timeouts.insert(time);
    }

    /// Get the minimum time duration we should wait for at least one timeout
    /// to be reached.  Returns `None` if there are no timeouts.
    ///
    /// ```
    /// # use std::time::{Duration};
    /// use reactor::{Timer, Timestamp};
    ///
    /// let mut tm = Timer::new();
    ///
    /// let now = Timestamp::now();
    /// tm.set_timer(Duration::from_secs(16), now);
    /// tm.set_timer(Duration::from_secs(8), now);
    /// tm.set_timer(Duration::from_secs(64), now);
    ///
    /// let mut now = Timestamp::now();
    /// // We need to wait 8 secs to trigger the next timeout (1).
    /// assert!(tm.next(now) <= Some(Duration::from_secs(8)));
    ///
    /// // ... sleep for a sec ...
    /// now += Duration::from_secs(1);
    ///
    /// // Now we don't need to wait as long!
    /// assert!(tm.next(now).unwrap() <= Duration::from_secs(7));
    /// ```
    pub fn next(&self, after: impl Into<Timestamp>) -> Option<Duration> {
        let after = after.into();
        self.timeouts
            .iter()
            .find(|t| **t >= after)
            .map(|t| Duration::from_secs((*t - after).into_secs()))
    }

    /// Returns vector of timers which has fired before certain time.
    pub fn expire(&mut self, time: Timestamp) -> usize {
        // Since `split_off` returns everything *after* the given key, including the key,
        // if a timer is set for exactly the given time, it would remain in the "after"
        // set of unexpired keys. This isn't what we want, therefore we add `1` to the
        // given time value so that it is put in the "before" set that gets expired
        // and overwritten.
        let at = Timestamp(time.0 + 1);
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
        tm.set_timer(Duration::from_secs(8), now);
        tm.set_timer(Duration::from_secs(9), now);
        tm.set_timer(Duration::from_secs(10), now);

        assert_eq!(tm.expire(now + Duration::from_secs(9)), 2);
        assert_eq!(tm.len(), 1);
    }

    #[test]
    fn test_wake() {
        let mut tm = Timer::new();

        let now = Timestamp::now();
        tm.set_timer(Duration::from_secs(8), now);
        tm.set_timer(Duration::from_secs(16), now);
        tm.set_timer(Duration::from_secs(64), now);
        tm.set_timer(Duration::from_secs(72), now);

        assert_eq!(tm.expire(now), 0);
        assert_eq!(tm.len(), 4);

        assert_eq!(tm.expire(now + Duration::from_secs(9)), 1);
        assert_eq!(tm.len(), 3, "one timeout has expired");

        assert_eq!(tm.expire(now + Duration::from_secs(66)), 2);
        assert_eq!(tm.len(), 1, "another two timeouts have expired");

        assert_eq!(tm.expire(now + Duration::from_secs(96)), 1);
        assert!(tm.is_empty(), "all timeouts have expired");
    }
}
