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

#![allow(unused_variables)] // because we need them for feature-gated logger

use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Display, Formatter};
use std::os::unix::io::{AsRawFd, RawFd};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{io, thread};

use crossbeam_channel as chan;
use crossbeam_channel::SendError;

use crate::poller::{IoType, Poll, Waker, WakerRecv, WakerSend};
use crate::resource::WriteError;
use crate::{Resource, ResourceId, ResourceType, Timer, Timestamp, WriteAtomic};

/// Maximum amount of time to wait for I/O.
const WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Reactor errors
#[derive(Error, Display, From)]
#[display(doc_comments)]
pub enum Error<L: Resource, T: Resource> {
    /// transport {0} got disconnected during poll operation.
    ListenerDisconnect(ResourceId, L),

    /// transport {0} got disconnected during poll operation.
    TransportDisconnect(ResourceId, T),

    /// polling multiple reactor has failed. Details: {0:?}
    Poll(io::Error),
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
}

/// A service which handles I/O events generated in the [`Reactor`].
pub trait Handler: Send + Iterator<Item = Action<Self::Listener, Self::Transport>> {
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

    /// A command which may be sent to the [`Handler`] from outside the [`Reactor`], including
    /// other threads.
    ///
    /// The handler object is owned by the reactor runtime and executes always in the context of the
    /// reactor runtime thread. Thus, if other (micro)services within the app needs to communicate
    /// to the handler they have to use this data type, which usually is an enumeration for a set of
    /// commands supported by the handler.
    ///
    /// The commands are sent by using reactor [`Controller`] API.
    type Command: Debug + Send;

    /// Method called by the reactor on the start of each event loop once the poll has returned.
    fn tick(&mut self, time: Timestamp);

    /// Method called by the reactor when a previously set timeout is fired.
    ///
    /// Related: [`Action::SetTimer`].
    fn handle_timer(&mut self);

    /// Method called by the reactor upon an I/O event on a listener resource.
    ///
    /// Since listener doesn't support writing, it can be only a read event (indicating that a new
    /// resource can be spawned from the listener).
    fn handle_listener_event(
        &mut self,
        id: ResourceId,
        event: <Self::Listener as Resource>::Event,
        time: Timestamp,
        sender: Controller<Self::Command, impl WakerSend>,
    );

    /// Method called by the reactor upon I/O event on a transport resource.
    fn handle_transport_event(
        &mut self,
        id: ResourceId,
        event: <Self::Transport as Resource>::Event,
        time: Timestamp,
        sender: Controller<Self::Command, impl WakerSend>,
    );

    /// Method called by the reactor when a given resource was successfully registered and provided
    /// with a resource id.
    ///
    /// The resource id will be used later in [`Self::handle_listener_event`],
    /// [`Self::handle_transport_event`], [`Self::handover_listener`] and [`handover_transport`]
    /// calls to the handler.
    fn handle_registered(&mut self, fd: RawFd, id: ResourceId, ty: ResourceType);

    /// Method called by the reactor when a [`Self::Command`] is received for the [`Handler`].
    ///
    /// The commands are sent via [`Controller`] from outside of the reactor, including other
    /// threads.
    fn handle_command(&mut self, cmd: Self::Command);

    /// Method called by the reactor on any kind of error during the event loop, including errors of
    /// the poll syscall or I/O errors returned as a part of the poll result events.
    ///
    /// See [`enum@Error`] for the details on errors which may happen.
    fn handle_error(&mut self, err: Error<Self::Listener, Self::Transport>);

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

/// High-level reactor API wrapping reactor [`Runtime`] into a thread and providing basic thread
/// management for it.
///
/// Apps running the [`Reactor`] can interface it and a [`Handler`] via use of the [`Controller`]
/// API.
pub struct Reactor<C, P: Poll> {
    thread: JoinHandle<()>,
    controller: Controller<C, <P::Waker as Waker>::Send>,
}

impl<C, P: Poll> Reactor<C, P> {
    /// Creates new reactor using provided [`Poll`] engine and a service exposing [`Handler`] API to
    /// the reactor.
    ///
    /// Both poll engine and the service are sent to the newly created reactor thread which runs the
    /// reactor [`Runtime`].
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn new<H: Handler<Command = C>>(service: H, poller: P) -> Result<Self, io::Error>
    where
        H: 'static,
        P: 'static,
        C: 'static + Send,
    {
        Reactor::with(service, poller, thread::Builder::new())
    }

    /// Creates new reactor using provided [`Poll`] engine and a service exposing [`Handler`] API to
    /// the reactor.
    ///
    /// Similar to the [`Reactor::new`], but allows to specify the name for the reactor thread.
    /// Both poll engine and the service are sent to the newly created reactor thread which runs the
    /// reactor [`Runtime`].
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn named<H: Handler<Command = C>>(
        service: H,
        poller: P,
        thread_name: String,
    ) -> Result<Self, io::Error>
    where
        H: 'static,
        P: 'static,
        C: 'static + Send,
    {
        Reactor::with(service, poller, thread::Builder::new().name(thread_name))
    }

    /// Creates new reactor using provided [`Poll`] engine and a service exposing [`Handler`] API to
    /// the reactor.
    ///
    /// Similar to the [`Reactor::new`], but allows to fully customize how the reactor thread is
    /// constructed. Both poll engine and the service are sent to the newly created reactor
    /// thread which runs the reactor [`Runtime`].
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn with<H: Handler<Command = C>>(
        service: H,
        mut poller: P,
        builder: thread::Builder,
    ) -> Result<Self, io::Error>
    where
        H: 'static,
        P: 'static,
        C: 'static + Send,
    {
        let (ctl_send, ctl_recv) = chan::unbounded();

        let (waker_writer, waker_reader) = P::Waker::pair()?;

        let controller = Controller {
            ctl_send,
            waker: waker_writer,
        };

        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Initializing reactor thread...");

        let runtime_controller = controller.clone();
        let thread = builder.spawn(move || {
            #[cfg(feature = "log")]
            log::debug!(target: "reactor", "Registering waker (fd {})", waker_reader.as_raw_fd());
            poller.register_waker(&waker_reader);

            let runtime = Runtime {
                service,
                poller,
                controller: runtime_controller,
                ctl_recv,
                listeners: empty!(),
                transports: empty!(),
                waker: waker_reader,
                timeouts: Timer::new(),
                actions: empty!(),
            };

            #[cfg(feature = "log")]
            log::info!(target: "reactor", "Entering reactor event loop");

            runtime.run();
        })?;

        // Waking up to consume actions which were provided by the service on launch
        controller.wake()?;
        Ok(Self { thread, controller })
    }

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside of its thread.
    ///
    /// See [`Handler::Command`] for the details.
    pub fn controller(&self) -> Controller<C, <P::Waker as Waker>::Send> { self.controller.clone() }

    /// Joins the reactor thread.
    pub fn join(self) -> thread::Result<()> { self.thread.join() }
}

pub enum Ctl<C> {
    Cmd(C),
    Send(ResourceId, Vec<u8>),
    Shutdown,
}

/// Control API to the service which is run inside a reactor.
///
/// The service is passed to the [`Reactor`] constructor as a parameter and also exposes [`Handler`]
/// API to the reactor itself for receiving reactor-generated events. This API is used by the
/// reactor to inform the service about incoming commands, sent via this [`Controller`] API (see
/// [`Handler::Command`] for the details).
pub struct Controller<C, W: WakerSend> {
    ctl_send: chan::Sender<Ctl<C>>,
    waker: W,
}

impl<C, W: WakerSend> Clone for Controller<C, W> {
    fn clone(&self) -> Self {
        Controller {
            ctl_send: self.ctl_send.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<C, W: WakerSend> Controller<C, W> {
    /// Send a command to the service inside a [`Reactor`] or a reactor [`Runtime`].
    #[allow(unused_mut)] // because of the `log` feature gate
    pub fn cmd(&self, mut command: C) -> Result<(), io::Error>
    where C: 'static {
        #[cfg(feature = "log")]
        {
            use std::any::Any;

            let cmd = Box::new(command);
            let any = cmd as Box<dyn Any>;
            let any = match any.downcast::<Box<dyn Debug>>() {
                Err(any) => {
                    log::debug!(target: "reactor-controller", "Sending command to the reactor");
                    any
                }
                Ok(debug) => {
                    log::debug!(target: "reactor-controller", "Sending command {debug:?} to the reactor");
                    debug
                }
            };
            command = *any.downcast().expect("from upcast");
        }

        self.ctl_send.send(Ctl::Cmd(command)).map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.wake()?;
        Ok(())
    }

    /// Shutdown the reactor.
    pub fn shutdown(self) -> Result<(), Self> {
        #[cfg(feature = "log")]
        log::info!(target: "reactor-controller", "Initiating reactor shutdown...");

        let res1 = self.ctl_send.send(Ctl::Shutdown);
        let res2 = self.wake();
        res1.or(res2).map_err(|_| self)
    }

    fn wake(&self) -> io::Result<()> {
        #[cfg(feature = "log")]
        log::trace!(target: "reactor-controller", "Wakening the reactor");
        self.waker.wake()
    }
}

/// Sender, holding [`Controller`], aware of a specific resource which was the source of the reactor
/// I/O event.
pub struct Sender<C, W: WakerSend> {
    controller: Controller<C, W>,
    resource_id: ResourceId,
}

impl<C, W: WakerSend> Sender<C, W> {
    /// Sends data to a source of the I/O event, generated inside the reactor and passed to this
    /// [`Sender`] instance.
    pub fn send(&self, data: impl ToOwned<Owned = Vec<u8>>) -> Result<(), SendError<Ctl<C>>>
    where C: 'static {
        self.controller.ctl_send.send(Ctl::Send(self.resource_id, data.to_owned()))?;
        Ok(())
    }
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
    ctl_recv: chan::Receiver<Ctl<H::Command>>,
    listeners: HashMap<ResourceId, H::Listener>,
    transports: HashMap<ResourceId, H::Transport>,
    waker: <P::Waker as Waker>::Recv,
    timeouts: Timer,
    actions: VecDeque<Action<H::Listener, H::Transport>>,
}

impl<H: Handler, P: Poll> Runtime<H, P> {
    /// Creates new reactor runtime using provided [`Poll`] engine and a service exposing
    /// [`Handler`] API to the reactor.
    pub fn with(service: H, mut poller: P) -> io::Result<Self> {
        let (ctl_send, ctl_recv) = chan::unbounded();

        let (waker_writer, waker_reader) = P::Waker::pair()?;

        #[cfg(feature = "log")]
        log::debug!(target: "reactor", "Registering waker (fd {})", waker_reader.as_raw_fd());
        poller.register_waker(&waker_reader);

        let controller = Controller {
            ctl_send,
            waker: waker_writer,
        };

        Ok(Runtime {
            service,
            poller,
            controller,
            ctl_recv,
            listeners: empty!(),
            transports: empty!(),
            waker: waker_reader,
            timeouts: Timer::new(),
            actions: empty!(),
        })
    }

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside its thread.
    ///
    /// See [`Handler::Command`] for the details.
    pub fn controller(&self) -> Controller<H::Command, <P::Waker as Waker>::Send> {
        self.controller.clone()
    }

    fn run(mut self) {
        loop {
            let before_poll = Timestamp::now();
            let timeout = self.timeouts.next_expiring_from(before_poll).unwrap_or(WAIT_TIMEOUT);

            for (id, res) in &self.listeners {
                self.poller.set_interest(*id, res.interests());
            }
            for (id, res) in &self.transports {
                self.poller.set_interest(*id, res.interests());
            }

            // Blocking
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Polling with timeout {timeout:?}");

            let res = self.poller.poll(Some(timeout));
            let now = Timestamp::now();
            self.service.tick(now);

            // Nb. The way this is currently used basically ignores which keys have
            // timed out. So as long as *something* timed out, we wake the service.
            let timers_fired = self.timeouts.remove_expired_by(now);
            if timers_fired > 0 {
                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Timer has fired");
                self.service.handle_timer();
            }

            match res {
                Ok(0) if timers_fired == 0 => {
                    #[cfg(feature = "log")]
                    log::trace!(target: "reactor", "Poll timeout; no I/O events had happened");
                }
                Err(err) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Error during polling: {err}");
                    self.service.handle_error(Error::Poll(err));
                }
                _ => {}
            }

            let awoken = self.handle_events(now);

            // Process the commands only if we awaken by the waker
            if awoken {
                loop {
                    match self.ctl_recv.try_recv() {
                        Err(chan::TryRecvError::Empty) => break,
                        Err(chan::TryRecvError::Disconnected) => {
                            panic!("control channel is broken")
                        }
                        Ok(Ctl::Shutdown) => return self.handle_shutdown(),
                        Ok(Ctl::Send(id, data)) => {
                            self.actions.push_back(Action::Send(id, data));
                        }
                        Ok(Ctl::Cmd(cmd)) => self.service.handle_command(cmd),
                    }
                }
            }

            self.handle_actions(now);
        }
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

                        let sender = self.controller();
                        let listener = self.listeners.get_mut(&id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = listener.handle_io(io) {
                                self.service.handle_listener_event(id, event, time, sender.clone());
                            }
                        }
                    }
                    Err(err) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Listener {id} {err}");
                        let listener =
                            self.unregister_listener(id).expect("listener has disappeared");
                        self.service.handle_error(Error::ListenerDisconnect(id, listener));
                    }
                }
            } else if self.transports.contains_key(&id) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from transport {id}");

                        let sender = self.controller();
                        let transport = self.transports.get_mut(&id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = transport.handle_io(io) {
                                let sender = sender.clone();
                                self.service.handle_transport_event(id, event, time, sender);
                            }
                        }
                    }
                    Err(err) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Transport {id} {err}");
                        let transport =
                            self.unregister_transport(id).expect("transport has disappeared");
                        self.service.handle_error(Error::TransportDisconnect(id, transport));
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

    fn handle_actions(&mut self, time: Timestamp) {
        while let Some(action) = self.actions.pop_front().or_else(|| self.service.next()) {
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Handling action {action} from the service");

            // NB: Deadlock may happen here if the service will generate events over and over
            // in the handle_* calls we may never get out of this loop
            if let Err(err) = self.handle_action(action, time) {
                #[cfg(feature = "log")]
                log::error!(target: "reactor", "Error: {err}");
                self.service.handle_error(err);
            }
        }
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
    ) -> Result<(), Error<H::Listener, H::Transport>> {
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
                    return Ok(());
                };
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over listener {id}");
                self.service.handover_listener(id, listener);
            }
            Action::UnregisterTransport(id) => {
                let Some(transport) = self.unregister_transport(id) else {
                    return Ok(());
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

                    return Ok(());
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
        }
        Ok(())
    }

    fn handle_shutdown(self) {
        #[cfg(feature = "log")]
        log::info!(target: "reactor", "Shutdown");

        // We just drop here?
    }

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

#[cfg(test)]
mod test {
    use std::io::stdout;
    use std::thread::sleep;

    use super::*;
    use crate::{poller, Io};

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
        impl Handler for DumbService {
            type Listener = DumbRes;
            type Transport = DumbRes;
            type Command = Cmd;

            fn tick(&mut self, _time: Timestamp) {}
            fn handle_timer(&mut self) {
                self.log.push(Event::Timer);
                self.set_timer = true;
            }
            fn handle_listener_event(
                &mut self,
                _d: ResourceId,
                _event: <Self::Listener as Resource>::Event,
                _time: Timestamp,
                _sender: Controller<Cmd, impl WakerSend>,
            ) {
                unreachable!()
            }
            fn handle_transport_event(
                &mut self,
                _id: ResourceId,
                _event: <Self::Transport as Resource>::Event,
                _time: Timestamp,
                _sender: Controller<Cmd, impl WakerSend>,
            ) {
                unreachable!()
            }
            fn handle_registered(&mut self, _fd: RawFd, _id: ResourceId, _ty: ResourceType) {}
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
            fn handle_error(&mut self, err: Error<Self::Listener, Self::Transport>) {
                panic!("{err}")
            }
            fn handover_listener(&mut self, _id: ResourceId, _listener: Self::Listener) {
                unreachable!()
            }
            fn handover_transport(&mut self, _id: ResourceId, _transport: Self::Transport) {
                unreachable!()
            }
        }

        let reactor = Reactor::new(DumbService::default(), poller::popol::Poller::new()).unwrap();
        reactor.controller().cmd(Cmd::Init).unwrap();
        sleep(Duration::from_secs(2));
        reactor.controller().cmd(Cmd::Expect(vec![Event::Timer; 6])).unwrap();
    }
}
