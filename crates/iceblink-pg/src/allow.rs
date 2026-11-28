//! The statement allow-list: what Iceblink runs, what it keeps itself, and what it refuses.
//!
//! Iceblink runs SELECT, WITH, VALUES, EXPLAIN [ANALYZE] of those, and SHOW; it accepts SET for a
//! few session settings, and BEGIN, COMMIT and ROLLBACK as no-ops. Every other statement is
//! refused with SQLSTATE 25006, naming its kind. DataFusion's own read-only plan check runs
//! after this as a second layer.

use datafusion::sql::sqlparser::ast::{Expr, ObjectName, Set, Statement};
use datafusion::sql::sqlparser::dialect::PostgreSqlDialect;
use datafusion::sql::sqlparser::keywords::Keyword;
use datafusion::sql::sqlparser::tokenizer::{Token, Tokenizer};

use crate::PgError;

/// What Iceblink does with a statement the allow-list lets through.
#[derive(Debug, PartialEq)]
pub(crate) enum Allowed<'a> {
    /// SELECT, WITH, VALUES, EXPLAIN of one of those, SHOW TABLES or SHOW COLUMNS: planned and
    /// run.
    Query,
    /// `SET statement_timeout`, which Iceblink keeps and only lets go down.
    SetStatementTimeout(&'a [Expr]),
    /// `SET search_path`, which picks the session's default schema.
    SetSearchPath(&'a [Expr]),
    /// `SET client_encoding`, UTF8 only.
    SetClientEncoding(&'a [Expr]),
    /// `SHOW statement_timeout`.
    ShowStatementTimeout,
    /// BEGIN, COMMIT, ROLLBACK, other SHOWs, and the settings the library keeps:
    /// application_name, DateStyle, extra_float_digits and TimeZone.
    Library,
}

/// Sorts `statement` onto the allow-list.
///
/// # Errors
///
/// [`PgError::ReadOnly`] naming the statement kind, or [`PgError::UnsupportedSetting`] for SET
/// of anything not on the list.
pub(crate) fn classify(statement: &Statement) -> Result<Allowed<'_>, PgError> {
    match statement {
        Statement::Query(_) | Statement::ShowTables { .. } | Statement::ShowColumns { .. } => {
            Ok(Allowed::Query)
        }
        Statement::Explain {
            statement: inner, ..
        } => match classify(inner)? {
            Allowed::Query => Ok(Allowed::Query),
            _ => Err(refuse(statement)),
        },
        Statement::Set(set) => classify_set(set),
        Statement::ShowVariable { variable } => {
            let name = ObjectName::from(variable.clone()).to_string();
            Ok(if setting(&name) == "statement_timeout" {
                Allowed::ShowStatementTimeout
            } else {
                Allowed::Library
            })
        }
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. } => Ok(Allowed::Library),
        _ => Err(refuse(statement)),
    }
}

fn classify_set(set: &Set) -> Result<Allowed<'_>, PgError> {
    match set {
        Set::SingleAssignment {
            variable,
            values,
            hivevar: false,
            ..
        } => {
            let name = variable.to_string();
            match setting(&name).as_str() {
                "statement_timeout" => Ok(Allowed::SetStatementTimeout(values)),
                "search_path" => Ok(Allowed::SetSearchPath(values)),
                "client_encoding" => Ok(Allowed::SetClientEncoding(values)),
                "application_name" | "datestyle" | "extra_float_digits" | "timezone" => {
                    Ok(Allowed::Library)
                }
                _ => Err(PgError::UnsupportedSetting { name }),
            }
        }
        Set::SetTimeZone { .. } => Ok(Allowed::Library),
        // SET ROLE, SET NAMES, SET TRANSACTION…: the word after SET names it.
        other => {
            let text = other.to_string();
            let name = text.split_whitespace().nth(1).unwrap_or("this setting");
            Err(PgError::UnsupportedSetting {
                name: name.to_owned(),
            })
        }
    }
}

/// A setting name as Postgres compares it: case-insensitive, quotes ignored.
fn setting(name: &str) -> String {
    name.trim_matches('"').to_lowercase()
}

fn refuse(statement: &Statement) -> PgError {
    PgError::ReadOnly {
        kind: kind(statement),
    }
}

/// The statement kind a person recognises, such as `CREATE TABLE` or `INSERT`.
pub(crate) fn kind(statement: &Statement) -> String {
    match statement {
        Statement::CreateTable(table) if table.external => "CREATE EXTERNAL TABLE".to_owned(),
        Statement::Drop { object_type, .. } => format!("DROP {object_type}"),
        _ => leading_kind(&statement.to_string()),
    }
}

/// Verbs that change something. SQL starting with one is refused even when it doesn't parse,
/// such as DataFusion's `CREATE EXTERNAL TABLE … STORED AS CSV`.
const CHANGING_VERBS: [&str; 12] = [
    "ALTER", "CALL", "COPY", "CREATE", "DELETE", "DROP", "GRANT", "INSERT", "MERGE", "REVOKE",
    "TRUNCATE", "UPDATE",
];

/// For SQL the parser rejected: the read-only refusal when it starts with a changing verb, so a
/// person learns that the statement can't run at all rather than how to fix its syntax.
pub(crate) fn refusal_for_unparsed(sql: &str) -> Option<PgError> {
    let kind = leading_kind(sql);
    let verb = kind.split(' ').next().unwrap_or_default();
    CHANGING_VERBS
        .contains(&verb)
        .then_some(PgError::ReadOnly { kind })
}

/// Object kinds that end a `CREATE …` or `ALTER …` name.
const OBJECT_KINDS: [Keyword; 14] = [
    Keyword::DATABASE,
    Keyword::DOMAIN,
    Keyword::EXTENSION,
    Keyword::FUNCTION,
    Keyword::INDEX,
    Keyword::POLICY,
    Keyword::PROCEDURE,
    Keyword::ROLE,
    Keyword::SCHEMA,
    Keyword::SEQUENCE,
    Keyword::TABLE,
    Keyword::TRIGGER,
    Keyword::TYPE,
    Keyword::VIEW,
];

/// The leading keywords of `sql`: the verb, and for CREATE and ALTER the words up to the kind
/// of object, as in `CREATE OR REPLACE VIEW`.
fn leading_kind(sql: &str) -> String {
    let dialect = PostgreSqlDialect {};
    let tokens = Tokenizer::new(&dialect, sql).tokenize().unwrap_or_default();
    let mut words: Vec<String> = Vec::new();
    for token in tokens {
        match token {
            Token::Whitespace(_) => {}
            Token::Word(word)
                if word.quote_style.is_none() && word.keyword != Keyword::NoKeyword =>
            {
                words.push(word.value.to_uppercase());
                let continues = matches!(word.keyword, Keyword::CREATE | Keyword::ALTER)
                    || (words.len() > 1 && !OBJECT_KINDS.contains(&word.keyword));
                if !continues || words.len() == 5 {
                    break;
                }
            }
            _ => break,
        }
    }
    if words.is_empty() {
        "this statement".to_owned()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod tests;
