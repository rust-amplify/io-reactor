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

#![deny(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unused_mut,
    unused_imports,
    dead_code,
    missing_docs
)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

//! ### _Concurrent I/O without rust async problems_
//!
//! This repository provides a set of libraries for concurrent access to I/O resources (file,
//! network, devices etc) which uses reactor pattern with pluggable `poll` syscall engines. This
//! allows to handle situations like multiple network connections within the scope of a single
//! thread (see [C10k problem]).
//!
//! The crate can be used for building concurrent microservice architectures without polling all
//! APIs with `async`s.
//!
//! The reactor design pattern is an event handling pattern for handling service requests delivered
//! concurrently to a service handler by one or more inputs. The service handler then demultiplexes
//! the incoming requests and dispatches them synchronously to the associated request handlers[^1].
//!
//! Core concepts:
//! - **[`Resource`]**: any resource that can provide input to or consume output from the system.
//! - **[`Runtime`]**: runs an event loop to block on all resources. Sends the resource service when
//!   it is possible to start a synchronous operation on a resource without blocking (Example: a
//!   synchronous call to read() will block if there is no data to read.
//! - **Service**: custom business logic provided by the application for a given set of resources.
//!   Service exposes a **[`Handler`] API** for the reactor [`Runtime`]. It is also responsible for
//!   the creation and destruction of the resources.
//!
//! [`Reactor`] exposes a high-level API and manages multiple resources of two main kinds listeners
//! and sessions. It uses a dedicated thread blocked on I/O events from all of these resources and
//! than calls the [`Resource::handle_io`] to process the I/O and generate resource-specific event.
//!
//! Once a resource generates an event, reactor calls [`Handler`] API (in the context of the runtime
//! thread) to process each of the events.
//!
//! All resources under poll reactor must be representable as file descriptors.
//!
//! [C10k problem]: https://en.wikipedia.org/wiki/C10k_problem
//! [^1]: Schmidt, Douglas et al. Pattern-Oriented Software Architecture Volume 2: Patterns for
//!       Concurrent and Networked Objects. Volume 2. Wiley, 2000.

#[macro_use]
extern crate amplify;

pub mod poller;
mod reactor;
mod resource;
mod timeouts;

pub use resource::{
    Io, Resource, ResourceId, ResourceIdGenerator, ResourceType, WriteAtomic, WriteError,
};
pub use timeouts::{Timer, Timestamp};

pub use self::reactor::{Action, Controller, Error, Handler, Reactor, Runtime};


mod os {
    #[cfg(unix)]
    pub(crate) use std::os::unix::io::{AsRawFd as AsRaw, RawFd as Raw};

    #[cfg(windows)]
    pub(crate) use std::os::windows::io::{AsRawSocket as AsRaw, RawSocket as Raw};
}