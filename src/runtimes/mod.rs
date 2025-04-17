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

use std::fmt::Debug;

pub use controller::Controller;
use crossbeam_channel::Receiver;

use crate::poller::{Poll, Waker, WakerSend};
use crate::runtimes::controller::Ctl;

/// Handler for a service run by a reactor.
pub trait ReactorHandler: Send {
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
}

/// Trait for specific reactor runtime implementations.
///
/// Reactor runtime runs inside the reactor thread and manages resources inside the reactor.
pub trait ReactorRuntime<P: Poll> {
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

    /// Executes reactor even loop.
    fn run(self);

    /// Provides a copy of a [`Controller`] object which exposes an API to the reactor and a service
    /// running inside its thread.
    fn controller(&self) -> Controller<<Self::Handler as ReactorHandler>::Command, impl WakerSend>;
}
