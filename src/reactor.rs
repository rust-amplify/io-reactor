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

use std::os::fd::AsRawFd;
use std::thread::JoinHandle;
use std::{io, thread};

use crate::poller::{Poll, Waker};
use crate::runtimes::{Controller, ReactorHandler, ReactorRuntime};

/// High-level reactor API wrapping reactor [`Runtime`] into a thread and providing basic thread
/// management for it.
///
/// Apps running the [`Reactor`] can interface it and a [`Handler`] via use of the [`Controller`]
/// API.
pub struct Reactor<R: ReactorRuntime<P>, P: Poll> {
    thread: JoinHandle<()>,
    controller: Controller<<R::Handler as ReactorHandler>::Command, <P::Waker as Waker>::Send>,
}

impl<R: ReactorRuntime<P>, P: Poll> Reactor<R, P> {
    /// Creates new reactor using provided [`Poll`] engine and a service exposing [`Handler`] API to
    /// the reactor.
    ///
    /// Both poll engine and the service are sent to the newly created reactor thread which runs the
    /// reactor [`Runtime`].
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn new(service: R::Handler, poller: P) -> Result<Self, io::Error>
    where
        P: 'static,
        R: 'static,
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
    pub fn named(service: R::Handler, poller: P, thread_name: String) -> Result<Self, io::Error>
    where
        P: 'static,
        R: 'static,
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
    /// # Blocking
    ///
    /// This call is blocking.
    ///
    /// # Error
    ///
    /// Errors with a system/OS error if it was impossible to spawn a thread.
    pub fn with(
        service: R::Handler,
        mut poller: P,
        builder: thread::Builder,
    ) -> Result<Self, io::Error>
    where
        P: 'static,
    {
        let (ctl_send, ctl_recv) = crossbeam_channel::unbounded();

        let (waker_writer, waker_reader) = P::Waker::pair()?;

        let controller = Controller::new(ctl_send, waker_writer);

        #[cfg(feature = "log")]
        log::debug!(target: "reactor-controller", "Initializing reactor thread...");

        let runtime_controller = controller.clone();
        let thread = builder.spawn(move || {
            #[cfg(feature = "log")]
            log::debug!(target: "reactor", "Registering waker (fd {})", waker_reader.as_raw_fd());
            poller.register_waker(&waker_reader);

            let runtime = R::with(service, poller, runtime_controller, ctl_recv, waker_reader);

            #[cfg(feature = "log")]
            log::info!(target: "reactor", "Entering reactor event loop");

            runtime.run();
        })?;

        // Waking up to consume actions which were provided by the service on launch
        controller.wake()?;
        Ok(Self { thread, controller })
    }

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside its thread.
    ///
    /// See [`Handler::Command`] for the details.
    pub fn controller(
        &self,
    ) -> Controller<<R::Handler as ReactorHandler>::Command, <P::Waker as Waker>::Send> {
        self.controller.clone()
    }

    /// Joins the reactor thread.
    pub fn join(self) -> thread::Result<()> { self.thread.join() }
}
