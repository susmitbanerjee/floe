//! One row per allowed and per refused statement kind.

use datafusion::sql::sqlparser::parser::Parser;

use super::*;

fn statement(sql: &str) -> Result<Statement, String> {
    let mut statements =
        Parser::parse_sql(&PostgreSqlDialect {}, sql).map_err(|error| format!("{sql}: {error}"))?;
    match (statements.pop(), statements.is_empty()) {
        (Some(statement), true) => Ok(statement),
        _ => Err(format!("{sql}: expected one statement")),
    }
}

/// The variant only: SET values are checked by the hook's own tests.
fn shape(allowed: &Allowed<'_>) -> &'static str {
    match allowed {
        Allowed::Query => "query",
        Allowed::SetStatementTimeout(_) => "statement_timeout",
        Allowed::SetSearchPath(_) => "search_path",
        Allowed::SetClientEncoding(_) => "client_encoding",
        Allowed::ShowStatementTimeout => "show statement_timeout",
        Allowed::Library => "library",
    }
}

#[test]
fn allows_queries_and_session_statements() -> Result<(), String> {
    let cases = [
        ("SELECT 1", "query"),
        ("WITH t AS (SELECT 1) SELECT * FROM t", "query"),
        ("VALUES (1), (2)", "query"),
        ("EXPLAIN SELECT 1", "query"),
        ("EXPLAIN ANALYZE SELECT count(*) FROM orders", "query"),
        ("SHOW TABLES", "query"),
        ("SHOW COLUMNS FROM orders", "query"),
        ("SET search_path = sales", "search_path"),
        ("SET search_path TO sales, public", "search_path"),
        ("SET statement_timeout = 1000", "statement_timeout"),
        ("SET SESSION statement_timeout TO '5s'", "statement_timeout"),
        ("SET client_encoding = 'UTF8'", "client_encoding"),
        ("SET application_name = 'psql'", "library"),
        ("SET DateStyle = 'ISO, MDY'", "library"),
        ("SET extra_float_digits = 3", "library"),
        ("SET TimeZone = 'UTC'", "library"),
        ("SET TIME ZONE 'UTC'", "library"),
        ("SHOW statement_timeout", "show statement_timeout"),
        ("SHOW search_path", "library"),
        ("BEGIN", "library"),
        ("START TRANSACTION", "library"),
        ("COMMIT", "library"),
        ("ROLLBACK", "library"),
    ];
    for (sql, expected) in cases {
        let parsed = statement(sql)?;
        let allowed = classify(&parsed).map_err(|error| format!("{sql}: {error}"))?;
        assert_eq!(shape(&allowed), expected, "{sql}");
    }
    Ok(())
}

#[test]
fn refuses_everything_else_naming_its_kind() -> Result<(), String> {
    let cases = [
        ("CREATE TABLE t (a INT)", "CREATE TABLE"),
        ("CREATE TABLE t AS SELECT 1", "CREATE TABLE"),
        ("CREATE TEMPORARY TABLE t (a INT)", "CREATE TEMPORARY TABLE"),
        (
            "CREATE EXTERNAL TABLE t (a INT) STORED AS PARQUET LOCATION '/tmp/t'",
            "CREATE EXTERNAL TABLE",
        ),
        (
            "CREATE OR REPLACE VIEW v AS SELECT 1",
            "CREATE OR REPLACE VIEW",
        ),
        ("CREATE SCHEMA scratch", "CREATE SCHEMA"),
        ("INSERT INTO t VALUES (1)", "INSERT"),
        ("UPDATE t SET a = 1", "UPDATE"),
        ("DELETE FROM t", "DELETE"),
        ("COPY t TO '/tmp/t.csv'", "COPY"),
        ("COPY (SELECT 1) TO '/tmp/t.csv'", "COPY"),
        ("DROP TABLE t", "DROP TABLE"),
        ("DROP SCHEMA sales", "DROP SCHEMA"),
        ("ALTER TABLE t ADD COLUMN b INT", "ALTER TABLE"),
        ("TRUNCATE t", "TRUNCATE"),
        ("GRANT SELECT ON t TO someone", "GRANT"),
        ("DECLARE c CURSOR FOR SELECT 1", "DECLARE"),
        ("EXPLAIN INSERT INTO t VALUES (1)", "INSERT"),
        ("EXPLAIN SET search_path = sales", "EXPLAIN"),
    ];
    for (sql, kind) in cases {
        match classify(&statement(sql)?) {
            Err(PgError::ReadOnly { kind: actual }) => assert_eq!(actual, kind, "{sql}"),
            other => {
                return Err(format!(
                    "{sql}: expected a read-only refusal, got {other:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn refuses_unsupported_settings() -> Result<(), String> {
    let cases = [
        ("SET work_mem = '64MB'", "work_mem"),
        (
            "SET datafusion.execution.batch_size = 1",
            "datafusion.execution.batch_size",
        ),
        ("SET ROLE admin", "ROLE"),
    ];
    for (sql, name) in cases {
        match classify(&statement(sql)?) {
            Err(PgError::UnsupportedSetting { name: actual }) => assert_eq!(actual, name, "{sql}"),
            other => {
                return Err(format!(
                    "{sql}: expected an unsupported setting, got {other:?}"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn unparsable_changes_are_still_refused_as_read_only() {
    let cases = [
        (
            "CREATE EXTERNAL TABLE t STORED AS CSV LOCATION '/etc/passwd'",
            Some("CREATE EXTERNAL TABLE"),
        ),
        ("COPY orders TO '/tmp/o' STORED AS PARQUET", Some("COPY")),
        ("insert into", Some("INSERT")),
        ("SELEC 1", None),
        ("SELECT FROM WHERE", None),
        ("", None),
    ];
    for (sql, kind) in cases {
        let refused = refusal_for_unparsed(sql).map(|error| match error {
            PgError::ReadOnly { kind } => kind,
            other => other.to_string(),
        });
        assert_eq!(refused.as_deref(), kind, "{sql}");
    }
}
