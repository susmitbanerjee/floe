//! Compares query results with expected answer files: integers and text exactly, other
//! numbers within 0.01, and in any order when the query sets none.

/// Rows parsed from an answer file: a header line, then one line per row.
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    /// The header's column names.
    pub header: Vec<String>,
    /// Each row's cells, as text.
    pub rows: Vec<Vec<String>>,
}

/// Splits `text` on `separator` (`|` for DuckDB's answers, `,` for iceblink-lab's checks). After
/// the header every line is a row, even an empty one: that is how DuckDB writes a single NULL.
pub fn parse(text: &str, separator: char) -> Expected {
    let mut lines = text.lines();
    let split = |line: &str| line.split(separator).map(str::to_owned).collect::<Vec<_>>();
    Expected {
        header: lines.next().map(split).unwrap_or_default(),
        rows: lines.map(split).collect(),
    }
}

/// True when `actual` matches `expected`: identical text, NULL against an empty field (how
/// answer files write NULL), equal integers, or numbers within 0.01 of each other (decimals
/// print with different scales across engines).
pub fn cell_matches(actual: &str, expected: &str) -> bool {
    if actual == expected || (actual == "NULL" && expected.is_empty()) {
        return true;
    }
    if let (Ok(actual), Ok(expected)) = (actual.parse::<i64>(), expected.parse::<i64>()) {
        return actual == expected;
    }
    match (actual.parse::<f64>(), expected.parse::<f64>()) {
        (Ok(actual), Ok(expected)) => (actual - expected).abs() <= 0.01,
        _ => false,
    }
}

/// Compares row by row. With `ordered` false, both sides are sorted first.
///
/// # Errors
///
/// The first difference: a row count, a row width, or a cell, with its position.
pub fn compare(actual: &[Vec<String>], expected: &Expected, ordered: bool) -> Result<(), String> {
    if actual.len() != expected.rows.len() {
        return Err(format!(
            "{} rows, expected {}",
            actual.len(),
            expected.rows.len()
        ));
    }
    let mut actual = actual.to_vec();
    let mut wanted = expected.rows.clone();
    if !ordered {
        actual.sort();
        wanted.sort();
    }
    for (index, (got, want)) in actual.iter().zip(&wanted).enumerate() {
        if got.len() != want.len() {
            return Err(format!(
                "row {index} has {} columns, expected {}",
                got.len(),
                want.len()
            ));
        }
        for (column, (cell, wanted_cell)) in got.iter().zip(want).enumerate() {
            if !cell_matches(cell, wanted_cell) {
                let name = expected.header.get(column).map_or("?", String::as_str);
                return Err(format!(
                    "row {index}, column {name}: got {cell:?}, expected {wanted_cell:?}"
                ));
            }
        }
    }
    Ok(())
}

/// True when `sql`'s outermost query orders its rows, so the comparison must keep order.
pub fn has_order_by(sql: &str) -> bool {
    sql.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair == ["order", "by"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_and_text_match_exactly_and_decimals_within_a_cent() {
        let cases = [
            ("380456", "380456", true),
            ("380456.00", "380456", true),
            ("380457", "380456", false),
            ("25.575154", "25.575154611454693", true),
            ("1193053.23", "1193053.2253", true),
            ("1193053.24", "1193053.2253", false),
            ("1995-03-11", "1995-03-11", true),
            ("Supplier#000000021", "Supplier#000000021 ", false),
            ("NULL", "", true),
            ("0", "", false),
        ];
        for (actual, expected, matches) in cases {
            assert_eq!(
                cell_matches(actual, expected),
                matches,
                "{actual} vs {expected}"
            );
        }
    }

    #[test]
    fn unordered_results_compare_as_sets() {
        let expected = parse("a|b\n1|x\n2|y\n", '|');
        let swapped = vec![
            vec!["2".to_owned(), "y".to_owned()],
            vec!["1".to_owned(), "x".to_owned()],
        ];
        assert!(compare(&swapped, &expected, false).is_ok());
        let failure = compare(&swapped, &expected, true).err().unwrap_or_default();
        assert!(failure.contains("column a"), "{failure}");
    }

    #[test]
    fn an_empty_line_is_a_row_holding_null() {
        let expected = parse("avg_yearly\n\n", '|');
        assert_eq!(expected.rows, [[String::new()]]);
        let actual = vec![vec!["NULL".to_owned()]];
        assert!(compare(&actual, &expected, true).is_ok());
    }

    #[test]
    fn finds_order_by_across_line_breaks() {
        assert!(has_order_by("SELECT 1\nORDER\n  BY 1"));
        assert!(!has_order_by("SELECT ordered, by FROM t"));
    }
}
