# I/O reactor

![Build](https://github.com/rust-amplify/io-reactor/workflows/Build/badge.svg)
![Tests](https://github.com/rust-amplify/io-reactor/workflows/Tests/badge.svg)
![Lints](https://github.com/rust-amplify/io-reactor/workflows/Lints/badge.svg)
[![codecov](https://codecov.io/gh/rust-amplify/io-reactor/branch/master/graph/badge.svg)](https://codecov.io/gh/rust-amplify/io-reactor)

[![crates.io](https://img.shields.io/crates/v/io-reactor)](https://crates.io/crates/io-reactor)
[![Docs](https://docs.rs/io-reactor/badge.svg)](https://docs.rs/io-reactor)
[![Apache-2 licensed](https://img.shields.io/crates/l/io-reactor)](./LICENSE)

### _Concurrent I/O without rust async problems_

This repository provides a set of libraries for concurrent access to I/O
resources (file, network, devices etc) which uses reactor pattern with
pluggable `poll` syscall engines. This allows to handle situations like
multiple network connections within the scope of a single thread (see 
[C10k problem]).

The crate can be used for building concurrent microservice architectures 
without polling all APIs with `async`s.

The reactor design pattern is an event handling pattern for handling service 
requests delivered concurrently to a service handler by one or more inputs. 
The service handler then demultiplexes the incoming requests and dispatches 
them synchronously to the associated service[^1].

Core concepts:
- **Resource**: any resource that can provide input to or consume output from 
  the system.
- **Runtime**: runs an event loop to block on all resources. Sends the resource
  service when it is possible to start a synchronous operation on a resource 
  without blocking (Example: a synchronous call to read() will block if there 
  is no data to read.
- **Service**: custom business logic provided by the application for a given
  set of resources. Service exposes a **Handler API** for the reactor *runtime*.
  It is also rsponsible for the creation and destruction of the resources.


## Documentation

API reference documentation for the library can be accessed at
<https://docs.rs/io-reactor/>.


## Licensing

The libraries are distributed on the terms of Apache 2.0 opensource license.
See [LICENCE](LICENSE) file for the license details.

[C10k problem]: https://en.wikipedia.org/wiki/C10k_problem
[^1]: Schmidt, Douglas et al. Pattern-Oriented Software Architecture Volume 2: Patterns for Concurrent and Networked Objects. Volume 2. Wiley, 2000.
