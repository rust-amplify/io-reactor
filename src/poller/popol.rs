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
use std::io;
use std::io::{Error, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use crate::poller::{IoFail, IoType, Poll, Waker, WakerRecv, WakerSend};

/// Manager for a set of reactor which are polled for an event loop by the
/// re-actor by using [`popol`] library.
pub struct Poller {
    poll: popol::Sources<RawFd>,
    events: VecDeque<popol::Event<RawFd>>,
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
        }
    }

    /// Constructs new [`popol`]-backed poll engine and reserves certain capacity for the resources
    /// which later will be registered for a poll operation.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            poll: popol::Sources::with_capacity(capacity),
            events: VecDeque::with_capacity(capacity),
        }
    }
}

impl Poll for Poller {
    type Waker = UnixStream;

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
    type Item = (RawFd, Result<IoType, IoFail>);

    fn next(&mut self) -> Option<Self::Item> {
        let event = self.events.pop_front()?;

        let fd = event.key;
        let fired = event.raw_events();
        let res = if event.is_hangup() {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Hangup on {fd}");

            Err(IoFail::Connectivity(fired))
        } else if event.is_error() || event.is_invalid() {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "OS error on {fd} (fired events {fired:#b})");

            Err(IoFail::Os(fired))
        } else {
            let io = IoType {
                read: event.is_readable(),
                write: event.is_writable(),
            };

            #[cfg(feature = "log")]
            log::trace!(target: "popol", "I/O event on {fd}: {io}");

            Ok(io)
        };
        Some((fd, res))
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

impl Waker for UnixStream {
    type Send = Arc<UnixStream>;
    type Recv = UnixStream;

    fn pair() -> Result<(Self::Send, Self::Recv), Error> {
        let (waker_writer, waker_reader) = UnixStream::pair()?;
        waker_reader.set_nonblocking(true)?;
        waker_writer.set_nonblocking(true)?;
        Ok((Arc::new(waker_writer), waker_reader))
    }
}

impl WakerRecv for UnixStream {
    fn reset(&self) { reset_fd(self).expect("waker failure"); }
}

impl WakerSend for Arc<UnixStream> {
    fn wake(&self) -> io::Result<()> {
        loop {
            let mut waker = self.as_ref();
            match (&mut waker).write_all(&[0x1]) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor-controller", "Waker write queue got overfilled, resetting and repeating...");
                    reset_fd(&self.as_raw_fd())?;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor-controller", "Waker failure, repeating...");
                }
                Err(e) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor-controller", "Waker error: {e}");

                    return Err(e);
                }
            }
        }
    }
}

fn reset_fd(fd: &impl AsRawFd) -> io::Result<()> {
    let mut buf = [0u8; 4096];

    loop {
        // We use a low-level "read" here because the alternative is to create a `UnixStream`
        // from the `RawFd`, which has "drop" semantics which we want to avoid.
        match unsafe {
            libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        } {
            -1 => match io::Error::last_os_error() {
                e if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                e => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor-controller", "Unable to reset waker queue: {e}");

                    return Err(e);
                }
            },
            0 => return Ok(()),
            _ => continue,
        }
    }
}
