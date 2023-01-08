use std::collections::VecDeque;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crate::poller::{IoType, Poll};

/// Manager for a set of resources which are polled for an event loop by the
/// re-actor by using [`popol`] library.
pub struct Poller {
    poll: popol::Poll<RawFd>,
    events: VecDeque<(RawFd, IoType)>,
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
        let len = self.events.len();

        #[cfg(feature = "log")]
        log::trace!(target: "popol",
            "Polling {} resources with timeout {timeout:?} (pending event queue is {len})",
            self.poll.len(),
        );

        // Blocking call
        if self.poll.wait_timeout(timeout.into())? {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Poll timed out with zero events generated");
            return Ok(0);
        }

        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Poll resulted in {} event(s)", self.poll.len());

        for (fd, ev) in self.poll.events() {
            let ev = IoType {
                read: ev.is_readable(),
                write: ev.is_writable(),
            };
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Got event `{ev}` for {fd}");
            self.events.push_back((*fd, ev))
        }

        Ok(self.events.len() - len)
    }
}

impl Iterator for Poller {
    type Item = (RawFd, IoType);

    fn next(&mut self) -> Option<Self::Item> {
        match self.events.pop_front() {
            Some((fd, ev)) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popped event `{ev}` for {fd} from the queue");
                Some((fd, ev))
            }
            None => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popol queue emptied");
                None
            }
        }
    }
}

impl From<IoType> for popol::Event {
    fn from(ev: IoType) -> Self {
        let mut e = popol::event::NONE;
        if ev.read {
            e |= popol::event::READ;
        }
        if ev.write {
            e |= popol::event::WRITE;
        }
        e
    }
}
