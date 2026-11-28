//! The one place errors become SQLSTATE codes (docs/adr/0008-postgres-server.md).
//!
//! User errors keep their message. Internal failures (execution, storage, Iceberg, anything
//! unrecognised) reach the client as a short request ID, because their text can name files and
//! hosts; the detail goes only to the log, under the same ID.

use std::error::Error;
use std::sync::Arc;

use datafusion::common::SchemaError;
use datafusion::error::DataFusionError;
use datafusion::sql::sqlparser::parser::ParserError;
use datafusion_postgres::pgwire::api::{ClientInfo, ErrorHandler};
use datafusion_postgres::pgwire::error::{ErrorInfo, PgWireError};

use crate::PgError;
use crate::statement::QueryIds;

/// SQLSTATE codes Floe sends, by name.
pub(crate) mod sqlstate {
    pub(crate) const SUCCESS: &str = "00000";
    pub(crate) const INVALID_PARAMETER_VALUE: &str = "22023";
    pub(crate) const READ_ONLY_TRANSACTION: &str = "25006";
    pub(crate) const INVALID_SCHEMA_NAME: &str = "3F000";
    pub(crate) const SYNTAX_OR_ACCESS_RULE: &str = "42000";
    pub(crate) const SYNTAX_ERROR: &str = "42601";
    pub(crate) const UNDEFINED_COLUMN: &str = "42703";
    pub(crate) const UNDEFINED_TABLE: &str = "42P01";
    pub(crate) const TOO_MANY_CONNECTIONS: &str = "53300";
    pub(crate) const QUERY_CANCELED: &str = "57014";
    pub(crate) const ADMIN_SHUTDOWN: &str = "57P01";
    pub(crate) const CANNOT_CONNECT_NOW: &str = "57P03";
    pub(crate) const FEATURE_NOT_SUPPORTED: &str = "0A000";
    pub(crate) const INTERNAL_ERROR: &str = "XX000";
}

/// The error a client sees for `error`. An internal error is logged here, at error level, with
/// `request_id`, and the client gets only the ID.
pub(crate) fn to_error_info(error: &(dyn Error + 'static), request_id: &str) -> ErrorInfo {
    let (code, message) = classify(error);
    if code == sqlstate::INTERNAL_ERROR {
        tracing::error!(request_id, error = %chain(error), "internal error");
        let message =
            format!("internal error (request ID {request_id}); the server log has the details");
        return ErrorInfo::new("ERROR".to_owned(), code.to_owned(), message);
    }
    let severity = match code {
        sqlstate::TOO_MANY_CONNECTIONS
        | sqlstate::ADMIN_SHUTDOWN
        | sqlstate::CANNOT_CONNECT_NOW => "FATAL",
        _ => "ERROR",
    };
    ErrorInfo::new(severity.to_owned(), code.to_owned(), message)
}

/// [`to_error_info`] wrapped for pgwire.
pub(crate) fn to_pgwire(error: &(dyn Error + 'static), request_id: &str) -> PgWireError {
    PgWireError::UserError(Box::new(to_error_info(error, request_id)))
}

/// The SQLSTATE and client message for `error`; the message is empty for internal errors.
fn classify(error: &(dyn Error + 'static)) -> (&'static str, String) {
    if let Some(error) = error.downcast_ref::<PgError>() {
        return (floe_code(error), error.to_string());
    }
    if let Some(error) = error.downcast_ref::<ParserError>() {
        return (sqlstate::SYNTAX_ERROR, error.to_string());
    }
    if let Some(error) = error.downcast_ref::<DataFusionError>() {
        return datafusion_code(error.find_root());
    }
    if let Some(PgWireError::ApiError(inner)) = error.downcast_ref::<PgWireError>() {
        return classify(inner.as_ref());
    }
    (sqlstate::INTERNAL_ERROR, String::new())
}

fn floe_code(error: &PgError) -> &'static str {
    match error {
        PgError::ReadOnly { .. } => sqlstate::READ_ONLY_TRANSACTION,
        PgError::UnsupportedSetting { .. } | PgError::UnsupportedEncoding { .. } => {
            sqlstate::FEATURE_NOT_SUPPORTED
        }
        PgError::InvalidTimeout { .. } | PgError::TimeoutAboveLimit { .. } => {
            sqlstate::INVALID_PARAMETER_VALUE
        }
        PgError::UnknownSchema { .. } => sqlstate::INVALID_SCHEMA_NAME,
        PgError::StatementTimeout => sqlstate::QUERY_CANCELED,
        PgError::TooManyConnections { .. } => sqlstate::TOO_MANY_CONNECTIONS,
        PgError::ShuttingDown => sqlstate::ADMIN_SHUTDOWN,
        PgError::NotAcceptingConnections => sqlstate::CANNOT_CONNECT_NOW,
        PgError::Bind { .. } | PgError::Session(_) | PgError::SessionSetup { .. } => {
            sqlstate::INTERNAL_ERROR
        }
    }
}

fn datafusion_code(error: &DataFusionError) -> (&'static str, String) {
    match error {
        DataFusionError::SQL(parser, _) => (sqlstate::SYNTAX_ERROR, parser.to_string()),
        DataFusionError::Plan(message) => plan_code(message),
        DataFusionError::SchemaError(schema, _) => match schema.as_ref() {
            SchemaError::FieldNotFound { .. } => (sqlstate::UNDEFINED_COLUMN, schema.to_string()),
            _ => (sqlstate::SYNTAX_OR_ACCESS_RULE, schema.to_string()),
        },
        DataFusionError::NotImplemented(message) => {
            (sqlstate::FEATURE_NOT_SUPPORTED, message.clone())
        }
        DataFusionError::External(inner) => classify(inner.as_ref()),
        _ => (sqlstate::INTERNAL_ERROR, String::new()),
    }
}

/// DataFusion reports missing tables and schemas only as planning messages
/// (`session_state.rs` in 54.2.0); the unit tests pin both forms.
fn plan_code(message: &str) -> (&'static str, String) {
    if let Some(name) = message
        .strip_prefix("table '")
        .and_then(|rest| rest.strip_suffix("' not found"))
    {
        let message = format!("relation \"{name}\" does not exist");
        return (sqlstate::UNDEFINED_TABLE, message);
    }
    if let Some(name) = message.strip_prefix("failed to resolve schema: ") {
        let message = format!("schema \"{name}\" does not exist");
        return (sqlstate::INVALID_SCHEMA_NAME, message);
    }
    (sqlstate::SYNTAX_OR_ACCESS_RULE, message.to_owned())
}

/// `error` and every source under it that adds something, for the log.
fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        let line = next.to_string();
        if !text.contains(&line) {
            text.push_str(": ");
            text.push_str(&line);
        }
        source = next.source();
    }
    text
}

/// The safety net: whatever error reaches pgwire still unmapped gets its SQLSTATE here, so no
/// raw error text (which can name files) reaches a client.
pub(crate) struct FloeErrorHandler {
    ids: Arc<QueryIds>,
}

impl FloeErrorHandler {
    pub(crate) fn new(ids: Arc<QueryIds>) -> Self {
        Self { ids }
    }
}

impl ErrorHandler for FloeErrorHandler {
    fn on_error<C>(&self, _client: &C, error: &mut PgWireError)
    where
        C: ClientInfo,
    {
        if let PgWireError::ApiError(inner) = error {
            let request_id = self.ids.next().to_string();
            let info = to_error_info(inner.as_ref(), &request_id);
            *error = PgWireError::UserError(Box::new(info));
        }
    }
}

#[cfg(test)]
mod tests;
