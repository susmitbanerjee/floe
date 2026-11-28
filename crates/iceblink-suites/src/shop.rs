//! The SQL logic tests' dataset: a few literal rows, small enough to work out every expected
//! answer by hand. The `.slt` files' results are derived from the rows below, never pasted from
//! a run.
//!
//! ```text
//! shop.customers  customer_id  name   country
//!                 1            Asha   IN
//!                 2            Ben    GB
//!                 3            Chen   NULL
//!                 4            Dana   US
//!
//! shop.orders     order_id  customer_id  amount  status     ordered_on
//!                 10        1            120.50  paid       2026-01-01
//!                 11        1             80.00  paid       2026-01-03
//!                 12        2             NULL   cancelled  2026-01-03
//!                 13        3             35.25  paid       2026-01-04
//!                 14        3             64.75  refunded   2026-01-05
//!                 15        9             10.00  paid       2026-01-06   (no such customer)
//!
//! shop_archive.orders: one row, 1, 1, 5.00, paid, 2025-12-31
//!
//! shop.types: one row per boundary value (see `types`), and a row of NULLs.
//! ```

use std::sync::Arc;

use datafusion::arrow::array::{
    ArrayRef, BooleanArray, Date32Array, Decimal128Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use datafusion::arrow::error::ArrowError;
use iceblink_testkit::Warehouse;

use crate::SuiteError;

/// Days since 1970-01-01 for a civil date (Howard Hinnant's `days_from_civil`).
pub fn days(year: i32, month: u32, day: u32) -> i32 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i32::try_from(month).unwrap_or_default();
    let day = i32::try_from(day).unwrap_or_default();
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Microseconds since the epoch for a UTC date and time.
pub fn micros(date: (i32, u32, u32), time: (i64, i64, i64), fraction: i64) -> i64 {
    let (year, month, day) = date;
    let (hours, minutes, seconds) = time;
    let seconds =
        i64::from(days(year, month, day)) * 86_400 + hours * 3_600 + minutes * 60 + seconds;
    seconds * 1_000_000 + fraction
}

/// Writes every shop table into a new warehouse.
///
/// # Errors
///
/// An invalid batch, or a failure writing or committing a table.
pub async fn shop() -> Result<Warehouse, SuiteError> {
    let warehouse = Warehouse::new().await?;
    warehouse
        .add_table("shop", "customers", &[customers()?])
        .await?;
    warehouse
        .add_table("shop", "orders", &[orders(ORDERS)?])
        .await?;
    warehouse.add_table("shop", "types", &[types()?]).await?;
    warehouse
        .add_table("shop_archive", "orders", &[orders(ARCHIVED)?])
        .await?;
    Ok(warehouse)
}

fn customers() -> Result<RecordBatch, ArrowError> {
    let schema = Schema::new(vec![
        Field::new("customer_id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("country", DataType::Utf8, true),
    ]);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1, 2, 3, 4])),
        Arc::new(StringArray::from(vec!["Asha", "Ben", "Chen", "Dana"])),
        Arc::new(StringArray::from(vec![
            Some("IN"),
            Some("GB"),
            None,
            Some("US"),
        ])),
    ];
    RecordBatch::try_new(Arc::new(schema), columns)
}

/// One order: id, customer, amount in cents, status, (year, month, day).
type OrderRow = (i64, i64, Option<i128>, &'static str, (i32, u32, u32));

const ORDERS: &[OrderRow] = &[
    (10, 1, Some(12_050), "paid", (2026, 1, 1)),
    (11, 1, Some(8_000), "paid", (2026, 1, 3)),
    (12, 2, None, "cancelled", (2026, 1, 3)),
    (13, 3, Some(3_525), "paid", (2026, 1, 4)),
    (14, 3, Some(6_475), "refunded", (2026, 1, 5)),
    (15, 9, Some(1_000), "paid", (2026, 1, 6)),
];

const ARCHIVED: &[OrderRow] = &[(1, 1, Some(500), "paid", (2025, 12, 31))];

fn orders(rows: &[OrderRow]) -> Result<RecordBatch, ArrowError> {
    let schema = Schema::new(vec![
        Field::new("order_id", DataType::Int64, false),
        Field::new("customer_id", DataType::Int64, false),
        Field::new("amount", DataType::Decimal128(14, 2), true),
        Field::new("status", DataType::Utf8, false),
        Field::new("ordered_on", DataType::Date32, false),
    ]);
    let amounts = Decimal128Array::from(rows.iter().map(|row| row.2).collect::<Vec<_>>())
        .with_precision_and_scale(14, 2)?;
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|row| row.0))),
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|row| row.1))),
        Arc::new(amounts),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|row| row.3))),
        Arc::new(Date32Array::from_iter_values(
            rows.iter().map(|row| days(row.4.0, row.4.1, row.4.2)),
        )),
    ];
    RecordBatch::try_new(Arc::new(schema), columns)
}

/// Boundary values for each type Iceblink promises to round-trip.
///
/// ```text
/// id  big                   label     price            day         at                                 flag
/// 1   9223372036854775807   plain     999999999999.99  2026-01-01  2026-01-01 00:00:00 UTC            true
/// 2   -9223372036854775808  (empty)   -0.01            1970-01-01  1999-12-31 23:59:59.123456 UTC     false
/// 3   0                     naïve ☃   0.00             2024-02-29  2026-06-15 12:30:00 UTC            NULL
/// 4   NULL                  NULL      NULL             NULL        NULL                               NULL
/// ```
fn types() -> Result<RecordBatch, ArrowError> {
    let schema = Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("big", DataType::Int64, true),
        Field::new("label", DataType::Utf8, true),
        Field::new("price", DataType::Decimal128(14, 2), true),
        Field::new("day", DataType::Date32, true),
        Field::new(
            "at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("+00:00".into())),
            true,
        ),
        Field::new("flag", DataType::Boolean, true),
    ]);
    let price = Decimal128Array::from(vec![Some(99_999_999_999_999), Some(-1), Some(0), None])
        .with_precision_and_scale(14, 2)?;
    let at = TimestampMicrosecondArray::from(vec![
        Some(micros((2026, 1, 1), (0, 0, 0), 0)),
        Some(micros((1999, 12, 31), (23, 59, 59), 123_456)),
        Some(micros((2026, 6, 15), (12, 30, 0), 0)),
        None,
    ])
    .with_timezone("+00:00");
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1, 2, 3, 4])),
        Arc::new(Int64Array::from(vec![
            Some(i64::MAX),
            Some(i64::MIN),
            Some(0),
            None,
        ])),
        Arc::new(StringArray::from(vec![
            Some("plain"),
            Some(""),
            Some("naïve ☃"),
            None,
        ])),
        Arc::new(price),
        Arc::new(Date32Array::from(vec![
            Some(days(2026, 1, 1)),
            Some(days(1970, 1, 1)),
            Some(days(2024, 2, 29)),
            None,
        ])),
        Arc::new(at),
        Arc::new(BooleanArray::from(vec![
            Some(true),
            Some(false),
            None,
            None,
        ])),
    ];
    RecordBatch::try_new(Arc::new(schema), columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_epoch_days() {
        assert_eq!(days(1970, 1, 1), 0);
        assert_eq!(days(2000, 1, 1), 10_957);
        assert_eq!(days(2024, 2, 29), 19_782);
        assert_eq!(days(2026, 1, 1), 20_454);
        assert_eq!(micros((1970, 1, 2), (0, 0, 1), 5), 86_401_000_005);
    }
}
