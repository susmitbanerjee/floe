//! `FloeHook`: the allow-list and Floe's own session settings, plugged into datafusion-postgres
//! as a query hook so they see every statement of both protocols.
//!
//! In the simple protocol the hook also runs allowed queries itself: it plans them, checks the
//! plan with DataFusion's read-only options (the second layer), and hands back tracked rows. In
//! the extended protocol the library plans and runs them, because only it knows the portal's
//! result formats, and `FloeHandler` checks and tracks them.

use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::common::{DFSchema, ParamValues};
use datafusion::execution::context::SQLOptions;
use datafusion::logical_expr::{EmptyRelation, LogicalPlan};
use datafusion::prelude::SessionContext;
use datafusion::sql::parser::Statement as DFStatement;
use datafusion::sql::sqlparser::ast::Statement;
use datafusion_postgres::QueryHook;
use datafusion_postgres::arrow_pg::datatypes::df::encode_dataframe;
use datafusion_postgres::hooks::HookClient;
use datafusion_postgres::pgwire::api::ClientInfo;
use datafusion_postgres::pgwire::api::Type;
use datafusion_postgres::pgwire::api::portal::Format;
use datafusion_postgres::pgwire::api::results::{
    DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag,
};
use datafusion_postgres::pgwire::error::{PgWireError, PgWireResult};
use datafusion_postgres::pgwire::messages::PgWireBackendMessage;
use datafusion_postgres::pgwire::messages::startup::ParameterStatus;
use datafusion_postgres::pgwire::types::format::FormatOptions;
use futures::stream;

use crate::PgError;
use crate::allow::{Allowed, classify};
use crate::errors::to_pgwire;
use crate::setting::{encoding, search_path, timeout, timeout_text};
use crate::statement::Statements;

/// DataFusion's own read-only check: no DDL, no DML, no statements such as SET. A plan that
/// got past the allow-list still has to pass this.
pub(crate) fn read_only() -> SQLOptions {
    SQLOptions::new()
        .with_allow_ddl(false)
        .with_allow_dml(false)
        .with_allow_statements(false)
}

/// The allow-list and Floe's session settings for one connection.
pub(crate) struct FloeHook {
    statements: Arc<Statements>,
}

impl FloeHook {
    pub(crate) fn new(statements: Arc<Statements>) -> Self {
        Self { statements }
    }

    /// Plans, checks and starts one query of the simple protocol.
    async fn query(
        &self,
        statement: &Statement,
        session: &SessionContext,
        client: &dyn HookClient,
    ) -> PgWireResult<Response> {
        let run = self.statements.start(statement);
        let format = Arc::new(FormatOptions::from_client_metadata(client.metadata()));
        match run.within(plan_and_start(statement, session, format)).await {
            Ok(Ok(rows)) => Ok(run.track(Response::Query(rows))),
            Ok(Err(error)) => Err(run.fail(error.as_ref())),
            Err(timeout) => Err(run.fail(&timeout)),
        }
    }

    /// Applies one of Floe's own settings, or answers SHOW statement_timeout.
    async fn apply(
        &self,
        allowed: Allowed<'_>,
        session: &SessionContext,
        client: &mut dyn HookClient,
    ) -> PgWireResult<Response> {
        let user_error = |error: PgError| to_pgwire(&error, "");
        match allowed {
            Allowed::SetStatementTimeout(values) => {
                let requested = timeout(values, self.statements.limit()).map_err(user_error)?;
                self.statements.set_timeout(requested);
                Ok(Response::Execution(Tag::new("SET")))
            }
            Allowed::SetSearchPath(values) => {
                let schema = search_path(values, session).map_err(user_error)?;
                // SHOW search_path, answered by the library, reads it from here.
                client
                    .metadata_mut()
                    .insert("search_path".to_owned(), schema.clone());
                report(client, "search_path", schema).await
            }
            Allowed::SetClientEncoding(values) => {
                encoding(values).map_err(user_error)?;
                report(client, "client_encoding", "UTF8".to_owned()).await
            }
            Allowed::ShowStatementTimeout => show(
                "statement_timeout",
                &timeout_text(self.statements.timeout()),
            ),
            Allowed::Query | Allowed::Library => Ok(Response::EmptyQuery),
        }
    }
}

#[async_trait]
impl QueryHook for FloeHook {
    async fn handle_simple_query(
        &self,
        statement: &Statement,
        session: &SessionContext,
        client: &mut dyn HookClient,
    ) -> Option<PgWireResult<Response>> {
        let allowed = match classify(statement) {
            Ok(allowed) => allowed,
            Err(refused) => return Some(Err(self.statements.start(statement).fail(&refused))),
        };
        match allowed {
            Allowed::Library => None,
            Allowed::Query => Some(self.query(statement, session, client).await),
            setting => {
                let run = self.statements.start(statement);
                Some(match self.apply(setting, session, client).await {
                    Ok(response) => Ok(run.track(response)),
                    Err(error) => Err(run.fail_pgwire(error)),
                })
            }
        }
    }

    async fn handle_extended_parse_query(
        &self,
        statement: &Statement,
        _session: &SessionContext,
        _client: &(dyn ClientInfo + Send + Sync),
    ) -> Option<PgWireResult<LogicalPlan>> {
        match classify(statement) {
            Err(refused) => Some(Err(self.statements.start(statement).fail(&refused))),
            Ok(Allowed::Query | Allowed::Library) => None,
            Ok(Allowed::ShowStatementTimeout) => Some(plan_for(vec![Field::new(
                "statement_timeout",
                DataType::Utf8,
                false,
            )])),
            Ok(_) => Some(plan_for(Vec::new())),
        }
    }

    async fn handle_extended_query(
        &self,
        statement: &Statement,
        _plan: &LogicalPlan,
        _params: &ParamValues,
        session: &SessionContext,
        client: &mut dyn HookClient,
    ) -> Option<PgWireResult<Response>> {
        match classify(statement) {
            Ok(Allowed::Query | Allowed::Library) | Err(_) => None,
            Ok(setting) => Some(self.apply(setting, session, client).await),
        }
    }
}

/// Plans `statement`, checks the plan is read-only, and starts it.
async fn plan_and_start(
    statement: &Statement,
    session: &SessionContext,
    format: Arc<FormatOptions>,
) -> Result<QueryResponse, Box<dyn Error + Send + Sync>> {
    let plan = session
        .state()
        .statement_to_plan(DFStatement::Statement(Box::new(statement.clone())))
        .await?;
    read_only().verify_plan(&plan)?;
    let frame = session.execute_logical_plan(plan).await?;
    Ok(encode_dataframe(frame, &Format::UnifiedText, Some(format)).await?)
}

/// Tells the client a setting's new value, as Postgres does, and acknowledges the SET.
async fn report(client: &mut dyn HookClient, name: &str, value: String) -> PgWireResult<Response> {
    let status = ParameterStatus::new(name.to_owned(), value);
    client
        .send_message(PgWireBackendMessage::ParameterStatus(status))
        .await?;
    Ok(Response::Execution(Tag::new("SET")))
}

/// A one-row, one-column SHOW result.
fn show(name: &str, value: &str) -> PgWireResult<Response> {
    let fields = Arc::new(vec![FieldInfo::new(
        name.to_owned(),
        None,
        None,
        Type::TEXT,
        FieldFormat::Text,
    )]);
    let mut encoder = DataRowEncoder::new(Arc::clone(&fields));
    encoder.encode_field(&Some(value))?;
    let row = encoder.take_row();
    Ok(Response::Query(QueryResponse::new(
        fields,
        stream::once(async move { Ok(row) }),
    )))
}

/// The plan the extended protocol stores for a statement Floe answers itself: nothing to run,
/// only the result's columns.
fn plan_for(fields: Vec<Field>) -> PgWireResult<LogicalPlan> {
    let schema = DFSchema::try_from(Schema::new(fields))
        .map_err(|error| PgWireError::ApiError(Box::new(error)))?;
    Ok(LogicalPlan::EmptyRelation(EmptyRelation {
        produce_one_row: true,
        schema: Arc::new(schema),
    }))
}
