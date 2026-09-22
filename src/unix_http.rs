//! A `ureq` agent whose every connection is the one Unix domain socket it was
//! built for. fux serves BRP only on a Unix socket; this is the client half.
//!
//! The URL's authority is HTTP metadata only (the `Host` header and the pool
//! key), never a destination: the resolver performs no lookup and the connector
//! ignores the address it is given. Proxies and redirects are disabled, so no
//! configuration or response can move a request onto TCP.
//!
//! Shared by the `fux` binary, its integration tests and `fux-fuzz` through
//! `#[path]`, so every client speaks to the socket the same way.
use std::{
    fmt,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};
use ureq::{
    Agent, Error,
    config::Config,
    http::Uri,
    unversioned::{
        resolver::{ResolvedSocketAddrs, Resolver},
        transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport},
    },
};

/// The URL every request is sent to. Only the path matters to fux.
pub const URL: &str = "http://fux/";

/// An agent that dials `socket`. `timeout` is the whole-call budget; `None`
/// is for a long-lived stream such as `fux.frame+watch`.
pub fn agent(socket: &Path, timeout: Option<Duration>) -> Agent {
    let config = Agent::config_builder()
        .timeout_global(timeout)
        .proxy(None)
        .max_redirects(0)
        .build();
    Agent::with_parts(config, UnixConnector(socket.to_owned()), NoLookup)
}

struct UnixConnector(PathBuf);

impl fmt::Debug for UnixConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UnixConnector").field(&self.0).finish()
    }
}

impl Connector<()> for UnixConnector {
    type Out = UnixTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, Error> {
        let stream = UnixStream::connect(&self.0).map_err(|error| {
            Error::Io(io::Error::new(
                error.kind(),
                format!("{}: {error}", self.0.display()),
            ))
        })?;
        let config = details.config;
        Ok(Some(UnixTransport {
            stream,
            buffers: LazyBuffers::new(config.input_buffer_size(), config.output_buffer_size()),
            read_timeout: None,
            write_timeout: None,
        }))
    }
}

/// Resolves nothing: the socket path is the only destination.
#[derive(Debug)]
struct NoLookup;

impl Resolver for NoLookup {
    fn resolve(
        &self,
        _uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let mut addresses = self.empty();
        addresses.push(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)));
        Ok(addresses)
    }
}

struct UnixTransport {
    stream: UnixStream,
    buffers: LazyBuffers,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl fmt::Debug for UnixTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixTransport").finish()
    }
}

/// Mirrors ureq's TCP transport: a would-block from a timed socket is a timeout.
fn timed<T>(result: io::Result<T>, timeout: &NextTimeout) -> Result<T, Error> {
    match result {
        Ok(value) => Ok(value),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Err(Error::Timeout(timeout.reason))
        }
        Err(error) => Err(error.into()),
    }
}

impl Transport for UnixTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        let wanted = timeout.not_zero().map(|t| *t);
        if wanted != self.write_timeout {
            self.stream.set_write_timeout(wanted)?;
            self.write_timeout = wanted;
        }
        let output = self.buffers.output().get(..amount).unwrap_or_default();
        timed(self.stream.write_all(output), &timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        let wanted = timeout.not_zero().map(|t| *t);
        if wanted != self.read_timeout {
            self.stream.set_read_timeout(wanted)?;
            self.read_timeout = wanted;
        }
        let input = self.buffers.input_append_buf();
        let amount = timed(self.stream.read(input), &timeout)?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        // A pooled connection is reusable only if the server has neither
        // closed it nor sent anything unasked.
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let open = matches!(
            self.stream.read(&mut [0]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        );
        open && self.stream.set_nonblocking(false).is_ok()
    }
}
