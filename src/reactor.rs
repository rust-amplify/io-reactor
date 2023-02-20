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

use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{io, thread};

use crossbeam_channel as chan;

use crate::poller::{IoFail, IoType, Poll};
use crate::resource::WriteError;
use crate::{Resource, Timer, Timestamp, WriteAtomic};

/// Maximum amount of time to wait for I/O.
const WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Reactor errors
#[derive(Error, Display, From)]
#[display(doc_comments)]
pub enum Error<L: Resource, T: Resource> {
    /// unknown listener {0}
    ListenerUnknown(L::Id),

    /// unknown transport {0}
    TransportUnknown(T::Id),

    /// unable to write to transport {0}. Details: {1:?}
    WriteFailure(T::Id, io::Error),

    /// writing to transport {0} before it is ready (business logic bug)
    WriteLogicError(T::Id, Vec<u8>),

    /// transport {0} got disconnected during poll operation.
    ListenerDisconnect(L::Id, L, i16),

    /// transport {0} got disconnected during poll operation.
    TransportDisconnect(T::Id, T, i16),

    /// poll on listener {0} has returned error.
    ListenerPollError(L::Id, i16),

    /// poll on transport {0} has returned error.
    TransportPollError(T::Id, i16),

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
    UnregisterListener(L::Id),

    /// Unregister transport resource from the reactor poll and handover it to the [`Handler`] via
    /// [`Handler::handover_transport`].
    ///
    /// When the resource is unregistered no action is performed, i.e. the file descriptor is not
    /// closed, listener is not unbound, connections are not closed etc. All these actions must be
    /// handled by the handler upon the handover event.
    #[display("unregister_transport")]
    UnregisterTransport(T::Id),

    /// Write the data to one of the transport resources using [`io::Write`].
    #[display("send_to({0})")]
    Send(T::Id, Vec<u8>),

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

    /// A command which may be sent to the [`Handler`] from outside of the [`Reactor`], including
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
        id: <Self::Listener as Resource>::Id,
        event: <Self::Listener as Resource>::Event,
        time: Timestamp,
    );

    /// Method called by the reactor upon I/O event on a transport resource.
    fn handle_transport_event(
        &mut self,
        id: <Self::Transport as Resource>::Id,
        event: <Self::Transport as Resource>::Event,
        time: Timestamp,
    );

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
    fn handover_listener(&mut self, listener: Self::Listener);

    /// Method called by the reactor upon receiving [`Action::UnregisterTransport`].
    ///
    /// Passes the transport resource to the [`Handler`] when it is already not a part of the
    /// reactor poll. From this point of time it is safe to send the resource to other threads
    /// (like workers) or close the resource.
    fn handover_transport(&mut self, transport: Self::Transport);
}

/// High-level reactor API wrapping reactor [`Runtime`] into a thread and providing basic thread
/// management for it.
///
/// Apps running the [`Reactor`] can interface it and a [`Handler`] via use of the [`Controller`]
/// API.
pub struct Reactor<C> {
    thread: JoinHandle<()>,
    controller: Controller<C>,
}

impl<C> Reactor<C> {
    /// Creates new reactor using provided [`Poll`] engine and a service exposing [`Handler`] API to
    /// the reactor.
    ///
    /// Both poll engine and the service are sent to the newly created reactor thread which runs the
    /// reactor [`Runtime`].
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn new<P: Poll, H: Handler<Command = C>>(service: H, poller: P) -> Result<Self, io::Error>
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
    pub fn named<P: Poll, H: Handler<Command = C>>(
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
    pub fn with<P: Poll, H: Handler<Command = C>>(
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

        let (waker_writer, waker_reader) = UnixStream::pair()?;
        waker_reader.set_nonblocking(true)?;
        waker_writer.set_nonblocking(true)?;

        let controller = Controller {
            ctl_send,
            waker: Arc::new(Mutex::new(waker_writer)),
        };

        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Initializing reactor thread...");

        let runtime_controller = controller.clone();
        let thread = builder.spawn(move || {
            #[cfg(feature = "log")]
            log::debug!(target: "reactor", "Registering waker (fd {})", waker_reader.as_raw_fd());
            poller.register(&waker_reader, IoType::read_only());

            let runtime = Runtime {
                service,
                poller,
                controller: runtime_controller,
                ctl_recv,
                listeners: empty!(),
                transports: empty!(),
                listener_map: empty!(),
                transport_map: empty!(),
                waker: waker_reader,
                timeouts: Timer::new(),
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
    pub fn controller(&self) -> Controller<C> { self.controller.clone() }

    /// Joins the reactor thread.
    pub fn join(self) -> thread::Result<()> { self.thread.join() }
}

enum Ctl<C> {
    Cmd(C),
    Shutdown,
}

/// Control API to the service which is run inside a reactor.
///
/// The service is passed to the [`Reactor`] constructor as a parameter and also exposes [`Handler`]
/// API to the reactor itself for receiving reactor-generated events. This API is used by the
/// reactor to inform the service about incoming commands, sent via this [`Controller`] API (see
/// [`Handler::Command`] for the details).
pub struct Controller<C> {
    ctl_send: chan::Sender<Ctl<C>>,
    waker: Arc<Mutex<UnixStream>>,
}

impl<C> Clone for Controller<C> {
    fn clone(&self) -> Self {
        Controller {
            ctl_send: self.ctl_send.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<C> Controller<C> {
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
        use io::ErrorKind::*;

        #[cfg(feature = "log")]
        log::trace!(target: "reactor-controller", "Wakening the reactor");

        #[allow(unused_variables)]
        let mut waker = self.waker.lock().map_err(|err| {
            #[cfg(feature = "log")]
            log::error!(target: "reactor-controller", "Waker lock is poisoned: {err}");
            WouldBlock
        })?;
        match waker.write_all(&[0x1]) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == WouldBlock => {
                #[cfg(feature = "log")]
                log::error!(target: "reactor-controller", "Waker write queue got overfilled, resetting and repeating...");

                reset_fd(&waker.as_raw_fd())?;
                self.wake()
            }
            Err(e) if e.kind() == Interrupted => {
                #[cfg(feature = "log")]
                log::error!(target: "reactor-controller", "Waker failure, repeating...");

                self.wake()
            }
            Err(e) => {
                #[cfg(feature = "log")]
                log::error!(target: "reactor-controller", "Waker error: {e}");

                Err(e)
            }
        }
    }
}

fn reset_fd(fd: &impl AsRawFd) -> io::Result<()> {
    let mut buf = [0u8; 4096];

    loop {
        // We use a low-level "read" here because the alternative is to create a `UnixStream`
        // from the `RawFd`, which has "drop" semantics which we want to avoid.
        match unsafe {
            libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        } {
            -1 => match io::Error::last_os_error() {
                e if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                e => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor-controller", "Unable to reset waker queue: {e}");

                    return Err(e);
                }
            },
            0 => return Ok(()),
            _ => continue,
        }
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
    controller: Controller<H::Command>,
    ctl_recv: chan::Receiver<Ctl<H::Command>>,
    listener_map: HashMap<RawFd, <H::Listener as Resource>::Id>,
    transport_map: HashMap<RawFd, <H::Transport as Resource>::Id>,
    listeners: HashMap<<H::Listener as Resource>::Id, H::Listener>,
    transports: HashMap<<H::Transport as Resource>::Id, H::Transport>,
    waker: UnixStream,
    timeouts: Timer,
}

impl<H: Handler, P: Poll> Runtime<H, P> {
    /// Creates new reactor runtime using provided [`Poll`] engine and a service exposing
    /// [`Handler`] API to the reactor.
    pub fn with(service: H, poller: P) -> io::Result<Self> {
        let (ctl_send, ctl_recv) = chan::unbounded();

        let (waker_writer, waker_reader) = UnixStream::pair()?;
        waker_reader.set_nonblocking(true)?;
        waker_writer.set_nonblocking(true)?;

        let controller = Controller {
            ctl_send,
            waker: Arc::new(Mutex::new(waker_writer)),
        };

        Ok(Runtime {
            service,
            poller,
            controller,
            ctl_recv,
            listeners: empty!(),
            transports: empty!(),
            listener_map: empty!(),
            transport_map: empty!(),
            waker: waker_reader,
            timeouts: Timer::new(),
        })
    }

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside of its thread.
    ///
    /// See [`Handler::Command`] for the details.
    pub fn controller(&self) -> Controller<H::Command> { self.controller.clone() }

    fn run(mut self) {
        loop {
            let before_poll = Timestamp::now();
            let timeout = self.timeouts.next(before_poll).unwrap_or(WAIT_TIMEOUT);

            for res in self.listeners.values() {
                self.poller.set_interest(res, res.interests());
            }
            for res in self.transports.values() {
                self.poller.set_interest(res, res.interests());
            }

            // Blocking
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Polling with timeout {timeout:?}");

            let res = self.poller.poll(Some(timeout));
            let now = Timestamp::now();
            self.service.tick(now);

            // Nb. The way this is currently used basically ignores which keys have
            // timed out. So as long as *something* timed out, we wake the service.
            let timers_fired = self.timeouts.expire(now);
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
                        Ok(Ctl::Cmd(cmd)) => self.service.handle_command(cmd),
                    }
                }
            }

            self.handle_actions(now);
        }
    }

    /// # Returns
    ///
    /// Whether it was awaken by a waker
    fn handle_events(&mut self, time: Timestamp) -> bool {
        let mut awoken = false;

        let mut unregister_queue = vec![];
        for (fd, res) in &mut self.poller {
            if fd == self.waker.as_raw_fd() {
                if let Err(err) = res {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Polling waker has failed: {err}");
                    panic!("waker failure: {err}");
                };

                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Awoken by the controller");

                reset_fd(&self.waker).expect("waker failure");
                awoken = true;
            } else if let Some(id) = self.listener_map.get(&fd) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from listener {id} (fd={fd})");

                        let listener = self.listeners.get_mut(id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = listener.handle_io(io) {
                                self.service.handle_listener_event(*id, event, time);
                            }
                        }
                    }
                    Err(IoFail::Connectivity(flags)) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Listener {id} hung up (OS flags {flags:#b})");

                        let listener = self.listeners.remove(id).expect("resource disappeared");
                        unregister_queue.push(listener.as_raw_fd());
                        self.service.handle_error(Error::ListenerDisconnect(*id, listener, flags));
                    }
                    Err(IoFail::Os(flags)) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Listener {id} errored (OS flags {flags:#b})");

                        self.service.handle_error(Error::ListenerPollError(*id, flags));
                    }
                }
            } else if let Some(id) = self.transport_map.get(&fd) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from transport {id} (fd={fd})");

                        let transport = self.transports.get_mut(id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = transport.handle_io(io) {
                                self.service.handle_transport_event(*id, event, time);
                            }
                        }
                    }
                    Err(IoFail::Connectivity(flags)) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Transport {id} hanged up (OS flags {flags:#b})");

                        let transport = self.transports.remove(id).expect("resource disappeared");
                        unregister_queue.push(transport.as_raw_fd());
                        self.service
                            .handle_error(Error::TransportDisconnect(*id, transport, flags));
                    }
                    Err(IoFail::Os(flags)) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Transport {id} errored (OS flags {flags:#b})");

                        self.service.handle_error(Error::TransportPollError(*id, flags));
                    }
                }
            } else {
                panic!(
                    "file descriptor in reactor which is not a known waker, listener or transport"
                )
            }
        }

        // We need this b/c of borrow checker
        for fd in unregister_queue {
            self.poller.unregister(&fd);
        }

        awoken
    }

    fn handle_actions(&mut self, time: Timestamp) {
        while let Some(action) = self.service.next() {
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

    fn handle_action(
        &mut self,
        action: Action<H::Listener, H::Transport>,
        time: Timestamp,
    ) -> Result<(), Error<H::Listener, H::Transport>> {
        match action {
            Action::RegisterListener(listener) => {
                let id = listener.id();
                let fd = listener.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Registering listener on {id} (fd={fd})");

                self.poller.register(&listener, IoType::read_only());
                self.listeners.insert(id, listener);
                self.listener_map.insert(fd, id);
            }
            Action::RegisterTransport(transport) => {
                let id = transport.id();
                let fd = transport.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Registering transport on {id} (fd={fd})");

                self.poller.register(&transport, IoType::read_only());
                self.transports.insert(id, transport);
                self.transport_map.insert(fd, id);
            }
            Action::UnregisterListener(id) => {
                let listener = self.listeners.remove(&id).ok_or(Error::ListenerUnknown(id))?;
                let fd = listener.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over listener {id} (fd={fd})");

                self.listener_map
                    .remove(&fd)
                    .expect("listener index content doesn't match registered listeners");
                self.poller.unregister(&listener);
                self.service.handover_listener(listener);
            }
            Action::UnregisterTransport(id) => {
                let transport = self.transports.remove(&id).ok_or(Error::TransportUnknown(id))?;
                let fd = transport.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over transport {id} (fd={fd})");

                self.transport_map
                    .remove(&fd)
                    .expect("transport index content doesn't match registered transports");
                self.poller.unregister(&transport);
                self.service.handover_transport(transport);
            }
            Action::Send(id, data) => {
                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Sending {} bytes to {id}", data.len());

                let transport = self.transports.get_mut(&id).ok_or_else(|| {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Transport {id} is not in the reactor");

                    Error::TransportUnknown(id)
                })?;
                transport.write_atomic(&data).map_err(|err| match err {
                    WriteError::NotReady => {
                        #[cfg(feature = "log")]
                        log::error!(target: "reactor", internal = true; 
                                "An attempt to write to transport {id} before it got ready");
                        Error::WriteLogicError(id, data)
                    }
                    WriteError::Io(e) => {
                        #[cfg(feature = "log")]
                        log::error!(target: "reactor", "Error writing to transport {id}: {e:?}");
                        Error::WriteFailure(id, e)
                    }
                })?;
            }
            Action::SetTimer(duration) => {
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Adding timer {duration:?} from now");

                self.timeouts.set_timer(duration, time);
            }
        }
        Ok(())
    }

    fn handle_shutdown(self) {
        #[cfg(feature = "log")]
        log::info!(target: "reactor", "Shutdown");

        // We just drop here?
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
    impl Write for DumbRes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> { Ok(buf.len()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    impl WriteAtomic for DumbRes {
        fn is_ready_to_write(&self) -> bool { true }
        fn empty_write_buf(&mut self) -> io::Result<bool> { Ok(true) }
        fn write_or_buf(&mut self, buf: &[u8]) -> io::Result<()> { Ok(()) }
    }
    impl Resource for DumbRes {
        type Id = RawFd;
        type Event = ();
        fn id(&self) -> Self::Id { self.0.as_raw_fd() }
        fn interests(&self) -> IoType { IoType::read_write() }
        fn handle_io(&mut self, io: Io) -> Option<Self::Event> { None }
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
                    Some(Action::SetTimer(Duration::from_secs(3)))
                } else {
                    None
                }
            }
        }
        impl Handler for DumbService {
            type Listener = DumbRes;
            type Transport = DumbRes;
            type Command = Cmd;

            fn tick(&mut self, time: Timestamp) {}
            fn handle_timer(&mut self) {
                self.log.push(Event::Timer);
                self.set_timer = true;
            }
            fn handle_listener_event(
                &mut self,
                id: <Self::Listener as Resource>::Id,
                event: <Self::Listener as Resource>::Event,
                time: Timestamp,
            ) {
                unreachable!()
            }
            fn handle_transport_event(
                &mut self,
                id: <Self::Transport as Resource>::Id,
                event: <Self::Transport as Resource>::Event,
                time: Timestamp,
            ) {
                unreachable!()
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
            fn handle_error(&mut self, err: Error<Self::Listener, Self::Transport>) {
                panic!("{err}")
            }
            fn handover_listener(&mut self, listener: Self::Listener) { unreachable!() }
            fn handover_transport(&mut self, transport: Self::Transport) { unreachable!() }
        }

        let reactor = Reactor::new(DumbService::default(), poller::popol::Poller::new()).unwrap();
        reactor.controller().cmd(Cmd::Init).unwrap();
        sleep(Duration::from_secs(20));
        reactor.controller().cmd(Cmd::Expect(vec![Event::Timer; 6])).unwrap();
    }
}
