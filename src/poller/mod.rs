pub mod popol;

use crate::resource::Io;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

/// Information about I/O events which has happened for an actor
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct IoEv {
    /// Specifies whether I/O source has data to read.
    pub is_readable: bool,
    /// Specifies whether I/O source is ready for write operations.
    pub is_writable: bool,
}

impl IoEv {
    pub fn read_only() -> Self {
        Self {
            is_readable: true,
            is_writable: false,
        }
    }

    pub fn write_only() -> Self {
        Self {
            is_readable: false,
            is_writable: true,
        }
    }

    pub fn read_write() -> Self {
        Self {
            is_readable: true,
            is_writable: true,
        }
    }
}

impl Iterator for IoEv {
    type Item = Io;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_writable {
            self.is_writable = false;
            Some(Io::Write)
        } else if self.is_readable {
            self.is_readable = false;
            Some(Io::Read)
        } else {
            None
        }
    }
}

pub trait Poll
where
    Self: Send + Iterator<Item = (RawFd, IoEv)>,
    for<'a> &'a mut Self: Iterator<Item = (RawFd, IoEv)>,
{
    fn register(&mut self, fd: &impl AsRawFd);
    fn unregister(&mut self, fd: &impl AsRawFd);
    fn set_iterest(&mut self, fd: &impl AsRawFd, interest: IoEv) -> bool;

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize>;
}
