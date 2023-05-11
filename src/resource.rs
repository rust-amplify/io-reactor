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

use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::RawFd;
use std::{io, net};

use crate::poller::IoType;

/// I/O events which can be subscribed for - or notified about by the [`crate::Reactor`] on a
/// specific [`Resource`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Io {
    /// Input event
    Read,
    /// Output event
    Write,
}

/// Marker traits for types which can be used as a reactor-managed [`Resource`] identifiers.
pub trait ResourceId: Copy + Eq + Ord + Hash + Send + Debug + Display {}

/// A resource which can be managed by the reactor.
pub trait Resource: AsRawFd + WriteAtomic + Send {
    /// Resource identifier type.
    type Id: ResourceId;
    /// Events which resource may generate upon receiving I/O from the reactor via
    /// [`Self::handle_io`]. These events are passed to the reactor [`crate::Handler`].
    type Event;

    /// Method returning the [`ResourceId`].
    fn id(&self) -> Self::Id;
    /// Method informing the reactor which types of events this resource is subscribed for.
    fn interests(&self) -> IoType;

    /// Method called by the reactor when an I/O event is received for this resource in a result of
    /// poll syscall.
    fn handle_io(&mut self, io: Io) -> Option<Self::Event>;
}

impl ResourceId for net::SocketAddr {}
impl ResourceId for RawFd {}

/// Error during write operation for a reactor-managed [`Resource`].
#[derive(Debug, Display, Error, From)]
pub enum WriteError {
    /// Underlying resource is not ready to accept the data: for instance,
    /// a connection has not yet established in full or handshake is not
    /// complete. A specific case in which this error is returned is defined
    /// by an underlying resource type; however, this error happens only
    /// due to a business logic bugs in a [`crate::reactor::Handler`]
    /// implementation.
    #[display("resource not ready to accept the data")]
    NotReady,

    /// Error returned by the operation system and not by the resource itself.
    #[display(inner)]
    #[from]
    Io(io::Error),
}

/// The trait guarantees that the data are either written in full - or, in case
/// of an error, none of the data is written. Types implementing the trait must
/// also guarantee that multiple attempts to do the write would not result in
/// data to be written out of the initial ordering.
pub trait WriteAtomic: io::Write {
    /// Atomic non-blocking I/O write operation, which must either write the whole buffer to a
    /// resource without blocking - or fail with [`WriteError::NotReady`] error.
    ///
    /// # Safety
    ///
    /// Panics on invalid [`WriteAtomic::write_or_buf`] implementation, i.e. if it doesn't handle
    /// EGAGAIN, EINTER, EWOULDBLOCK I/O errors by buffering the data and returns them instead.
    fn write_atomic(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        if !self.is_ready_to_write() {
            Err(WriteError::NotReady)
        } else {
            // TODO: on EGAGAIN, EINTER, EWOULDBLOCK just keep the data buffered
            self.write_or_buf(buf).map_err(|err| {
                debug_assert!(
                    matches!(
                        err.kind(),
                        ErrorKind::WouldBlock | ErrorKind::Interrupted | ErrorKind::WriteZero
                    ),
                    "WriteAtomic::write_or_buf must handle EGAGAIN, EINTER, EWOULDBLOCK errors by \
                     buffering the data"
                );
                WriteError::from(err)
            })
        }
    }

    /// Checks whether resource can be written to without blocking.
    fn is_ready_to_write(&self) -> bool;

    /// Empties any write buffers in a non-blocking way. If a non-blocking
    /// operation is not possible, errors with [`io::ErrorKind::WouldBlock`]
    /// kind of [`io::Error`].
    ///
    /// # Returns
    ///
    /// If the buffer contained any data before this operation.
    fn empty_write_buf(&mut self) -> io::Result<bool>;

    #[doc = hidden]
    /// Writes to the resource in a non-blocking way, buffering the data if necessary - or failing
    /// with a system-level error.
    ///
    /// This method shouldn't be called directly; [`Self::write_atomic`] must be used instead.
    ///
    /// # Safety
    ///
    /// The method must handle EGAGAIN, EINTER, EWOULDBLOCK I/O errors and buffer the data in such
    /// cases. Ig these errors are returned from this methods [`WriteAtomic::write_atomic`] will
    /// panic.
    fn write_or_buf(&mut self, buf: &[u8]) -> io::Result<()>;
}
