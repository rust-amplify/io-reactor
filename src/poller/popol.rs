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

//! Poll engine provided by the [`popol`] crate.

use std::collections::VecDeque;
use std::io::{self, Error};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;
use std::time::Duration;

use crate::poller::{IoFail, IoType, Poll, Waker, WakerRecv, WakerSend};
use crate::ResourceId;

/// Manager for a set of reactor which are polled for an event loop by the
/// re-actor by using [`popol`] library.
pub struct Poller {
    poll: popol::Sources<ResourceId>,
    events: VecDeque<popol::Event<ResourceId>>,
    id_top: ResourceId,
}

impl Default for Poller {
    fn default() -> Self { Self::new() }
}

impl Poller {
    /// Constructs new [`popol`]-backed poll engine.
    pub fn new() -> Self {
        Self {
            poll: popol::Sources::new(),
            events: empty!(),
            id_top: 0,
        }
    }

    /// Constructs new [`popol`]-backed poll engine and reserves certain capacity for the resources
    /// which later will be registered for a poll operation.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            poll: popol::Sources::with_capacity(capacity),
            events: VecDeque::with_capacity(capacity),
            id_top: 0,
        }
    }
}

impl Poll for Poller {
    type Waker = PopolWaker;

    fn register(&mut self, fd: &impl AsRawFd, interest: IoType) -> ResourceId {
        let id = self.id_top;
        self.id_top += 1;

        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Registering file descriptor {} as resource with id {}", fd.as_raw_fd(), id);

        self.poll.register(id, fd, interest.into());
        id
    }

    fn unregister(&mut self, id: ResourceId) {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Unregistering {}", id);
        self.poll.unregister(&id);
    }

    fn set_interest(&mut self, id: ResourceId, interest: IoType) -> bool {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Setting interest `{interest}` on {}", id);

        self.poll.unset(&id, (!interest).into());
        self.poll.set(&id, interest.into())
    }

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        #[cfg(feature = "log")]
        log::trace!(target: "popol",
            "Polling {} reactor resources with timeout {timeout:?} (pending event queue is {})",
            self.poll.len(), self.events.len()
        );

        // Blocking call
        match self.poll.poll(&mut self.events, timeout) {
            Ok(count) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Poll resulted in {} new event(s)", count);
                Ok(count)
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Poll timed out with zero events generated");
                Ok(0)
            }
            Err(err) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Poll resulted in error: {err}");
                Err(err)
            }
        }
    }
}

impl Iterator for Poller {
    type Item = (ResourceId, Result<IoType, IoFail>);

    fn next(&mut self) -> Option<Self::Item> {
        let event = self.events.pop_front()?;

        let id = event.key;
        let fired = event.raw_events();
        let res = if event.is_hangup() {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Hangup on {id}");

            Err(IoFail::Connectivity(fired))
        } else if event.is_error() || event.is_invalid() {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "OS error on {id} (fired events {fired:#b})");

            Err(IoFail::Os(fired))
        } else {
            let io = IoType {
                read: event.is_readable(),
                write: event.is_writable(),
            };

            #[cfg(feature = "log")]
            log::trace!(target: "popol", "I/O event on {id}: {io}");

            Ok(io)
        };
        Some((id, res))
    }
}

impl From<IoType> for popol::Interest {
    fn from(ev: IoType) -> Self {
        let mut e = popol::interest::NONE;
        if ev.read {
            e |= popol::interest::READ;
        }
        if ev.write {
            e |= popol::interest::WRITE;
        }
        e
    }
}

/// Wrapper type around the waker provided by `popol` crate.
#[derive(Clone)]
pub struct PopolWaker(Arc<popol::Waker>);

impl Waker for PopolWaker {
    type Send = Self;
    type Recv = Self;

    fn pair() -> Result<(Self::Send, Self::Recv), Error> {
        let waker = Arc::new(popol::Waker::new()?);
        Ok((PopolWaker(waker.clone()), PopolWaker(waker)))
    }
}

impl io::Read for PopolWaker {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        self.reset();
        // Waker reads only when there is something which was sent.
        // That's why we just return here.
        Ok(0)
    }
}

impl AsRawFd for PopolWaker {
    fn as_raw_fd(&self) -> RawFd { self.0.as_ref().as_raw_fd() }
}

impl WakerRecv for PopolWaker {
    fn reset(&self) {
        if let Err(e) = popol::Waker::reset(self.0.as_ref()) {
            #[cfg(feature = "log")]
            log::error!(target: "reactor-controller", "Unable to reset waker queue: {e}");
            panic!("unable to reset waker queue. Details: {e}");
        }
    }
}

impl WakerSend for PopolWaker {
    fn wake(&self) -> io::Result<()> { self.0.wake() }
}
