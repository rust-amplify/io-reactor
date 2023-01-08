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
    fn register(&mut self, fd: &impl AsRawFd) {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Registering {}", fd.as_raw_fd());
        self.poll.register(fd.as_raw_fd(), fd, popol::event::ALL);
    }

    fn unregister(&mut self, fd: &impl AsRawFd) {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Unregistering {}", fd.as_raw_fd());
        self.poll.unregister(&fd.as_raw_fd());
    }

    fn set_iterest(&mut self, fd: &impl AsRawFd, interest: IoEv) -> bool {
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Setting interest `{}` on {}", interest, fd.as_raw_fd());
        self.poll.set(&fd.as_raw_fd(), interest.into())
    }

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        let len = self.events.len();

        #[cfg(feature = "log")]
        log::trace!(target: "popol",
            "Polling {} resources with timeout {:?} (pending event queue is {})",
            self.poll.len(), timeout, len
        );

        // Blocking call
        if self.poll.wait_timeout(timeout.into())? {
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Poll timed out with zero events generated");
            return Ok(0);
        }

        let event_count = self.events.len() - len;
        #[cfg(feature = "log")]
        log::trace!(target: "popol", "Poll resulted in {} events; adding them to the queue", event_count);

        for (fd, ev) in self.poll.events() {
            let ev = IoEv {
                is_readable: ev.is_readable(),
                is_writable: ev.is_writable(),
            };
            #[cfg(feature = "log")]
            log::trace!(target: "popol", "Got event `{}` for {}", ev, fd);
            self.events.push_back((*fd, ev))
        }

        Ok(self.events.len() - len)
    }
}

impl Iterator for Poller {
    type Item = (RawFd, IoEv);

    fn next(&mut self) -> Option<Self::Item> {
        match self.events.pop_front() {
            Some((fd, ev)) => {
                #[cfg(feature = "log")]
                log::trace!(target: "popol", "Popped event `{}` for {} from the queue", ev, fd);
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

impl From<IoEv> for popol::Event {
    fn from(ev: IoEv) -> Self {
        let mut e = popol::event::NONE;
        if ev.is_readable {
            e |= popol::event::READ;
        }
        if ev.is_writable {
            e |= popol::event::WRITE;
        }
        e
    }
}
