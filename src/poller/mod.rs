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

//! OS and implementation-specific poll engines.

#[cfg(feature = "popol")]
pub mod popol;

use std::fmt::{self, Display, Formatter};
use std::os::unix::io::AsRawFd;
use std::time::Duration;
use std::{io, ops};

use crate::resource::Io;
use crate::ResourceId;

/// Information about I/O events which has happened for a resource.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct IoType {
    /// Specifies whether I/O source has data to read.
    pub read: bool,
    /// Specifies whether I/O source is ready for write operations.
    pub write: bool,
}

impl IoType {
    /// Indicates no I/O operations are tracked.
    pub fn none() -> Self {
        Self {
            read: false,
            write: false,
        }
    }

    /// Indicates interest in only read I/O events.
    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
        }
    }

    /// Indicates interest in only write I/O events.
    pub fn write_only() -> Self {
        Self {
            read: false,
            write: true,
        }
    }

    /// Indicates interest in both read and write I/O events.
    pub fn read_write() -> Self {
        Self {
            read: true,
            write: true,
        }
    }

    /// Indicates no I/O operations has happened on a resource.
    pub fn is_none(self) -> bool { !self.read && !self.write }
    /// Indicates data available to be read from a resource.
    pub fn is_read_only(self) -> bool { self.read && !self.write }
    /// Indicates that the resource is ready to accept data.
    pub fn is_write_only(self) -> bool { !self.read && self.write }
    /// Indicates that the resource can accept data - and has aa data which can be read.
    pub fn is_read_write(self) -> bool { self.read && self.write }
}

impl ops::Not for IoType {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self {
            read: !self.read,
            write: !self.write,
        }
    }
}

impl Iterator for IoType {
    type Item = Io;

    fn next(&mut self) -> Option<Self::Item> {
        if self.write {
            self.write = false;
            Some(Io::Write)
        } else if self.read {
            self.read = false;
            Some(Io::Read)
        } else {
            None
        }
    }
}

impl Display for IoType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.is_none() {
            f.write_str("none")
        } else if self.is_read_write() {
            f.write_str("read-write")
        } else if self.read {
            f.write_str("read")
        } else if self.write {
            f.write_str("write")
        } else {
            unreachable!()
        }
    }
}

/// Reasons for the poll operation failure for a specific resource.
#[derive(Copy, Clone, Debug, Display, Error)]
#[display(doc_comments)]
pub enum IoFail {
    /// connection is absent (POSIX events {0:#b})
    Connectivity(i16),
    /// OS-level error (POSIX events {0:#b})
    Os(i16),
}

/// An engine providing `poll` syscall interface to the [`crate::Reactor`].
///
/// Since `poll` syscalls are platform-dependent and multiple crates can expose it with a different
/// API and tradeoffs, the current library allows selection of a specific poll engine
/// implementation.
///
/// To read I/O events from the engine please use its Iterator interface.
pub trait Poll
where
    Self: Send + Iterator<Item = (ResourceId, Result<IoType, IoFail>)>,
    for<'a> &'a mut Self: Iterator<Item = (ResourceId, Result<IoType, IoFail>)>,
{
    /// Waker type used by the poll provider.
    type Waker: Waker;

    /// Registers a waker object.
    fn register_waker(&mut self, fd: &impl AsRawFd);
    /// Registers a file-descriptor based resource for a poll.
    fn register(&mut self, fd: &impl AsRawFd, interest: IoType) -> ResourceId;
    /// Unregisters a file-descriptor based resource from a poll.
    fn unregister(&mut self, id: ResourceId);
    /// Subscribes for a specific set of events for a given file descriptor-backed resource (see
    /// [`IoType`] for the details on event subscription).
    fn set_interest(&mut self, id: ResourceId, interest: IoType) -> bool;

    /// Runs single poll syscall over all registered resources with an optional timeout.
    ///
    /// # Returns
    ///
    /// Number of generated events.
    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize>;
}

/// Waker object provided by the poller.
pub trait Waker {
    /// Data type for sending wake signals to the poller.
    type Send: WakerSend;
    /// Data type for receiving wake signals inside the poller.
    type Recv: WakerRecv;

    /// Constructs pair of waker receiver and sender objects.
    fn pair() -> Result<(Self::Send, Self::Recv), io::Error>;
}

/// Sending part of the waker.
pub trait WakerSend: Send + Sync + Clone {
    /// Awakes the poller to read events.
    fn wake(&self) -> io::Result<()>;
}

/// Receiver part of the waker.
pub trait WakerRecv: AsRawFd + Send + io::Read {
    /// Resets the waker reader.
    fn reset(&self);
}
