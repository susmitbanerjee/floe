//! Graceful shutdown: count the connections that are busy with a statement, refuse new work
//! once shutdown starts, and wake the server when the last busy one is done.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// The server's busy count, tied to its shutdown token.
#[derive(Debug)]
pub(crate) struct Drain {
    busy: AtomicUsize,
    idle: Notify,
    shutdown: CancellationToken,
}

impl Drain {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        Self {
            busy: AtomicUsize::new(0),
            idle: Notify::new(),
            shutdown,
        }
    }

    /// Connections busy with a statement right now.
    pub(crate) fn busy(&self) -> usize {
        self.busy.load(Ordering::SeqCst)
    }

    /// True once shutdown has started.
    pub(crate) fn is_draining(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    /// Marks a connection busy until the guard drops, or `None` once shutdown has started.
    pub(crate) fn enter(self: &Arc<Self>) -> Option<Busy> {
        // Count first, then check: a drain that starts in between still waits for this one.
        self.busy.fetch_add(1, Ordering::SeqCst);
        let busy = Busy(Arc::clone(self));
        (!self.is_draining()).then_some(busy)
    }

    /// Waits until no connection is busy.
    pub(crate) async fn idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.busy.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// A connection busy with a statement, from its first message to the reply that ends it.
#[derive(Debug)]
pub(crate) struct Busy(Arc<Drain>);

impl Drop for Busy {
    fn drop(&mut self) {
        if self.0.busy.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_busy_connections_and_refuses_new_ones() {
        let shutdown = CancellationToken::new();
        let drain = Arc::new(Drain::new(shutdown.clone()));
        let busy = drain.enter();
        assert!(busy.is_some());

        shutdown.cancel();
        assert!(drain.enter().is_none(), "work started after shutdown");
        let waiting = tokio::spawn({
            let drain = Arc::clone(&drain);
            async move { drain.idle().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "the drain ignored a busy connection"
        );

        drop(busy);
        assert!(waiting.await.is_ok());
    }
}
