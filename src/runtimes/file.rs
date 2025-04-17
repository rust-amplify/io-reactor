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

//! File reactor runtime.

#![allow(unused_variables)] // because we need them for feature-gated logger

use std::collections::HashMap;
use std::fmt::Debug;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Duration;

use crossbeam_channel as chan;
use crossbeam_channel::Receiver;

use super::{Controller, ReactorHandler, ReactorRuntime};
use crate::poller::{IoType, Poll, Waker, WakerRecv, WakerSend};
use crate::resource::WriteError;
use crate::resources::{FileEvent, FileResource};
use crate::runtimes::controller::Ctl;
use crate::{Resource, ResourceId, ResourceType, Timer, Timestamp, WriteAtomic};

/// Maximum amount of time to wait for I/O.
const WAIT_TIMEOUT: Duration = Duration::from_secs(1);

/// Reactor errors
#[derive(Debug, Error, Display, From)]
#[display(doc_comments)]
pub enum Error {
    /// file {0} got closed during poll operation.
    Closed(ResourceId, FileResource),

    /// polling multiple resources has failed. Details: {0:?}
    Poll(io::Error),
}

/// Actions which can be provided to the reactor by the [`Handler`].
///
/// Reactor reads actions on each event loop using [`Handler`] iterator interface.
#[derive(Display)]
pub enum Action {
    /// Register a new file resource for the reactor poll.
    ///
    /// NB: Reactor can't open the file. Reactor only can register already opened file as a
    /// resource for polling in the event loop.
    #[display("register_file")]
    RegisterFile(FileResource),

    /// Unregister file resource from the reactor poll and handover it to the [`Handler`] via
    /// [`Handler::handover_file`].
    ///
    /// When the file is unregistered no action is performed, i.e. the file descriptor is not
    /// closed. All these actions must be handled by the handler upon the handover event.
    #[display("unregister_transport")]
    UnregisterFile(ResourceId),

    /// Write the data to one of the file resources using [`io::Write`].
    #[display("send_to({0})")]
    Write(ResourceId, Vec<u8>),

    // TODO: Add read, seek and close actions.
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
pub trait Handler: ReactorHandler + Iterator<Item = Action> {
    /// Method called by the reactor on the start of each event loop once the poll has returned.
    fn tick(&mut self, time: Timestamp);

    /// Method called by the reactor when a previously set timeout is fired.
    ///
    /// Related: [`Action::SetTimer`].
    fn handle_timer(&mut self);

    /// Method called by the reactor upon I/O event on a file resource.
    fn handle_event(&mut self, fd: RawFd, id: ResourceId, event: FileEvent, time: Timestamp);

    /// Method called by the reactor when a given file was successfully registered and provided
    /// with a resource id.
    ///
    /// The resource id will be used later in [`Self::handle_event`] and [`handover_file`]
    /// calls to the handler.
    fn handle_registered(&mut self, fd: RawFd, id: ResourceId, ty: ResourceType);

    /// Method called by the reactor when a [`Self::Command`] is received for the [`Handler`].
    ///
    /// The commands are sent via [`Controller`] from outside the reactor, including other threads.
    fn handle_command(&mut self, cmd: Self::Command);

    /// Method called by the reactor on any kind of error during the event loop, including errors of
    /// the poll syscall or I/O errors returned as a part of the poll result events.
    ///
    /// See [`enum@Error`] for the details on errors which may happen.
    fn handle_error(&mut self, err: Error);

    /// Method called by the reactor upon receiving [`Action::UnregisterFile`].
    ///
    /// Passes the transport resource to the [`Handler`] when it is already not a part of the
    /// reactor poll. From this point of time it is safe to send the resource to other threads
    /// (like workers) or close the resource.
    fn handover_file(&mut self, id: ResourceId, file: FileResource);
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
    files: HashMap<ResourceId, FileResource>,
    waker: <P::Waker as Waker>::Recv,
    timeouts: Timer,
}

impl<H: Handler, P: Poll> Runtime<H, P> {
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
            } else if self.files.contains_key(&id) {
                match res {
                    Ok(io) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Got `{io}` event from transport {id}");

                        let transport = self.files.get_mut(&id).expect("resource disappeared");
                        for io in io {
                            if let Some(event) = transport.handle_io(io) {
                                let fd = transport.as_raw_fd();
                                self.service.handle_event(fd, id, event, time);
                            }
                        }
                    }
                    Err(err) => {
                        #[cfg(feature = "log")]
                        log::trace!(target: "reactor", "Transport {id} {err}");
                        let transport =
                            self.unregister_file(id).expect("transport has disappeared");
                        self.service.handle_error(Error::Closed(id, transport));
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

    /// Handles the actions from the queue.
    ///
    /// # Return
    ///
    /// Return value indicates whether the reactor must proceed operating (`true`) or should
    /// terminate (`false`).
    fn handle_actions(&mut self, time: Timestamp) -> bool {
        let mut result = true;
        while let Some(action) = self.service.next() {
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Handling action {action} from the service");

            // NB: Deadlock may happen here if the service will generate events over and over
            // in the handle_* calls we may never get out of this loop
            match self.handle_action(action, time) {
                Ok(ret) => result |= ret,
                Err(err) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Error: {err}");
                    self.service.handle_error(err);
                }
            }
        }
        result
    }

    /// # Safety
    ///
    /// Panics on `Action::Send` for read-only resources or resources which are not ready for a
    /// write operation (i.e. returning `false` from [`WriteAtomic::is_ready_to_write`]
    /// implementation.
    fn handle_action(&mut self, action: Action, time: Timestamp) -> Result<bool, Error> {
        match action {
            Action::RegisterFile(file) => {
                let fd = file.as_raw_fd();

                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Registering file with fd={fd}");

                let id = self.poller.register(&file, IoType::read_only());
                self.files.insert(id, file);
                self.service.handle_registered(fd, id, ResourceType::Transport);
            }
            Action::UnregisterFile(id) => {
                let Some(transport) = self.unregister_file(id) else {
                    return Ok(true);
                };
                #[cfg(feature = "log")]
                log::debug!(target: "reactor", "Handling over file {id}");
                self.service.handover_file(id, transport);
            }
            Action::Write(id, data) => {
                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Sending {} bytes to {id}", data.len());

                let Some(file) = self.files.get_mut(&id) else {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Transport {id} is not in the reactor");

                    return Ok(true);
                };
                match file.write_atomic(&data) {
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
                        if let Some(transport) = self.unregister_file(id) {
                            return Err(Error::Closed(id, transport));
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

    fn unregister_file(&mut self, id: ResourceId) -> Option<FileResource> {
        let Some(file) = self.files.remove(&id) else {
            #[cfg(feature = "log")]
            log::warn!(target: "reactor", "Unregistering non-registered file {id}");
            return None;
        };

        #[cfg(feature = "log")]
        log::debug!(target: "reactor", "Unregistering over file {id} (fd={})", file.as_raw_fd());

        self.poller.unregister(id);

        Some(file)
    }
}

impl<H: Handler + 'static, P: Poll> ReactorRuntime<P> for Runtime<H, P> {
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
            files: empty!(),
            waker: waker_recv,
            timeouts: Timer::new(),
        }
    }

    fn run(mut self) {
        loop {
            let before_poll = Timestamp::now();
            let timeout = self.timeouts.next_expiring_from(before_poll).unwrap_or(WAIT_TIMEOUT);

            for (id, res) in &self.files {
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
                        Ok(Ctl::Cmd(cmd)) => self.service.handle_command(cmd),
                    }
                }
            }

            if !self.handle_actions(now) {
                break;
            };
        }
    }

    fn controller(&self) -> Controller<<Self::Handler as ReactorHandler>::Command, impl WakerSend> {
        self.controller.clone()
    }
}
