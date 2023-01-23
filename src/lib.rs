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
    //missing_docs
)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

//! Poll reactor ([`Reactor`]) manages multiple reactor of two main kinds:
//! listeners and sessions. It uses a dedicated thread blocked on I/O events
//! from all of these reactor (plus waker, plus timeouts) and than calls
//! the resource to process the I/O and generate (none or multiple) resource-
//! specific events.
//!
//! Once a resource generates events, reactor calls the service (in the context
//! of the runtime thread) to process each of the events.
//!
//! These events can be iterated by
//!
//! All reactor under poll reactor must be representable as file descriptors.

#[macro_use]
extern crate amplify;

pub mod poller;
mod reactor;
mod resource;
mod timeouts;

pub use resource::{Io, Resource, ResourceId, WriteAtomic, WriteError};
pub use timeouts::TimeoutManager;

pub use self::reactor::{Action, Controller, Error, Handler, Reactor, Runtime};
