use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::TcpStream;
use std::time::Duration;

pub enum IoStatus {
    Success(usize),
    WouldBlock,
    Shutdown,
    Err(Error),
}

pub trait ReadNonblocking: Read {
    fn set_read_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()>;

    fn read_nonblocking(&mut self, buf: &mut [u8]) -> IoStatus {
        match self.read(buf) {
            // If we get zero bytes read as a return value, it means we are still
            // in the process of handshake.
            Ok(len) => IoStatus::Success(len),
            Err(err) if err.kind() == ErrorKind::WouldBlock => IoStatus::WouldBlock,
            Err(err) => IoStatus::Err(err),
        }
    }
}

impl ReadNonblocking for TcpStream {
    fn set_read_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.set_nonblocking(true)?;
        self.set_read_timeout(timeout)
    }
}

#[cfg(feature = "socket2")]
impl ReadNonblocking for socket2::Socket {
    fn set_read_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.set_nonblocking(true)?;
        self.set_read_timeout(timeout)
    }
}

pub trait WriteNonblocking: Write {
    fn set_write_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()>;

    fn can_write(&self) -> bool;

    fn write_nonblocking(&mut self, buf: &[u8]) -> IoStatus {
        if !self.can_write() {
            return IoStatus::Err(ErrorKind::NotConnected.into());
        }
        if buf.is_empty() {
            return IoStatus::Success(0);
        }
        match self.write(buf) {
            Ok(0) => IoStatus::WouldBlock,
            Ok(len) => IoStatus::Success(len),
            Err(err) if err.kind() == ErrorKind::WriteZero => IoStatus::WouldBlock,
            Err(err) if err.kind() == ErrorKind::WouldBlock => IoStatus::WouldBlock,
            Err(err) => IoStatus::Err(err),
        }
    }

    fn flush_nonblocking(&mut self) -> IoStatus {
        match self.flush() {
            Ok(_) => IoStatus::Success(0),
            Err(err) if err.kind() == ErrorKind::WouldBlock => IoStatus::WouldBlock,
            Err(err) => IoStatus::Err(err),
        }
    }
}

// TODO: Do a dedicated non-blocking TcpStream type
impl WriteNonblocking for TcpStream {
    fn set_write_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.set_nonblocking(true)?;
        self.set_write_timeout(timeout)
    }

    fn can_write(&self) -> bool {
        // In the reality we might not, but this would be decided on the levels above
        true
    }
}

#[cfg(feature = "socket2")]
impl WriteNonblocking for socket2::Socket {
    fn set_write_nonblocking(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.set_nonblocking(true)?;
        self.set_write_nonblocking(timeout)
    }

    fn can_write(&self) -> bool {
        // In the reality we might not, but this would be decided on the levels above
        true
    }
}
