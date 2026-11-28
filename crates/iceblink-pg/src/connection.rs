//! What pgwire runs for one connection: Iceblink's handlers for a served connection, and a
//! startup-only refusal for one Iceblink turns away (too many connections, shutting down, or no
//! session).

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion_postgres::pgwire::api::auth::{
    DefaultServerParameterProvider, StartupHandler, finish_authentication, protocol_negotiation,
    save_startup_parameters_to_metadata,
};
use datafusion_postgres::pgwire::api::cancel::{CancelHandler, DefaultCancelHandler};
use datafusion_postgres::pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use datafusion_postgres::pgwire::api::{
    ClientInfo, ConnectionManager, ErrorHandler, PgWireServerHandlers, PidSecretKeyGenerator,
    RandomPidSecretKeyGenerator,
};
use datafusion_postgres::pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use datafusion_postgres::pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use futures::Sink;

use crate::PgError;
use crate::errors::{PgErrorHandler, to_error_info};
use crate::handler::PgHandler;

/// A served connection's handlers.
pub(crate) struct PgHandlers {
    pub(crate) handler: Arc<PgHandler>,
    pub(crate) startup: Arc<PgStartup>,
    pub(crate) errors: Arc<PgErrorHandler>,
    pub(crate) cancel: Arc<DefaultCancelHandler>,
}

impl PgWireServerHandlers for PgHandlers {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        Arc::clone(&self.handler)
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        Arc::clone(&self.handler)
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        Arc::clone(&self.startup)
    }

    fn error_handler(&self) -> Arc<impl ErrorHandler> {
        Arc::clone(&self.errors)
    }

    fn cancel_handler(&self) -> Arc<impl CancelHandler> {
        Arc::clone(&self.cancel)
    }
}

/// Startup without authentication (v0.1 adds it): registers the connection for cancel
/// requests and tells the client what kind of server this is.
pub(crate) struct PgStartup {
    connections: Arc<ConnectionManager>,
    parameters: DefaultServerParameterProvider,
    keys: RandomPidSecretKeyGenerator,
}

impl PgStartup {
    /// `search_path` is the session's default schema, reported to the client.
    pub(crate) fn new(connections: Arc<ConnectionManager>, search_path: String) -> Self {
        let mut parameters = DefaultServerParameterProvider::default();
        // Clients read the leading version number; the rest says which server answered.
        parameters.server_version = format!("16.6 (Iceblink {})", env!("CARGO_PKG_VERSION"));
        parameters.client_encoding = Some("UTF8".to_owned());
        parameters.integer_datetimes = true;
        parameters.is_superuser = false;
        parameters.default_transaction_read_only = true;
        parameters.search_path = search_path;
        Self {
            connections,
            parameters,
            keys: RandomPidSecretKeyGenerator::default(),
        }
    }
}

#[async_trait]
impl StartupHandler for PgStartup {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if let PgWireFrontendMessage::Startup(startup) = &message {
            protocol_negotiation(client, startup).await?;
            save_startup_parameters_to_metadata(client, startup);
            // The schema this session really uses, whatever the client asked for at startup:
            // SHOW search_path reads it from here.
            client.metadata_mut().insert(
                "search_path".to_owned(),
                self.parameters.search_path.clone(),
            );
            let (pid, secret_key) = self.keys.generate(&*client);
            client.set_pid_and_secret_key(pid, secret_key.clone());
            let (handle, guard) = self.connections.register(pid, secret_key);
            client.session_extensions().insert(handle);
            client.session_extensions().insert(guard);
            finish_authentication(client, &self.parameters).await?;
        }
        Ok(())
    }
}

/// A connection Iceblink turns away: FATAL with the reason, right after the client's startup
/// message, as Postgres does. Cancel requests on such a connection still work.
pub(crate) struct Refusal {
    code: String,
    message: String,
    cancel: Arc<DefaultCancelHandler>,
}

impl Refusal {
    /// The refusal for `error`; an internal error is logged once, under `request_id`.
    pub(crate) fn new(
        error: &PgError,
        request_id: &str,
        cancel: Arc<DefaultCancelHandler>,
    ) -> Self {
        let info = to_error_info(error, request_id);
        Self {
            code: info.code,
            message: info.message,
            cancel,
        }
    }
}

/// The handlers for a refused connection.
pub(crate) struct RefusalHandlers(pub(crate) Arc<Refusal>);

impl PgWireServerHandlers for RefusalHandlers {
    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        Arc::clone(&self.0)
    }

    fn cancel_handler(&self) -> Arc<impl CancelHandler> {
        Arc::clone(&self.0.cancel)
    }
}

#[async_trait]
impl StartupHandler for Refusal {
    async fn on_startup<C>(
        &self,
        _client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if matches!(message, PgWireFrontendMessage::Startup(_)) {
            let info = ErrorInfo::new("FATAL".to_owned(), self.code.clone(), self.message.clone());
            return Err(PgWireError::UserError(Box::new(info)));
        }
        Ok(())
    }
}
