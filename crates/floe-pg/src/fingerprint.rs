//! Statement fingerprints: the SQL with every literal replaced by `?`, so statements that differ
//! only in their values log the same line, and no value reaches an info-level log.

use std::ops::ControlFlow;

use datafusion::sql::sqlparser::ast::{Expr, Statement, Value, visit_expressions_mut};

/// `statement` printed with each literal as `?`. Parameters such as `$1` stay as they are.
pub(crate) fn fingerprint(statement: &Statement) -> String {
    let mut statement = statement.clone();
    let _: ControlFlow<()> = visit_expressions_mut(&mut statement, |expr| {
        if let Expr::Value(literal) = expr
            && !matches!(literal.value, Value::Placeholder(_))
        {
            literal.value = Value::Placeholder("?".to_owned());
        }
        ControlFlow::Continue(())
    });
    statement.to_string()
}

#[cfg(test)]
mod tests {
    use datafusion::sql::sqlparser::dialect::PostgreSqlDialect;
    use datafusion::sql::sqlparser::parser::{Parser, ParserError};

    use super::*;

    fn print(sql: &str) -> Result<Vec<String>, ParserError> {
        Ok(Parser::parse_sql(&PostgreSqlDialect {}, sql)?
            .iter()
            .map(fingerprint)
            .collect())
    }

    #[test]
    fn removes_every_literal() -> Result<(), ParserError> {
        let printed = print(
            "SELECT region, 42 FROM orders WHERE region = 'eu-west-1' AND amount > 10.5 \
             AND customer_id = $1 LIMIT 10",
        )?;
        assert_eq!(
            printed,
            [
                "SELECT region, ? FROM orders WHERE region = ? AND amount > ? AND customer_id = $1 \
              LIMIT ?"
            ]
        );
        Ok(())
    }

    #[test]
    fn statements_differing_only_in_values_share_a_fingerprint() -> Result<(), ParserError> {
        let first = print("SELECT * FROM orders WHERE region = 'eu-west-1'")?;
        let second = print("SELECT * FROM orders WHERE region = 'us-east-1'")?;
        assert_eq!(first, second);
        assert!(!first.concat().contains("east"));
        Ok(())
    }
}
