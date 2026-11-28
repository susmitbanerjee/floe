//! The values of Floe's own SET statements: statement_timeout, search_path and
//! client_encoding, turned into checked values or a clear refusal.

use std::time::Duration;

use datafusion::prelude::SessionContext;
use datafusion::sql::sqlparser::ast::{Expr, Value};

use crate::PgError;

/// The timeout `SET statement_timeout` asks for. `DEFAULT` is the server limit; a number alone
/// is milliseconds, as in Postgres; units `ms`, `s`, `min` and `h` are accepted.
///
/// # Errors
///
/// [`PgError::InvalidTimeout`] for an unreadable value, and [`PgError::TimeoutAboveLimit`]
/// above the limit or for 0, which in Postgres means no limit at all.
pub(crate) fn timeout(values: &[Expr], limit: Duration) -> Result<Duration, PgError> {
    let text = first_text(values);
    if text.eq_ignore_ascii_case("default") {
        return Ok(limit);
    }
    let invalid = || PgError::InvalidTimeout {
        value: text.clone(),
    };
    let digits = text.trim().len()
        - text
            .trim()
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let (number, unit) = text.trim().split_at(digits);
    let number: u64 = number.parse().map_err(|_| invalid())?;
    let unit_ms: u64 = match unit.trim() {
        "" | "ms" => 1,
        "s" => 1_000,
        "min" => 60_000,
        "h" => 3_600_000,
        _ => return Err(invalid()),
    };
    let requested = Duration::from_millis(number.checked_mul(unit_ms).ok_or_else(invalid)?);
    if requested.is_zero() || requested > limit {
        return Err(PgError::TimeoutAboveLimit {
            limit: timeout_text(limit),
        });
    }
    Ok(requested)
}

/// A timeout the way Postgres shows it: `30s`, `500ms` or `2min`.
pub(crate) fn timeout_text(timeout: Duration) -> String {
    let ms = timeout.as_millis();
    if ms > 0 && ms.is_multiple_of(60_000) {
        format!("{}min", ms / 60_000)
    } else if ms > 0 && ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

/// Makes the first schema in the path that exists the session's default schema, and returns
/// it. `"$user"` is skipped: Floe has no per-user schemas.
///
/// # Errors
///
/// [`PgError::UnknownSchema`], naming the first entry, when none of them exists.
pub(crate) fn search_path(values: &[Expr], session: &SessionContext) -> Result<String, PgError> {
    let names: Vec<String> = values
        .iter()
        .flat_map(|value| {
            text(value)
                .split(',')
                .map(|name| name.trim().trim_matches('"').to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|name| !name.is_empty() && name != "$user")
        .collect();
    let catalog_name = session
        .state_ref()
        .read()
        .config_options()
        .catalog
        .default_catalog
        .clone();
    let catalog = session.catalog(&catalog_name);
    let found = names.iter().find(|name| {
        catalog
            .as_ref()
            .is_some_and(|catalog| catalog.schema(name).is_some())
    });
    let Some(schema) = found else {
        return Err(PgError::UnknownSchema {
            name: names.first().cloned().unwrap_or_default(),
        });
    };
    session
        .state_ref()
        .write()
        .config_mut()
        .options_mut()
        .catalog
        .default_schema
        .clone_from(schema);
    Ok(schema.clone())
}

/// Accepts UTF8 under any of its Postgres spellings.
///
/// # Errors
///
/// [`PgError::UnsupportedEncoding`] for any other encoding.
pub(crate) fn encoding(values: &[Expr]) -> Result<(), PgError> {
    let encoding = first_text(values);
    let normalized = encoding.replace(['-', '_'], "").to_uppercase();
    if matches!(normalized.as_str(), "UTF8" | "UNICODE") {
        Ok(())
    } else {
        Err(PgError::UnsupportedEncoding { encoding })
    }
}

fn first_text(values: &[Expr]) -> String {
    values.first().map(text).unwrap_or_default()
}

/// A SET value as text: quoted strings without their quotes, identifiers by name.
fn text(value: &Expr) -> String {
    match value {
        Expr::Value(literal) => match &literal.value {
            Value::SingleQuotedString(text) | Value::DoubleQuotedString(text) => text.clone(),
            Value::Number(number, _) => number.clone(),
            other => other.to_string(),
        },
        Expr::Identifier(ident) => ident.value.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use datafusion::sql::sqlparser::ast::{Set, Statement};
    use datafusion::sql::sqlparser::dialect::PostgreSqlDialect;
    use datafusion::sql::sqlparser::parser::Parser;

    use super::*;

    /// The values of `SET statement_timeout = <value>`.
    fn values(value: &str) -> Result<Vec<Expr>, String> {
        let sql = format!("SET statement_timeout = {value}");
        let parsed = Parser::parse_sql(&PostgreSqlDialect {}, &sql).map_err(|e| e.to_string())?;
        match parsed.into_iter().next() {
            Some(Statement::Set(Set::SingleAssignment { values, .. })) => Ok(values),
            other => Err(format!("{sql} parsed as {other:?}")),
        }
    }

    #[test]
    fn reads_timeouts_up_to_the_limit() -> Result<(), String> {
        let limit = Duration::from_secs(30);
        let cases = [
            ("1", 1),
            ("2500", 2_500),
            ("'100ms'", 100),
            ("'5s'", 5_000),
            ("'5 s'", 5_000),
            ("'0min'", 0),
            ("DEFAULT", 30_000),
            ("'30s'", 30_000),
        ];
        for (value, ms) in cases {
            let read = timeout(&values(value)?, limit);
            if ms == 0 {
                assert!(
                    matches!(read, Err(PgError::TimeoutAboveLimit { .. })),
                    "{value}"
                );
            } else {
                assert_eq!(read.ok(), Some(Duration::from_millis(ms)), "{value}");
            }
        }
        Ok(())
    }

    #[test]
    fn refuses_timeouts_above_the_limit_or_unreadable() -> Result<(), String> {
        let limit = Duration::from_secs(30);
        for value in ["0", "'31s'", "'1min'", "'1h'"] {
            let read = timeout(&values(value)?, limit);
            match read {
                Err(PgError::TimeoutAboveLimit { limit }) => assert_eq!(limit, "30s"),
                other => return Err(format!("{value}: {other:?}")),
            }
        }
        for value in ["'soon'", "'5 weeks'", "'-1'", "'99999999999999999999'"] {
            let read = timeout(&values(value)?, limit);
            assert!(
                matches!(read, Err(PgError::InvalidTimeout { .. })),
                "{value}"
            );
        }
        Ok(())
    }

    #[test]
    fn shows_timeouts_as_postgres_does() {
        assert_eq!(timeout_text(Duration::from_secs(30)), "30s");
        assert_eq!(timeout_text(Duration::from_millis(1_500)), "1500ms");
        assert_eq!(timeout_text(Duration::from_secs(120)), "2min");
    }

    #[test]
    fn accepts_only_utf8() -> Result<(), String> {
        for value in ["'UTF8'", "'utf-8'", "'UNICODE'"] {
            assert!(encoding(&values(value)?).is_ok(), "{value}");
        }
        assert!(matches!(
            encoding(&values("'LATIN1'")?),
            Err(PgError::UnsupportedEncoding { .. })
        ));
        Ok(())
    }
}
