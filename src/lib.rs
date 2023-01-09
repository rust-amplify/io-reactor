//! Poll reactor ([`Reactor`]) manages multiple resources of two main kinds:
//! listeners and sessions. It uses a dedicated thread blocked on I/O events
//! from all of these resources (plus waker, plus timeouts) and than calls
//! the resource to process the I/O and generate (none or multiple) resource-
//! specific events.
//!
//! Once a resource generates events, reactor calls the service (in the context
//! of the runtime thread) to process each of the events.
//!
//! These events can be iterated by
//!
//! All resources under poll reactor must be representable as file descriptors.

#[macro_use]
extern crate amplify;

pub mod poller;
mod reactor;
mod resource;
mod timeouts;

pub use reactor::{Action, Controller, Error, Handler, Reactor};
pub use resource::{Io, Resource, ResourceId, WriteAtomic, WriteError};
pub use timeouts::TimeoutManager;
