use std::collections::VecDeque;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crate::poller::{IoEv, Poll};

/// Manager for a set of resources which are polled for an event loop by the
/// re-actor by using [`popol`] library.
pub struct Poller {
    poll: popol::Poll<RawFd>,
    events: VecDeque<(RawFd, IoEv)>,
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
    fn register(&mut self, fd: impl AsRawFd) {
        self.poll.register(fd.as_raw_fd(), &fd, popol::event::ALL);
    }

    fn unregister(&mut self, fd: impl AsRawFd) {
        self.poll.unregister(&fd.as_raw_fd());
    }

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        let len = self.events.len();

        // Blocking call
        if self.poll.wait_timeout(timeout.into())? {
            return Ok(0);
        }

        for (fd, ev) in self.poll.events() {
            self.events.push_back((
                *fd,
                IoEv {
                    is_readable: ev.is_readable(),
                    is_writable: ev.is_writable(),
                },
            ))
        }

        Ok(self.events.len() - len)
    }
}

impl Iterator for Poller {
    type Item = (RawFd, IoEv);

    fn next(&mut self) -> Option<Self::Item> {
        self.events.pop_front()
    }
}
