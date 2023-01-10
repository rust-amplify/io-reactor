use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io;
use std::io::Write;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel as chan;

use crate::poller::{IoFail, IoType, Poll};
use crate::resource::WriteError;
use crate::{Resource, TimeoutManager, WriteAtomic};

/// Maximum amount of time to wait for i/o.
const WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

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

    /// polling multiple resources has failed. Details: {0:?}
    Poll(io::Error),
}

impl<L: Resource, T: Resource> Debug for Error<L, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

#[derive(Display)]
pub enum Action<L: Resource, T: Resource> {
    #[display("register_listener")]
    RegisterListener(L),
    #[display("register_transport")]
    RegisterTransport(T),
    #[display("unregister_listener")]
    UnregisterListener(L::Id),
    #[display("unregister_transport")]
    UnregisterTransport(T::Id),
    #[display("send_to({0})")]
    Send(T::Id, Vec<u8>),
    #[display("set_timer({0:?})")]
    SetTimer(Duration),
}

pub trait Handler: Send + Iterator<Item = Action<Self::Listener, Self::Transport>> {
    type Listener: Resource;
    type Transport: Resource;
    type Command: Debug + Send;

    fn tick(&mut self, time: Instant);

    fn handle_wakeup(&mut self);

    fn handle_listener_event(
        &mut self,
        id: <Self::Listener as Resource>::Id,
        event: <Self::Listener as Resource>::Event,
        time: Instant,
    );

    fn handle_transport_event(
        &mut self,
        id: <Self::Transport as Resource>::Id,
        event: <Self::Transport as Resource>::Event,
        time: Instant,
    );

    fn handle_command(&mut self, cmd: Self::Command);

    fn handle_error(&mut self, err: Error<Self::Listener, Self::Transport>);

    /// Called by the reactor upon receiving [`Action::UnregisterListener`]
    fn handover_listener(&mut self, listener: Self::Listener);
    /// Called by the reactor upon receiving [`Action::UnregisterTransport`]
    fn handover_transport(&mut self, transport: Self::Transport);
}

pub struct Reactor<S: Handler> {
    thread: JoinHandle<()>,
    controller: Controller<S>,
}

impl<S: Handler> Reactor<S> {
    pub fn new<P: Poll>(service: S, mut poller: P) -> Result<Self, io::Error>
    where
        S: 'static,
        P: 'static,
    {
        let (ctl_send, ctl_recv) = chan::unbounded();
        let (cmd_send, cmd_recv) = chan::unbounded();

        let (waker_writer, waker_reader) = UnixStream::pair()?;
        waker_reader.set_nonblocking(true)?;
        waker_writer.set_nonblocking(true)?;

        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Initializing reactor thread...");
        let thread = std::thread::spawn(move || {
            #[cfg(feature = "log")]
            log::debug!(target: "reactor", "Registering waker (fd {})", waker_reader.as_raw_fd());
            poller.register(&waker_reader, IoType::read_only());

            let runtime = Runtime {
                service,
                poller,
                cmd_recv,
                ctl_recv,
                listeners: empty!(),
                transports: empty!(),
                listener_map: empty!(),
                transport_map: empty!(),
                waker: waker_reader,
                timeouts: TimeoutManager::new(Duration::from_secs(1)),
            };

            #[cfg(feature = "log")]
            log::info!(target: "reactor", "Entering reactor event loop");

            runtime.run();
        });

        let controller = Controller {
            cmd_send,
            ctl_send,
            waker: Arc::new(Mutex::new(waker_writer)),
        };
        // Waking up to consume actions which were provided by the service on launch
        controller.wake()?;
        Ok(Self { thread, controller })
    }

    pub fn controller(&self) -> Controller<S> {
        self.controller.clone()
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.thread.join()
    }
}

enum Ctl<S: Handler> {
    RegisterListener(S::Listener),
    RegisterTransport(S::Transport),
    Shutdown,
}

pub struct Controller<S: Handler> {
    // TODO: Unify command anc control channels
    cmd_send: chan::Sender<S::Command>,
    ctl_send: chan::Sender<Ctl<S>>,
    waker: Arc<Mutex<UnixStream>>,
}

impl<S: Handler> Clone for Controller<S> {
    fn clone(&self) -> Self {
        Controller {
            cmd_send: self.cmd_send.clone(),
            ctl_send: self.ctl_send.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<S: Handler> Controller<S> {
    pub fn register_listener(&self, listener: S::Listener) -> Result<(), io::Error> {
        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Registering listener {}", listener.id());

        self.ctl_send
            .send(Ctl::RegisterListener(listener))
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.wake()?;
        Ok(())
    }

    pub fn register_transport(&self, transport: S::Transport) -> Result<(), io::Error> {
        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Registering transport {}", transport.id());

        self.ctl_send
            .send(Ctl::RegisterTransport(transport))
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.wake()?;
        Ok(())
    }

    pub fn shutdown(self) -> Result<(), Self> {
        #[cfg(feature = "log")]
        log::info!(target: "reactor-controller", "Initiating reactor shutdown...");

        let res1 = self.ctl_send.send(Ctl::Shutdown);
        let res2 = self.wake();
        res1.or(res2).map_err(|_| self)
    }

    pub fn send(&self, command: S::Command) -> Result<(), io::Error> {
        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Sending command {command:?} to the reactor");

        self.cmd_send
            .send(command)
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.wake()?;
        Ok(())
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
            libc::read(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
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

pub struct Runtime<H: Handler, P: Poll> {
    service: H,
    poller: P,
    cmd_recv: chan::Receiver<H::Command>,
    ctl_recv: chan::Receiver<Ctl<H>>,
    listener_map: HashMap<RawFd, <H::Listener as Resource>::Id>,
    transport_map: HashMap<RawFd, <H::Transport as Resource>::Id>,
    listeners: HashMap<<H::Listener as Resource>::Id, H::Listener>,
    transports: HashMap<<H::Transport as Resource>::Id, H::Transport>,
    waker: UnixStream,
    timeouts: TimeoutManager,
}

impl<H: Handler, P: Poll> Runtime<H, P> {
    fn run(mut self) {
        loop {
            let timeout = self
                .timeouts
                .next(Instant::now())
                .unwrap_or(WAIT_TIMEOUT)
                .into();

            for res in self.listeners.values() {
                self.poller.set_interest(res, res.interests());
            }
            for res in self.transports.values() {
                self.poller.set_interest(res, res.interests());
            }

            // Blocking
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Polling with timeout {timeout:?}");
            match self.poller.poll(Some(timeout)) {
                Ok(0) => {
                    #[cfg(feature = "log")]
                    log::trace!(target: "reactor", "Timeout");
                    continue;
                }
                Ok(count) => count,
                Err(err) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Error during polling: {err}");
                    self.service.handle_error(Error::Poll(err));
                    continue;
                }
            };

            let instant = Instant::now();
            self.service.tick(instant);

            let awoken = self.handle_events(instant);

            // Process the commands only if we awaken by the waker
            if awoken {
                loop {
                    match self.cmd_recv.try_recv() {
                        Err(chan::TryRecvError::Empty) => break,
                        Err(chan::TryRecvError::Disconnected) => {
                            panic!("control channel is broken")
                        }
                        Ok(cmd) => self.service.handle_command(cmd),
                    }
                }
                loop {
                    match self.ctl_recv.try_recv() {
                        Err(chan::TryRecvError::Empty) => break,
                        Err(chan::TryRecvError::Disconnected) => {
                            panic!("shutdown channel is broken")
                        }
                        Ok(Ctl::Shutdown) => return self.handle_shutdown(),
                        Ok(Ctl::RegisterListener(listener)) => self
                            .handle_action(Action::RegisterListener(listener), instant)
                            .expect("register actions do not error"),
                        Ok(Ctl::RegisterTransport(transport)) => self
                            .handle_action(Action::RegisterTransport(transport), instant)
                            .expect("register actions do not error"),
                    }
                }
            }

            self.handle_actions(instant);
        }
    }

    /// # Returns
    ///
    /// Whether it was awaken by a waker
    fn handle_events(&mut self, time: Instant) -> bool {
        let mut awoken = false;

        let mut unregister_queue = vec![];
        for (fd, res) in &mut self.poller {
            if fd == self.waker.as_raw_fd() {
                if let Err(err) = res {
                    log::error!(target: "reactor", "Polling waker has failed: {err}");
                    panic!("waker failure");
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
                        self.service
                            .handle_error(Error::ListenerDisconnect(*id, listener, flags));
                    }
                    Err(IoFail::Os(flags)) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Listener {id} errored (OS flags {flags:#b})");

                        self.service
                            .handle_error(Error::ListenerPollError(*id, flags));
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

                        self.service
                            .handle_error(Error::TransportPollError(*id, flags));
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

    fn handle_actions(&mut self, time: Instant) {
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
        time: Instant,
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
                let listener = self
                    .listeners
                    .remove(&id)
                    .ok_or(Error::ListenerUnknown(id))?;
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
                let transport = self
                    .transports
                    .remove(&id)
                    .ok_or(Error::TransportUnknown(id))?;
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
                log::debug!(target: "reactor", "Adding timer {duration:?}");

                self.timeouts.register((), time + duration);
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
