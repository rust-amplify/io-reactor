pub mod popol;

use std::fmt::{self, Display, Formatter};
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crate::resource::Io;

/// Information about I/O events which has happened for an actor
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug)]
pub struct IoType {
    /// Specifies whether I/O source has data to read.
    pub is_readable: bool,
    /// Specifies whether I/O source is ready for write operations.
    pub is_writable: bool,
}

impl IoType {
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

impl Iterator for IoType {
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

impl Display for IoType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.is_readable {
            f.write_str("r")?;
        }
        if self.is_writable {
            f.write_str("w")?;
        }
        Ok(())
    }
}

pub trait Poll
where
    Self: Send + Iterator<Item = (RawFd, IoType)>,
    for<'a> &'a mut Self: Iterator<Item = (RawFd, IoType)>,
{
    fn register(&mut self, fd: &impl AsRawFd, interest: IoType);
    fn unregister(&mut self, fd: &impl AsRawFd);
    fn set_interest(&mut self, fd: &impl AsRawFd, interest: IoType) -> bool;

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize>;
}
