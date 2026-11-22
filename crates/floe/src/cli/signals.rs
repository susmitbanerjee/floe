//! Shutdown signals: SIGINT (Ctrl-C) and, on Unix, SIGTERM (systemd, Docker, Kubernetes)
//! cancel the shutdown token. The server then drains in-flight queries.

use std::io;

use tokio_util::sync::CancellationToken;

/// The installed signal handlers.
pub(crate) struct Signals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Signals {
    /// Installs the handlers now, so a failure shows before the server starts.
    ///
    /// # Errors
    ///
    /// If the operating system refuses the SIGTERM handler.
    pub(crate) fn install() -> io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    /// Cancels `shutdown` on the first signal. Returns early, without cancelling, once
    /// `shutdown` is cancelled some other way.
    pub(crate) async fn cancel_on_signal(mut self, shutdown: CancellationToken) {
        let first = tokio::select! {
            () = shutdown.cancelled() => return,
            interrupted = tokio::signal::ctrl_c() => match interrupted {
                Ok(()) => Some("SIGINT"),
                Err(error) => {
                    tracing::warn!(%error, "couldn't listen for Ctrl-C; use SIGTERM to stop Floe");
                    None
                }
            },
            () = self.terminated() => Some("SIGTERM"),
        };
        let signal = match first {
            Some(signal) => signal,
            None => tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.terminated() => "SIGTERM",
            },
        };
        tracing::info!(signal, "shutting down: finishing in-flight queries");
        shutdown.cancel();
    }

    #[cfg(unix)]
    async fn terminated(&mut self) {
        self.terminate.recv().await;
    }

    #[cfg(not(unix))]
    async fn terminated(&mut self) {
        std::future::pending::<()>().await;
    }
}
