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

#![allow(unused_variables)] // because we need them for feature-gated logger

use std::io;

use crossbeam_channel as chan;

use crate::poller::WakerSend;

pub enum Ctl<C> {
    Cmd(C),
    Shutdown,
}

/// Control API to the service which is run inside a reactor.
///
/// The service is passed to the [`Reactor`] constructor as a parameter and also exposes [`Handler`]
/// API to the reactor itself for receiving reactor-generated events. This API is used by the
/// reactor to inform the service about incoming commands, sent via this [`Controller`] API (see
/// [`Handler::Command`] for the details).
pub struct Controller<C, W: WakerSend> {
    ctl_send: chan::Sender<Ctl<C>>,
    waker: W,
}

impl<C, W: WakerSend> Clone for Controller<C, W> {
    fn clone(&self) -> Self {
        Controller {
            ctl_send: self.ctl_send.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<C, W: WakerSend> Controller<C, W> {
    pub(crate) fn new(ctl_send: chan::Sender<Ctl<C>>, waker: W) -> Self { Self { ctl_send, waker } }

    /// Send a command to the service inside a [`Reactor`] or a reactor [`Runtime`].
    #[allow(unused_mut)] // because of the `log` feature gate
    pub fn cmd(&self, mut command: C) -> Result<(), io::Error>
    where C: 'static {
        #[cfg(feature = "log")]
        {
            use std::any::Any;
            use std::fmt::Debug;

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

    pub(crate) fn wake(&self) -> io::Result<()> {
        #[cfg(feature = "log")]
        log::trace!(target: "reactor-controller", "Wakening the reactor");
        self.waker.wake()
    }
}
