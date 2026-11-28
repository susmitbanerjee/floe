use std::net::SocketAddr;
use std::time::Duration;

/// How Floe's Postgres listener behaves, already checked by the binary's config loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSettings {
    /// Address the Postgres listener binds. Loopback only until authentication arrives.
    pub listen: SocketAddr,
    /// Longest a single statement may run before Floe cancels it.
    pub statement_timeout: Duration,
    /// Most client connections open at once.
    pub max_connections: usize,
    /// How long a shutdown waits for in-flight statements before cancelling them.
    pub shutdown_grace: Duration,
}
