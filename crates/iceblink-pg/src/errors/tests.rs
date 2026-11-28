//! Every SQLSTATE row of docs/adr/0008-postgres-server.md, and internal errors never leaking
//! their text.

use datafusion::prelude::SessionContext;
use floe_scan::ScanError;

use super::*;

fn code_and_message(error: &(dyn Error + 'static)) -> (String, String) {
    let info = to_error_info(error, "q-00002a");
    (info.code, info.message)
}

#[test]
fn maps_each_error_to_its_sqlstate() {
    let parser = ParserError::ParserError("Expected: an expression, found: EOF".to_owned());
    let cases: Vec<(Box<dyn Error>, &str, &str)> = vec![
        (Box::new(parser.clone()), "42601", "Expected: an expression"),
        (
            Box::new(DataFusionError::SQL(Box::new(parser), None)),
            "42601",
            "Expected: an expression",
        ),
        (
            Box::new(DataFusionError::Plan(
                "table 'lake.sales.missing' not found".to_owned(),
            )),
            "42P01",
            "relation \"lake.sales.missing\" does not exist",
        ),
        (
            Box::new(DataFusionError::Context(
                "while planning".to_owned(),
                Box::new(DataFusionError::Plan(
                    "failed to resolve schema: nope".to_owned(),
                )),
            )),
            "3F000",
            "schema \"nope\" does not exist",
        ),
        (
            Box::new(DataFusionError::NotImplemented("LATERAL".to_owned())),
            "0A000",
            "LATERAL",
        ),
        (
            Box::new(PgError::ReadOnly {
                kind: "CREATE TABLE".to_owned(),
            }),
            "25006",
            "Floe is read-only: CREATE TABLE is not allowed.",
        ),
        (
            Box::new(PgError::UnsupportedSetting {
                name: "work_mem".to_owned(),
            }),
            "0A000",
            "work_mem",
        ),
        (
            Box::new(PgError::TimeoutAboveLimit {
                limit: "30s".to_owned(),
            }),
            "22023",
            "can only be lowered",
        ),
        (
            Box::new(PgError::StatementTimeout),
            "57014",
            "statement timeout",
        ),
        (
            Box::new(PgError::TooManyConnections { limit: 2 }),
            "53300",
            "too many connections (limit 2)",
        ),
        (Box::new(PgError::ShuttingDown), "57P01", "shutting down"),
        (
            Box::new(PgError::NotAcceptingConnections),
            "57P03",
            "shutting down",
        ),
        (
            Box::new(DataFusionError::Plan("Unsupported join".to_owned())),
            "42000",
            "Unsupported join",
        ),
    ];
    for (error, code, message) in cases {
        let (actual_code, actual_message) = code_and_message(error.as_ref());
        assert_eq!(actual_code, code, "{error}");
        assert!(actual_message.contains(message), "{actual_message}");
    }
}

#[tokio::test]
async fn an_unknown_column_is_42703() {
    let error = SessionContext::new()
        .sql("SELECT nope")
        .await
        .err()
        .map(|error| code_and_message(&error).0);
    assert_eq!(error.as_deref(), Some("42703"));
}

#[test]
fn internal_errors_show_only_a_request_id() {
    let path = "/var/floe/warehouse/sales/orders/data/part-0.parquet";
    let errors: Vec<Box<dyn Error>> = vec![
        Box::new(DataFusionError::Execution(format!("Failed to open {path}"))),
        Box::new(DataFusionError::External(Box::new(ScanError::Io {
            message: format!("couldn't read {path}"),
        }))),
        Box::new(PgWireError::ApiError(Box::new(DataFusionError::Internal(
            format!("bad state reading {path}"),
        )))),
        Box::new(std::io::Error::other(path)),
    ];
    for error in errors {
        let info = to_error_info(error.as_ref(), "q-00002a");
        assert_eq!(info.code, "XX000");
        assert_eq!(info.severity, "ERROR");
        assert!(
            info.message.contains("request ID q-00002a"),
            "{}",
            info.message
        );
        assert!(!info.message.contains('/'), "{}", info.message);
    }
}

#[test]
fn connection_refusals_are_fatal() {
    for error in [
        PgError::TooManyConnections { limit: 1 },
        PgError::ShuttingDown,
        PgError::NotAcceptingConnections,
    ] {
        assert_eq!(to_error_info(&error, "q-1").severity, "FATAL");
    }
}
