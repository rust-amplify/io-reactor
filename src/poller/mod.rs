pub mod popol;

use std::fmt::{self, Display, Formatter};
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;
use std::{io, ops};

use crate::resource::Io;

/// Information about I/O events which has happened for an actor
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct IoType {
    /// Specifies whether I/O source has data to read.
    pub read: bool,
    /// Specifies whether I/O source is ready for write operations.
    pub write: bool,
}

impl IoType {
    pub fn none() -> Self {
        Self {
            read: false,
            write: false,
        }
    }

    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
        }
    }

    pub fn write_only() -> Self {
        Self {
            read: false,
            write: true,
        }
    }

    pub fn read_write() -> Self {
        Self {
            read: true,
            write: true,
        }
    }

    pub fn is_none(self) -> bool {
        self.read == false && self.write == false
    }
    pub fn is_read_only(self) -> bool {
        self.read == true && self.write == false
    }
    pub fn is_write_only(self) -> bool {
        self.read == false && self.write == true
    }
    pub fn is_read_write(self) -> bool {
        self.read == true && self.write == true
    }
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

#[derive(Copy, Clone, Debug, Display, Error)]
#[display(doc_comments)]
pub enum IoFail {
    /// connection is absent (POSIX events {0:#b})
    Connectivity(i16),
    /// OS-level error (POSIX events {0:#b})
    Os(i16),
}

pub trait Poll
where
    Self: Send + Iterator<Item = (RawFd, Result<IoType, IoFail>)>,
    for<'a> &'a mut Self: Iterator<Item = (RawFd, Result<IoType, IoFail>)>,
{
    fn register(&mut self, fd: &impl AsRawFd, interest: IoType);
    fn unregister(&mut self, fd: &impl AsRawFd);
    fn set_interest(&mut self, fd: &impl AsRawFd, interest: IoType) -> bool;

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize>;
}
