//! `FloeHandler`: datafusion-postgres's `DfSessionService`, wrapped.
//!
//! The wrapper adds what a hook can't: holding a connection busy until its reply is flushed
//! (for graceful shutdown), the deadline and tracking of extended-protocol rows, the read-only
//! plan check before the library runs a portal, and "read-only" rather than "syntax error" for
//! changing statements the parser rejects.

use std::fmt::Debug;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use datafusion::sql::sqlparser::parser::ParserError;
use datafusion_postgres::pgwire::api::portal::{Format, Portal};
use datafusion_postgres::pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use datafusion_postgres::pgwire::api::results::{FieldInfo, Response};
use datafusion_postgres::pgwire::api::stmt::QueryParser;
use datafusion_postgres::pgwire::api::store::PortalStore;
use datafusion_postgres::pgwire::api::{ClientInfo, ClientPortalStore, DEFAULT_NAME, Type};
use datafusion_postgres::pgwire::error::{PgWireError, PgWireResult};
use datafusion_postgres::pgwire::messages::PgWireBackendMessage;
use datafusion_postgres::pgwire::messages::extendedquery::{Execute, Sync as SyncMessage};
use datafusion_postgres::pgwire::messages::response::ReadyForQuery;
use datafusion_postgres::pgwire::messages::simplequery::Query;
use datafusion_postgres::{DfSessionService, Parser};
use futures::{Sink, SinkExt};

use crate::PgError;
use crate::allow::refusal_for_unparsed;
use crate::drain::{Busy, Drain};
use crate::errors::to_pgwire;
use crate::hook::read_only;
use crate::statement::Statements;

/// The statement type the library's extended protocol stores: the SQL, and once parsed, the
/// statement and its plan.
type Stored = <DfSessionService as ExtendedQueryHandler>::Statement;

/// One connection's query handler.
pub(crate) struct FloeHandler {
    inner: DfSessionService,
    parser: Arc<FloeParser>,
    statements: Arc<Statements>,
    drain: Arc<Drain>,
    /// Held from an extended Execute until the Sync that ends it has been answered.
    busy: Mutex<Option<Busy>>,
}

impl FloeHandler {
    pub(crate) fn new(
        inner: DfSessionService,
        statements: Arc<Statements>,
        drain: Arc<Drain>,
    ) -> Self {
        let parser = Arc::new(FloeParser {
            inner: inner.query_parser(),
        });
        Self {
            inner,
            parser,
            statements,
            drain,
            busy: Mutex::new(None),
        }
    }

    /// Marks the connection busy, or refuses with FATAL 57P01 once shutdown has started.
    fn enter(&self) -> PgWireResult<Busy> {
        self.drain
            .enter()
            .ok_or_else(|| to_pgwire(&PgError::ShuttingDown, ""))
    }

    fn hold(&self, busy: Option<Busy>) {
        *self.busy.lock().unwrap_or_else(PoisonError::into_inner) = busy;
    }
}

/// For a parse error on SQL that starts with a changing verb, the read-only refusal instead.
/// Other errors go on to `FloeErrorHandler`.
fn unparsed(error: PgWireError, sql: &str) -> PgWireError {
    if let PgWireError::ApiError(inner) = &error
        && inner.downcast_ref::<ParserError>().is_some()
        && let Some(refusal) = refusal_for_unparsed(sql)
    {
        return to_pgwire(&refusal, "");
    }
    error
}

#[async_trait]
impl SimpleQueryHandler for FloeHandler {
    async fn on_query<C>(&self, client: &mut C, query: Query) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        // Busy until ReadyForQuery is sent, so a drain never cuts a reply short.
        let _busy = self.enter()?;
        self._on_query(client, query).await
    }

    async fn do_query<C>(&self, client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        SimpleQueryHandler::do_query(&self.inner, client, query)
            .await
            .map_err(|error| unparsed(error, query))
    }
}

#[async_trait]
impl ExtendedQueryHandler for FloeHandler {
    type Statement = Stored;
    type QueryParser = FloeParser;

    fn query_parser(&self) -> Arc<FloeParser> {
        Arc::clone(&self.parser)
    }

    async fn on_execute<C>(&self, client: &mut C, message: Execute) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        self.hold(Some(self.enter()?));
        self._on_execute(client, message).await
    }

    /// pgwire's own Sync handling, then the connection is no longer busy.
    async fn on_sync<C>(&self, client: &mut C, _message: SyncMessage) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        client.portal_store().rm_portal(DEFAULT_NAME);
        let ready = ReadyForQuery::new(client.transaction_status());
        client
            .send(PgWireBackendMessage::ReadyForQuery(ready))
            .await?;
        client.flush().await?;
        self.hold(None);
        Ok(())
    }

    async fn do_query<C>(
        &self,
        client: &mut C,
        portal: &Portal<Self::Statement>,
        max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let Some((statement, plan)) = &portal.statement.statement.1 else {
            return ExtendedQueryHandler::do_query(&self.inner, client, portal, max_rows).await;
        };
        let run = self.statements.start(statement);
        if let Err(error) = read_only().verify_plan(plan) {
            return Err(run.fail(&error));
        }
        let running = ExtendedQueryHandler::do_query(&self.inner, client, portal, max_rows);
        match run.within(running).await {
            Ok(Ok(response)) => Ok(run.track(response)),
            Ok(Err(error)) => Err(run.fail_pgwire(error)),
            Err(timeout) => Err(run.fail(&timeout)),
        }
    }
}

/// The library's extended-protocol parser, with the same read-only refusal for changing
/// statements it can't parse.
pub(crate) struct FloeParser {
    inner: Arc<Parser>,
}

#[async_trait]
impl QueryParser for FloeParser {
    type Statement = Stored;

    async fn parse_sql<C>(
        &self,
        client: &C,
        sql: &str,
        types: &[Option<Type>],
    ) -> PgWireResult<Self::Statement>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        self.inner
            .parse_sql(client, sql, types)
            .await
            .map_err(|error| unparsed(error, sql))
    }

    fn get_parameter_types(&self, statement: &Self::Statement) -> PgWireResult<Vec<Type>> {
        self.inner.get_parameter_types(statement)
    }

    fn get_result_schema(
        &self,
        statement: &Self::Statement,
        column_format: Option<&Format>,
    ) -> PgWireResult<Vec<FieldInfo>> {
        self.inner.get_result_schema(statement, column_format)
    }
}
