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

use std::collections::VecDeque;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crate::poller::{IoFail, IoType, Poll};

/// Manager for a set of reactor which are polled for an event loop by the
/// re-actor by using [`popol`] library.
pub struct Poller {
    poll: popol::Poll<RawFd>,
    events: VecDeque<(RawFd, Result<IoType, IoFail>)>,
}

impl Poller {
    pub fn new() -> Self {
        Self {
            poll: popol::Poll::new(),
            events: empty!(),
        }
    }
}

impl Poll for Poller {
    fn register(&mut self, fd: &impl AsRawFd, interest: IoType) {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Registering {}", fd.as_raw_fd());
        self.poll.register(fd.as_raw_fd(), fd, interest.into());
    }

    fn unregister(&mut self, fd: &impl AsRawFd) {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Unregistering {}", fd.as_raw_fd());
        self.poll.unregister(&fd.as_raw_fd());
    }

    fn set_interest(&mut self, fd: &impl AsRawFd, interest: IoType) -> bool {
        let fd = fd.as_raw_fd();

        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Setting interest `{interest}` on {}", fd);

        self.poll.unset(&fd, (!interest).into());
        self.poll.set(&fd, interest.into())
    }

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        let len = self.events.len();

        #[cfg(feature = "log")]
        log::trace!(target: "popol",
            "Polling {} reactor with timeout {timeout:?} (pending event queue is {len})",
            self.poll.len(),
        );

        // Blocking call
        if self.poll.wait_timeout(timeout.into())? {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Poll timed out with zero events generated");
            return Ok(0);
        }

        for (fd, fired) in self.poll.events() {
            let res = if fired.has_hangup() {
                Err(IoFail::Connectivity(fired.fired_events()))
            } else if fired.is_err() {
                Err(IoFail::Os(fired.fired_events()))
            } else {
                Ok(IoType {
                    read: fired.is_readable(),
                    write: fired.is_writable(),
                })
            };
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Got `{res:?}` for {fd}");
            self.events.push_back((*fd, res))
        }

        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Poll resulted in {} new event(s)", self.events.len() - len);

        Ok(self.events.len() - len)
    }
}

impl Iterator for Poller {
    type Item = (RawFd, Result<IoType, IoFail>);

    fn next(&mut self) -> Option<Self::Item> {
        match self.events.pop_front() {
            Some((fd, Ok(io))) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popped event `{io}` for {fd} from the queue");
                Some((fd, Ok(io)))
            }
            Some((fd, Err(err))) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popped error `{err}` for {fd} from the queue");
                Some((fd, Err(err)))
            }
            None => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popol queue emptied");
                None
            }
        }
    }
}

impl From<IoType> for popol::PollEvents {
    fn from(ev: IoType) -> Self {
        let mut e = popol::event::NONE;
        if ev.read {
            e |= popol::event::READ;
        }
        if ev.write {
            e |= popol::event::WRITE;
        }
        e
    }
}
