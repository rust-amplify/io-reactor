// Library for concurrent I/O resource management using reactor pattern.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2021-2025 by
//     Dr. Maxim Orlovsky <orlovsky@ubideco.org>
//     Alexis Sellier <alexis@cloudhead.io>
//
// Copyright 2022-2025 UBIDECO Labs, InDCS, Lugano, Switzerland. All Rights reserved.
// Copyright 2021-2023 Alexis Sellier <alexis@cloudhead.io>. All Rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except
// in compliance with the License. You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software distributed under the License
// is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express
// or implied. See the License for the specific language governing permissions and limitations under
// the License.

//! Networking reactor runtime.

#![allow(unused_variables)] // because we need them for feature-gated logger

use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};

use crate::poller::{IoType, Poll, Waker, WakerRecv, WakerSend};
use crate::resource::WriteError;
use crate::runtimes::controller::Ctl;
use crate::runtimes::{Controller, Error as RuntimeError, ReactorHandler, ReactorRuntime};
use crate::{Resource, ResourceId, Timer, Timestamp, WriteAtomic};

/// Type of the resource.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ResourceType {
    /// Listener resource.
    Listener,
    /// Transport resource.
    Transport,
}

/// Reactor errors
#[derive(Error, Display, From)]
#[display(doc_comments)]
pub enum Error<L: Resource, T: Resource> {
    /// transport {0} got disconnected during poll operation.
    ListenerDisconnect(ResourceId, L),

    /// transport {0} got disconnected during poll operation.
    TransportDisconnect(ResourceId, T),
}

impl<L: Resource, T: Resource> Debug for Error<L, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result { Display::fmt(self, f) }
}

/// Actions which can be provided to the reactor by the [`Handler`].
///
/// Reactor reads actions on each event loop using [`Handler`] iterator interface.
#[derive(Display)]
pub enum Action<L: Resource, T: Resource> {
    /// Register a new listener resource for the reactor poll.
    ///
    /// Reactor can't instantiate the resource, like bind a network listener.
    /// Reactor only can register already active resource for polling in the event loop.
    #[display("register_listener")]
    RegisterListener(L),

    /// Register a new transport resource for the reactor poll.
    ///
    /// Reactor can't instantiate the resource, like open a file or establish network connection.
    /// Reactor only can register already active resource for polling in the event loop.
    #[display("register_transport")]
    RegisterTransport(T),

    /// Unregister listener resource from the reactor poll and handover it to the [`Handler`] via
    /// [`Handler::handover_listener`].
    ///
    /// When the resource is unregistered no action is performed, i.e. the file descriptor is not
    /// closed, listener is not unbound, connections are not closed etc. All these actions must be
    /// handled by the handler upon the handover event.
    #[display("unregister_listener")]
    UnregisterListener(ResourceId),

    /// Unregister transport resource from the reactor poll and handover it to the [`Handler`] via
    /// [`Handler::handover_transport`].
    ///
    /// When the resource is unregistered no action is performed, i.e. the file descriptor is not
    /// closed, listener is not unbound, connections are not closed etc. All these actions must be
    /// handled by the handler upon the handover event.
    #[display("unregister_transport")]
    UnregisterTransport(ResourceId),

    /// Write the data to one of the transport resources using [`io::Write`].
    #[display("send_to({0})")]
    Send(ResourceId, Vec<u8>),

    /// Set a new timer for a given duration from this moment.
    ///
    /// When the timer fires reactor will timeout poll syscall and call [`Handler::handle_timer`].
    #[display("set_timer({0:?})")]
    SetTimer(Duration),

    /// Graceffully terminate the reactor.
    #[display("terminate")]
    Terminate,
}

/// A service which handles I/O events generated in the [`Reactor`].
pub trait Handler:
    ReactorHandler<
    Action = Action<Self::Listener, Self::Transport>,
    Error = Error<Self::Listener, Self::Transport>,
>
{
    /// Type for a listener resource.
    ///
    /// Listener resources are resources which may spawn more resources and can't be written to. A
    /// typical example of a listener resource is a [`std::net::TcpListener`], however this may also
    /// be a special form of a peripheral device or something else.
    type Listener: Resource;

    /// Type for a transport resource.
    ///
    /// Transport is a "full" resource which can be read from - and written to. Usual files, network
    /// connections, database connections etc are all fall into this category.
    type Transport: Resource;

    /// Method called by the reactor upon an I/O event on a listener resource.
    ///
    /// Since listener doesn't support writing, it can be only a read event (indicating that a new
    /// resource can be spawned from the listener).
    fn handle_listener_event(
        &mut self,
        fd: RawFd,
        id: ResourceId,
        event: <Self::Listener as Resource>::Event,
        time: Timestamp,
    );

    /// Method called by the reactor upon I/O event on a transport resource.
    fn handle_transport_event(
        &mut self,
        fd: RawFd,
        id: ResourceId,
        event: <Self::Transport as Resource>::Event,
        time: Timestamp,
    );

    /// Method called by the reactor when a given resource was successfully registered and provided
    /// with a resource id.
    ///
    /// The resource id will be used later in [`Self::handle_listener_event`],
    /// [`Self::handle_transport_event`], [`Self::handover_listener`] and [`handover_transport`]
    /// calls to the handler.
    fn handle_registered(&mut self, fd: RawFd, id: ResourceId, ty: ResourceType);

    /// Method called by the reactor upon receiving [`Action::UnregisterListener`].
    ///
    /// Passes the listener resource to the [`Handler`] when it is already not a part of the reactor
    /// poll. From this point of time it is safe to send the resource to other threads (like
    /// workers) or close the resource.
    fn handover_listener(&mut self, id: ResourceId, listener: Self::Listener);

    /// Method called by the reactor upon receiving [`Action::UnregisterTransport`].
    ///
    /// Passes the transport resource to the [`Handler`] when it is already not a part of the
    /// reactor poll. From this point of time it is safe to send the resource to other threads
    /// (like workers) or close the resource.
    fn handover_transport(&mut self, id: ResourceId, transport: Self::Transport);
}

/// Internal [`Reactor`] runtime which is run in a dedicated thread.
///
/// Use this structure direactly only if you'd like to have the full control over the reactor
/// thread.
///
/// This runtime structure **does not** spawns a thread and is **blocking**. It implements the
/// actual reactor event loop.
pub struct Runtime<H: Handler, P: Poll> {
    service: H,
    poller: P,
    controller: Controller<H::Command, <P::Waker as Waker>::Send>,
    ctl_recv: Receiver<Ctl<H::Command>>,
    listeners: HashMap<ResourceId, H::Listener>,
    transports: HashMap<ResourceId, H::Transport>,
    waker: <P::Waker as Waker>::Recv,
    timeouts: Timer,
}

impl<H: Handler, P: Poll> Runtime<H, P> {
    fn unregister_listener(&mut self, id: ResourceId) -> Option<H::Listener> {
        let Some(listener) = self.listeners.remove(&id) else {
            #[cfg(feature = "log")]
            log::warn!(target: "reactor", "Unregistering non-registered listener {id}");
            return None;
        };

        #[cfg(feature = "log")]
        log::debug!(target: "reactor", "Handling over listener {id} (fd={})", listener.as_raw_fd());

        self.poller.unregister(id);

        Some(listener)
    }

    fn unregister_transport(&mut self, id: ResourceId) -> Option<H::Transport> {
        let Some(transport) = self.transports.remove(&id) else {
            #[cfg(feature = "log")]
            log::warn!(target: "reactor", "Unregistering non-registered transport {id}");
            return None;
        };

        #[cfg(feature = "log")]
        log::debug!(target: "reactor", "Unregistering over transport {id} (fd={})", transport.as_raw_fd());

        self.poller.unregister(id);

        Some(transport)
    }
}

impl<H: Handler + 'static, P: Poll> ReactorRuntime<P> for Runtime<H, P> {
    const WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

    type Handler = H;

    fn with(
        service: Self::Handler,
        poller: P,
        controller: Controller<
            <Self::Handler as ReactorHandler>::Command,
            <P::Waker as Waker>::Send,
        >,
        ctl_recv: Receiver<Ctl<<Self::Handler as ReactorHandler>::Command>>,
        waker_recv: <P::Waker as Waker>::Recv,
    ) -> Self {
        Runtime {
            service,
            poller,
            controller,
            ctl_recv,
            listeners: empty!(),
            transports: empty!(),
            waker: waker_recv,
            timeouts: Timer::new(),
        }
    }

    fn timeouts(&mut self) -> &mut Timer { &mut self.timeouts }
    fn poller(&mut self) -> &mut P { &mut self.poller }
    fn service(&mut self) -> &mut Self::Handler { &mut self.service }
    fn resource_interests(&self) -> impl Iterator<Item = (ResourceId, IoType)> {
        self.listeners
            .iter()
            .map(move |(id, handler)| (*id, handler.interests()))
            .chain(self.transports.iter().map(move |(id, transport)| (*id, transport.interests())))
    }

    fn ctl_recv(&self) -> Result<Ctl<<Self::Handler as ReactorHandler>::Command>, TryRecvError> {
        self.ctl_recv.try_recv()
    }

    fn controller(&self) -> Controller<<Self::Handler as ReactorHandler>::Command, impl WakerSend> {
        self.controller.clone()
    }

    /// # Returns
    ///
    /// Whether it was awakened by a waker
    fn handle_events(&mut self, time: Timestamp) -> bool {
        let mut awoken = false;

        while let Some((id, res)) = self.poller.next() {
            if id == ResourceId::WAKER {
                if let Err(err) = res {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Polling waker has failed: {err}");
                    panic!("waker failure: {err}");
                };

                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Awoken by the controller");

                self.waker.reset();
                awoken = true;
            } else if self.listeners.contains_key(&id) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from listener {id}");

                        let listener = self.listeners.get_mut(&id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = listener.handle_io(io) {
                                let fd = listener.as_raw_fd();
                                self.service.handle_listener_event(fd, id, event, time);
                            }
                        }
                    }
                    Err(err) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Listener {id} {err}");
                        let listener =
                            self.unregister_listener(id).expect("listener has disappeared");
                        self.service.handle_error(RuntimeError::Handler(
                            Error::ListenerDisconnect(id, listener),
                        ));
                    }
                }
            } else if self.transports.contains_key(&id) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from transport {id}");

                        let transport = self.transports.get_mut(&id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = transport.handle_io(io) {
                                let fd = transport.as_raw_fd();
                                self.service.handle_transport_event(fd, id, event, time);
                            }
                        }
                    }
                    Err(err) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Transport {id} {err}");
                        let transport =
                            self.unregister_transport(id).expect("transport has disappeared");
                        self.service.handle_error(RuntimeError::Handler(
                            Error::TransportDisconnect(id, transport),
                        ));
                    }
                }
            } else {
                panic!(
                    "file descriptor in reactor which is not a known waker, listener or transport"
                )
            }
        }

        awoken
    }

    /// # Safety
    ///
    /// Panics on `Action::Send` for read-only resources or resources which are not ready for a
    /// write operation (i.e. returning `false` from [`WriteAtomic::is_ready_to_write`]
    /// implementation.
    fn handle_action(
        &mut self,
        action: Action<H::Listener, H::Transport>,
        time: Timestamp,
    ) -> Result<bool, Error<H::Listener, H::Transport>> {
        match action {
            Action::RegisterListener(listener) => {
                let fd = listener.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Registering listener with fd={fd}");

                let id = self.poller.register(&listener, IoType::read_only());
                self.listeners.insert(id, listener);
                self.service.handle_registered(fd, id, ResourceType::Listener);
            }
            Action::RegisterTransport(transport) => {
                let fd = transport.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Registering transport with fd={fd}");

                let id = self.poller.register(&transport, IoType::read_only());
                self.transports.insert(id, transport);
                self.service.handle_registered(fd, id, ResourceType::Transport);
            }
            Action::UnregisterListener(id) => {
                let Some(listener) = self.unregister_listener(id) else {
                    return Ok(true);
                };
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over listener {id}");
                self.service.handover_listener(id, listener);
            }
            Action::UnregisterTransport(id) => {
                let Some(transport) = self.unregister_transport(id) else {
                    return Ok(true);
                };
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over transport {id}");
                self.service.handover_transport(id, transport);
            }
            Action::Send(id, data) => {
                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Sending {} bytes to {id}", data.len());

                let Some(transport) = self.transports.get_mut(&id) else {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Transport {id} is not in the reactor");

                    return Ok(true);
                };
                match transport.write_atomic(&data) {
                    Err(WriteError::NotReady) => {
                        #[cfg(feature = "log")]
                        log::error!(target: "reactor", internal = true; 
                                "An attempt to write to transport {id} before it got ready");
                        panic!(
                            "application business logic error: write to transport {id} which is \
                             read-only or not ready for a write operation"
                        );
                    }
                    Err(WriteError::Io(e)) => {
                        #[cfg(feature = "log")]
                        log::error!(target: "reactor", "Fatal error writing to transport {id}, disconnecting. Error details: {e:?}");
                        if let Some(transport) = self.unregister_transport(id) {
                            return Err(Error::TransportDisconnect(id, transport));
                        }
                    }
                    Ok(_) => {}
                }
            }
            Action::SetTimer(duration) => {
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Adding timer {duration:?} from now");

                self.timeouts.set_timeout(duration, time);
            }
            Action::Terminate => return Ok(false),
        }
        Ok(true)
    }

    fn handle_shutdown(self) {
        #[cfg(feature = "log")]
        log::info!(target: "reactor", "Shutdown");

        // We just drop here?
    }
}

#[cfg(test)]
mod test {
    use std::io::{self, stdout};
    use std::thread::sleep;

    use super::*;
    use crate::{poller, Io, Reactor};

    pub struct DumbRes(Box<dyn AsRawFd + Send>);
    impl DumbRes {
        pub fn new() -> DumbRes { DumbRes(Box::new(stdout())) }
    }
    impl AsRawFd for DumbRes {
        fn as_raw_fd(&self) -> RawFd { self.0.as_raw_fd() }
    }
    impl io::Write for DumbRes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> { Ok(buf.len()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    impl WriteAtomic for DumbRes {
        fn is_ready_to_write(&self) -> bool { true }
        fn empty_write_buf(&mut self) -> io::Result<bool> { Ok(true) }
        fn write_or_buf(&mut self, _buf: &[u8]) -> io::Result<()> { Ok(()) }
    }
    impl Resource for DumbRes {
        type Event = ();
        fn interests(&self) -> IoType { IoType::read_write() }
        fn handle_io(&mut self, _io: Io) -> Option<Self::Event> { None }
    }

    #[test]
    fn timer() {
        #[derive(Clone, Eq, PartialEq, Debug)]
        enum Cmd {
            Init,
            Expect(Vec<Event>),
        }
        #[derive(Clone, Eq, PartialEq, Debug)]
        enum Event {
            Timer,
        }
        #[derive(Clone, Debug, Default)]
        struct DumbService {
            pub add_resource: bool,
            pub set_timer: bool,
            pub log: Vec<Event>,
        }
        impl Iterator for DumbService {
            type Item = Action<DumbRes, DumbRes>;
            fn next(&mut self) -> Option<Self::Item> {
                if self.add_resource {
                    self.add_resource = false;
                    Some(Action::RegisterTransport(DumbRes::new()))
                } else if self.set_timer {
                    self.set_timer = false;
                    Some(Action::SetTimer(Duration::from_millis(3)))
                } else {
                    None
                }
            }
        }
        impl ReactorHandler for DumbService {
            type Command = Cmd;
            type Action = Action<DumbRes, DumbRes>;
            type Error = Error<DumbRes, DumbRes>;

            fn tick(&mut self, _time: Timestamp) {}
            fn handle_timer(&mut self) {
                self.log.push(Event::Timer);
                self.set_timer = true;
            }
            fn handle_command(&mut self, cmd: Self::Command) {
                match cmd {
                    Cmd::Init => {
                        self.add_resource = true;
                        self.set_timer = true;
                    }
                    Cmd::Expect(expected) => {
                        assert_eq!(expected, self.log);
                    }
                }
            }
            fn handle_error(&mut self, err: RuntimeError<Self::Error>) { panic!("{err}") }
        }
        impl Handler for DumbService {
            type Listener = DumbRes;
            type Transport = DumbRes;

            fn handle_listener_event(
                &mut self,
                _fd: RawFd,
                _d: ResourceId,
                _event: <Self::Listener as Resource>::Event,
                _time: Timestamp,
            ) {
                unreachable!()
            }
            fn handle_transport_event(
                &mut self,
                _fd: RawFd,
                _id: ResourceId,
                _event: <Self::Transport as Resource>::Event,
                _time: Timestamp,
            ) {
                unreachable!()
            }
            fn handle_registered(&mut self, _fd: RawFd, _id: ResourceId, _ty: ResourceType) {}
            fn handover_listener(&mut self, _id: ResourceId, _listener: Self::Listener) {
                unreachable!()
            }
            fn handover_transport(&mut self, _id: ResourceId, _transport: Self::Transport) {
                unreachable!()
            }
        }

        let reactor =
            Reactor::<Runtime<_, _>, _>::new(DumbService::default(), poller::popol::Poller::new())
                .unwrap();
        reactor.controller().cmd(Cmd::Init).unwrap();
        sleep(Duration::from_secs(2));
        reactor.controller().cmd(Cmd::Expect(vec![Event::Timer; 6])).unwrap();
    }
}
