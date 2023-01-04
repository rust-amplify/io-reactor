use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::os::unix::io::AsRawFd;
use std::os::unix::prelude::RawFd;
use std::{io, net};

use crate::poller::IoEv;

pub trait ResourceId: Copy + Eq + Ord + Hash + Debug + Display {}

pub trait Resource: AsRawFd + Send + Iterator<Item = Self::Event> {
    type Id: ResourceId + Send;
    type Event;
    type Message;

    fn id(&self) -> Self::Id;

    /// Asks resource to handle I/O. Must return a number of events generated
    /// from the resource I/O.
    fn handle_io(&mut self, ev: IoEv) -> usize;

    fn send(&mut self, msg: Self::Message) -> io::Result<()>;

    fn disconnect(self) -> io::Result<()>;
}

impl ResourceId for net::SocketAddr {}
impl ResourceId for RawFd {}
