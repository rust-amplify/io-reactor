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

#[cfg(feature = "popol")]
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

    pub fn is_none(self) -> bool { !self.read && !self.write }
    pub fn is_read_only(self) -> bool { self.read && !self.write }
    pub fn is_write_only(self) -> bool { !self.read && self.write }
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
