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

use std::collections::BTreeMap;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::{Duration, SystemTime};

/// UNIX timestamp which helps working with absolute time.
#[derive(Wrapper, WrapperMut, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug, From)]
#[wrapper(Display, Hex, Octal, Add, Sub)]
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
#[derive(Debug)]
pub struct Timer<K = ()> {
    /// Timeouts are durations since the UNIX epoch.
    timeouts: BTreeMap<Timestamp, K>,
    /// Threshold below which a timeout can't be added if another timeout is set
    /// within the range of the threshold.
    threshold: Duration,
}

impl<K> Timer<K> {
    /// Create a new timeout manager.
    ///
    /// Takes a threshold below which two timeouts cannot overlap.
    pub fn new(threshold: Duration) -> Self {
        Self {
            timeouts: bmap! {},
            threshold,
        }
    }

    /// Return the number of timeouts being tracked.
    pub fn len(&self) -> usize { self.timeouts.len() }

    /// Check whether there are timeouts being tracked.
    pub fn is_empty(&self) -> bool { self.timeouts.is_empty() }

    /// Register a new timeout with an associated key and wake-up time from a
    /// UNIX time epoch.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use reactor::{Timer, Timestamp};
    ///
    /// let mut tm = Timer::new(Duration::from_secs(1));
    /// let now = Timestamp::now();
    ///
    /// // Sets timeout from Instant::now();
    /// let registered = tm.set_timer(0xA, Duration::from_secs(8), now);
    /// assert!(registered);
    ///
    /// let registered = tm.set_timer(0xB, Duration::from_secs(9), now);
    /// assert!(registered);
    /// assert_eq!(tm.len(), 2);
    ///
    /// let registered = tm.set_timer(0xC, Duration::from_millis(9541), now);
    /// assert!(!registered);
    ///
    /// let registered = tm.set_timer(0xC, Duration::from_millis(9999), now);
    /// assert!(!registered);
    /// assert_eq!(tm.len(), 2);
    /// ```
    pub fn set_timer(&mut self, key: K, span: Duration, after: Timestamp) -> bool {
        let time = after + Timestamp(span.as_secs());
        // If this timeout is too close to a pre-existing timeout,
        // don't register it.
        if self.timeouts.keys().any(|t| {
            if *t < time {
                time - *t < self.threshold.as_secs().into()
            } else {
                *t - time < self.threshold.as_secs().into()
            }
        }) {
            return false;
        }

        self.timeouts.insert(time, key);

        true
    }

    /// Get the minimum time duration we should wait for at least one timeout
    /// to be reached.  Returns `None` if there are no timeouts.
    ///
    /// ```
    /// # use std::time::{Duration};
    /// use reactor::{Timer, Timestamp};
    ///
    /// let mut tm = Timer::new(Duration::from_secs(0));
    ///
    /// let now = Timestamp::now();
    /// tm.set_timer(0xA, Duration::from_secs(16), now);
    /// tm.set_timer(0xB, Duration::from_secs(8), now);
    /// tm.set_timer(0xC, Duration::from_secs(64), now);
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
            .keys()
            .find(|t| **t >= after)
            .map(|t| Duration::from_secs((*t - after).into_secs()))
    }

    /// Returns number of timers which has fired before certain time.
    pub fn count_fired(&mut self, time: Timestamp) -> usize {
        self.timeouts.iter().take_while(move |(t, _)| **t <= time).count()
    }

    /// Returns iterator over timers which has fired before certain time.
    pub fn iter_fired(&mut self, time: Timestamp) -> impl Iterator<Item = &K> {
        self.timeouts.iter().take_while(move |(t, _)| **t <= time).map(|(_, k)| k)
    }

    /// Returns vector of timers which has fired before certain time.
    pub fn split_fired(mut self, time: Timestamp) -> (Vec<K>, Self) {
        let (fired, timeouts) = self.timeouts.into_iter().partition(move |(t, _)| *t <= time);
        self.timeouts = timeouts;
        (fired.into_values().collect::<Vec<_>>(), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wake() {
        let mut tm = Timer::new(Duration::from_secs(0));

        let now = Timestamp::now();
        tm.set_timer(0xA, Duration::from_secs(8), now);
        tm.set_timer(0xB, Duration::from_secs(16), now);
        tm.set_timer(0xC, Duration::from_secs(64), now);
        tm.set_timer(0xD, Duration::from_secs(72), now);

        assert_eq!(tm.count_fired(now), 0);
        assert_eq!(tm.len(), 4);

        let (fired, tm) = tm.split_fired(now + Duration::from_secs(9));
        assert_eq!(fired, vec![0xA]);
        eprintln!("{:#?}", tm);
        assert_eq!(tm.len(), 3, "one timeout has expired");

        let (fired, tm) = tm.split_fired(now + Duration::from_secs(66));
        assert_eq!(fired, vec![0xB, 0xC]);
        assert_eq!(tm.len(), 1, "another two timeouts have expired");

        let (fired, tm) = tm.split_fired(now + Duration::from_secs(96));
        assert_eq!(fired, vec![0xD]);
        assert!(tm.is_empty(), "all timeouts have expired");
    }
}
