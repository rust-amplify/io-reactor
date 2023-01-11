use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::RawFd;
use std::{io, net};

use crate::poller::IoType;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Io {
    Read,
    Write,
}

pub trait ResourceId: Copy + Eq + Ord + Hash + Send + Debug + Display {}

pub trait Resource: AsRawFd + WriteAtomic + Send {
    type Id: ResourceId;
    type Event;

    fn id(&self) -> Self::Id;
    fn interests(&self) -> IoType;

    fn handle_io(&mut self, io: Io) -> Option<Self::Event>;

    fn disconnect(self) -> io::Result<()>;
}

impl ResourceId for net::SocketAddr {}
impl ResourceId for RawFd {}

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
    fn write_atomic(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        if !self.is_ready_to_write() {
            return Err(WriteError::NotReady);
        } else {
            self.write_or_buf(buf).map_err(WriteError::from)
        }
    }

    fn is_ready_to_write(&self) -> bool;

    /// Empties any write buffers in a non-blocking way. If a non-blocking
    /// operation is not possible, errors with [`io::ErrorKind::WouldBlock`]
    /// kind of [`io::Error`].
    ///
    /// # Returns
    ///
    /// If the buffer contained any data before this operation.
    fn empty_write_buf(&mut self) -> io::Result<bool>;
    fn write_or_buf(&mut self, buf: &[u8]) -> io::Result<()>;
}
