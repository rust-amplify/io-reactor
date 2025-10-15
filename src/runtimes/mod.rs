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

//! Reactor runtimes.

mod controller;
pub mod net;
pub mod file;

use std::error::Error as StdError;
use std::fmt::{Debug, Display};
use std::io;
use std::time::Duration;

pub use controller::Controller;
use crossbeam_channel::Receiver;

use crate::poller::{IoType, Poll, Waker, WakerSend};
use crate::runtimes::controller::Ctl;
use crate::{ResourceId, Timer, Timestamp};

/// Reactor errors
#[derive(Debug, Error, Display)]
#[display(doc_comments)]
pub enum Error<E: StdError> {
    /// file {0} got closed during poll operation.
    Handler(E),

    /// polling multiple resources has failed. Details: {0:?}
    Poll(io::Error),
}

/// Handler for a service run by a reactor.
pub trait ReactorHandler: Send + Iterator<Item = Self::Action> {
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

    /// I/O action generated within the service handler.
    type Action: Display;

    /// Error type returned by the service handler.
    type Error: StdError;

    /// Method called by the reactor on any kind of error during the event loop, including errors of
    /// the poll syscall or I/O errors returned as a part of the poll result events.
    ///
    /// See [`enum@Error`] for the details on errors which may happen.
    fn handle_error(&mut self, err: Error<Self::Error>);

    /// Method called by the reactor on the start of each event loop once the poll has returned.
    fn tick(&mut self, time: Timestamp);

    /// Method called by the reactor when a previously set timeout is fired.
    ///
    /// Related: [`Action::SetTimer`].
    fn handle_timer(&mut self);

    /// Method called by the reactor when a [`Self::Command`] is received for the [`Handler`].
    ///
    /// The commands are sent via [`Controller`] from outside the reactor, including other
    /// threads.
    fn handle_command(&mut self, cmd: Self::Command);
}

/// Trait for specific reactor runtime implementations.
///
/// Reactor runtime runs inside the reactor thread and manages resources inside the reactor.
pub trait ReactorRuntime<P: Poll>: Sized {
    /// Maximum amount of time to wait for I/O.
    const WAIT_TIMEOUT: Duration;

    /// Handler for a service run by a reactor.
    type Handler: ReactorHandler + 'static;

    /// Creates new reactor runtime using provided [`Poll`] engine and a service exposing
    /// [`ReactorHandler`] API to the reactor.
    fn with(
        service: Self::Handler,
        poller: P,
        controller: Controller<
            <Self::Handler as ReactorHandler>::Command,
            <P::Waker as Waker>::Send,
        >,
        ctl_recv: Receiver<Ctl<<Self::Handler as ReactorHandler>::Command>>,
        waker_recv: <P::Waker as Waker>::Recv,
    ) -> Self;

    /// Accessor for timer.
    fn timeouts(&mut self) -> &mut Timer;
    /// Accessor for poller.
    fn poller(&mut self) -> &mut P;
    /// Accessor for service.
    fn service(&mut self) -> &mut Self::Handler;
    /// Iterator over resource interests.
    fn resource_interests(&self) -> impl Iterator<Item = (ResourceId, IoType)>;
    /// Reads control command send to the service from outside the reactor.
    fn ctl_recv(
        &self,
    ) -> Result<Ctl<<Self::Handler as ReactorHandler>::Command>, crossbeam_channel::TryRecvError>;

    /// Executes reactor even loop.
    fn run(mut self) {
        loop {
            let before_poll = Timestamp::now();
            let timeout =
                self.timeouts().next_expiring_from(before_poll).unwrap_or(Self::WAIT_TIMEOUT);

            let resources = self.resource_interests().collect::<Vec<_>>();
            for (id, io) in resources {
                self.poller().set_interest(id, io);
            }

            // Blocking
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Polling with timeout {timeout:?}");

            let res = self.poller().poll(Some(timeout));
            let now = Timestamp::now();
            self.service().tick(now);

            // Nb. The way this is currently used basically ignores which keys have
            // timed out. So as long as *something* timed out, we wake the service.
            let timers_fired = self.timeouts().remove_expired_by(now);
            if timers_fired > 0 {
                #[cfg(feature = "log")]
                log::trace!(target: "reactor", "Timer has fired");
                self.service().handle_timer();
            }

            match res {
                Ok(0) if timers_fired == 0 => {
                    #[cfg(feature = "log")]
                    log::trace!(target: "reactor", "Poll timeout; no I/O events had happened");
                }
                Err(err) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Error during polling: {err}");
                    self.service().handle_error(Error::Poll(err));
                }
                _ => {}
            }

            let awoken = self.handle_events(now);

            // Process the commands only if we awaken by the waker
            if awoken {
                loop {
                    match self.ctl_recv() {
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            panic!("control channel is broken")
                        }
                        Ok(Ctl::Shutdown) => return self.handle_shutdown(),
                        Ok(Ctl::Cmd(cmd)) => self.service().handle_command(cmd),
                    }
                }
            }

            if !self.handle_actions(now) {
                break;
            };
        }
    }

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside its thread.
    fn controller(&self) -> Controller<<Self::Handler as ReactorHandler>::Command, impl WakerSend>;

    /// Handles the actions from the queue.
    ///
    /// # Return
    ///
    /// Return value indicates whether the reactor must proceed operating (`true`) or should
    /// terminate (`false`).
    fn handle_actions(&mut self, time: Timestamp) -> bool {
        let mut result = true;
        while let Some(action) = self.service().next() {
            #[cfg(feature = "log")]
            log::trace!(target: "reactor", "Handling action {action} from the service");

            // NB: Deadlock may happen here if the service will generate events over and over
            // in the handle_* calls we may never get out of this loop
            match self.handle_action(action, time) {
                Ok(ret) => result |= ret,
                Err(err) => {
                    #[cfg(feature = "log")]
                    log::error!(target: "reactor", "Error: {err}");
                    self.service().handle_error(Error::Handler(err));
                }
            }
        }
        result
    }

    /// # Panics
    ///
    /// Panics on `Action::Send` for read-only resources or resources which are not ready for a
    /// write operation (i.e. returning `false` from [`WriteAtomic::is_ready_to_write`]
    /// implementation.
    fn handle_action(
        &mut self,
        action: <Self::Handler as ReactorHandler>::Action,
        time: Timestamp,
    ) -> Result<bool, <Self::Handler as ReactorHandler>::Error>;

    /// # Returns
    ///
    /// Whether it was awakened by a waker.
    fn handle_events(&mut self, time: Timestamp) -> bool;

    /// Handler fo a shutdown action.
    fn handle_shutdown(self);
}
