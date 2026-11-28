//! The Postgres listener: accept connections within the limit, give each its own session, and
//! on shutdown drain in-flight statements before closing (docs/adr/0008-postgres-server.md).

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use datafusion::prelude::SessionContext;
use datafusion_postgres::datafusion_pg_catalog::pg_catalog::context::EmptyContextProvider;
use datafusion_postgres::datafusion_pg_catalog::setup_pg_catalog;
use datafusion_postgres::hooks::set_show::SetShowHook;
use datafusion_postgres::hooks::transactions::TransactionStatementHook;
use datafusion_postgres::pgwire::api::ConnectionManager;
use datafusion_postgres::pgwire::api::cancel::DefaultCancelHandler;
use datafusion_postgres::pgwire::tokio::process_socket;
use datafusion_postgres::{DfSessionService, QueryHook};
use iceblink_scan::{CATALOG_NAME, QueryReceipt, ScanError};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::connection::{PgHandlers, PgStartup, Refusal, RefusalHandlers};
use crate::drain::Drain;
use crate::errors::PgErrorHandler;
use crate::handler::PgHandler;
use crate::hook::PgHook;
use crate::statement::{QueryIds, Statements};
use crate::{PgError, ServerSettings};

/// How long to wait before accepting again after `accept` fails, such as when the process is
/// out of file descriptors.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Serves Iceblink over the Postgres protocol on `settings.listen` until `shutdown` is cancelled,
/// then waits up to `settings.shutdown_grace` for in-flight statements, cancels the rest, and
/// says how many of each.
///
/// Each connection gets its own session from `session_factory`, so session settings such as
/// `search_path` stay with the connection.
///
/// # Errors
///
/// [`PgError::Bind`] when the address can't be bound. Errors on one connection are sent to
/// that client and logged; they never stop the server.
pub async fn serve<F>(
    session_factory: F,
    settings: ServerSettings,
    shutdown: CancellationToken,
) -> Result<ShutdownSummary, PgError>
where
    F: Fn() -> Result<SessionContext, ScanError> + Send + Sync + 'static,
{
    let listener = TcpListener::bind(settings.listen)
        .await
        .map_err(|source| PgError::Bind {
            listen: settings.listen,
            source,
        })?;
    serve_on(listener, session_factory, settings, shutdown).await
}

/// [`serve`] on a listener the caller has already bound, such as `127.0.0.1:0` in tests.
/// `settings.listen` is not used.
///
/// # Errors
///
/// None today; the `Result` matches [`serve`].
pub async fn serve_on<F>(
    listener: TcpListener,
    session_factory: F,
    settings: ServerSettings,
    shutdown: CancellationToken,
) -> Result<ShutdownSummary, PgError>
where
    F: Fn() -> Result<SessionContext, ScanError> + Send + Sync + 'static,
{
    let server = Arc::new(Server::new(session_factory, settings, shutdown.clone()));
    let mut connections = JoinSet::new();
    if let Ok(address) = listener.local_addr() {
        tracing::info!(%address, "accepting Postgres connections");
    }
    accept_until(&listener, &server, &mut connections, shutdown.cancelled()).await;

    let in_flight = server.drain.busy();
    tracing::info!(in_flight, "shutting down: waiting for in-flight statements");
    let grace = settings.shutdown_grace;
    let drained = async {
        if tokio::time::timeout(grace, server.drain.idle())
            .await
            .is_err()
        {
            tracing::warn!(?grace, "statements still running after the shutdown grace");
        }
    };
    // Keep accepting while draining, to refuse newcomers with a reason rather than a reset.
    accept_until(&listener, &server, &mut connections, drained).await;
    drop(listener);
    // Statements still running now are cancelled: aborting their connections drops their
    // streams, which stops DataFusion's work.
    let cancelled = server.drain.busy();
    connections.shutdown().await;
    let summary = ShutdownSummary {
        drained: in_flight.saturating_sub(cancelled),
        cancelled,
    };
    tracing::info!("Postgres listener stopped");
    Ok(summary)
}

/// What a shutdown did with the statements in flight when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownSummary {
    /// Statements that finished within the shutdown grace.
    pub drained: usize,
    /// Statements still running when the grace ran out, and cancelled.
    pub cancelled: usize,
}

impl fmt::Display for ShutdownSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plural = if self.drained == 1 {
            "query"
        } else {
            "queries"
        };
        write!(
            formatter,
            "{} {plural} drained, {} cancelled",
            self.drained, self.cancelled
        )
    }
}

/// Accepts connections until `stop` completes, reaping finished ones as it goes.
async fn accept_until<F>(
    listener: &TcpListener,
    server: &Arc<Server<F>>,
    connections: &mut JoinSet<()>,
    stop: impl Future<Output = ()>,
) where
    F: Fn() -> Result<SessionContext, ScanError> + Send + Sync + 'static,
{
    tokio::pin!(stop);
    loop {
        tokio::select! {
            biased;
            () = &mut stop => return,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) => {
                    connections.spawn(Arc::clone(server).connection(socket));
                }
                Err(error) => {
                    tracing::warn!(%error, "couldn't accept a connection");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
        }
    }
}

/// What every connection shares.
struct Server<F> {
    session_factory: F,
    settings: ServerSettings,
    ids: Arc<QueryIds>,
    drain: Arc<Drain>,
    permits: Arc<Semaphore>,
    connections: Arc<ConnectionManager>,
    cancel: Arc<DefaultCancelHandler>,
    errors: Arc<PgErrorHandler>,
}

impl<F> Server<F>
where
    F: Fn() -> Result<SessionContext, ScanError> + Send + Sync + 'static,
{
    fn new(session_factory: F, settings: ServerSettings, shutdown: CancellationToken) -> Self {
        let ids = Arc::new(QueryIds::default());
        let connections = Arc::new(ConnectionManager::new());
        Self {
            session_factory,
            settings,
            errors: Arc::new(PgErrorHandler::new(Arc::clone(&ids))),
            ids,
            drain: Arc::new(Drain::new(shutdown)),
            permits: Arc::new(Semaphore::new(settings.max_connections)),
            cancel: Arc::new(DefaultCancelHandler::new(Arc::clone(&connections))),
            connections,
        }
    }

    /// Serves one connection, or turns it away with the reason.
    async fn connection(self: Arc<Self>, socket: TcpStream) {
        let outcome = if self.drain.is_draining() {
            process_socket(
                socket,
                None,
                self.refusal(&PgError::NotAcceptingConnections),
            )
            .await
        } else if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            let served = match self.setup().await {
                Ok(handlers) => process_socket(socket, None, handlers).await,
                Err(error) => process_socket(socket, None, self.refusal(&error)).await,
            };
            drop(permit);
            served
        } else {
            let limit = self.settings.max_connections;
            let refusal = self.refusal(&PgError::TooManyConnections { limit });
            process_socket(socket, None, refusal).await
        };
        if let Err(error) = outcome {
            tracing::debug!(%error, "connection ended with an I/O error");
        }
    }

    fn refusal(&self, error: &PgError) -> RefusalHandlers {
        let request_id = self.ids.next().to_string();
        let refusal = Refusal::new(error, &request_id, Arc::clone(&self.cancel));
        RefusalHandlers(Arc::new(refusal))
    }

    /// [`handlers`](Self::handlers) on the blocking pool: setting up pg_catalog parses its
    /// embedded tables, about 250 ms of CPU per connection in a debug build (ADR 0008).
    async fn setup(self: &Arc<Self>) -> Result<PgHandlers, PgError> {
        let server = Arc::clone(self);
        tokio::task::spawn_blocking(move || server.handlers())
            .await
            .map_err(|error| PgError::SessionSetup {
                message: format!("the setup task stopped: {error}"),
            })?
    }

    /// A new session from the factory, with pg_catalog and a receipt, behind Iceblink's handlers.
    fn handlers(&self) -> Result<PgHandlers, PgError> {
        let session = (self.session_factory)()?;
        setup_pg_catalog(&session, CATALOG_NAME, EmptyContextProvider).map_err(|error| {
            PgError::SessionSetup {
                message: format!("pg_catalog: {error}"),
            }
        })?;
        let receipt = Arc::new(QueryReceipt::default());
        let state = session.state_ref();
        let search_path = {
            let mut state = state.write();
            state.config_mut().set_extension(Arc::clone(&receipt));
            state.config_options().catalog.default_schema.clone()
        };
        let statements = Arc::new(Statements::new(
            Arc::clone(&self.ids),
            receipt,
            self.settings.statement_timeout,
        ));
        let hooks: Vec<Arc<dyn QueryHook>> = vec![
            Arc::new(TransactionStatementHook),
            Arc::new(PgHook::new(Arc::clone(&statements))),
            Arc::new(SetShowHook),
        ];
        let service = DfSessionService::new_with_hooks(Arc::new(session), hooks);
        Ok(PgHandlers {
            handler: Arc::new(PgHandler::new(service, statements, Arc::clone(&self.drain))),
            startup: Arc::new(PgStartup::new(Arc::clone(&self.connections), search_path)),
            errors: Arc::clone(&self.errors),
            cancel: Arc::clone(&self.cancel),
        })
    }
}
