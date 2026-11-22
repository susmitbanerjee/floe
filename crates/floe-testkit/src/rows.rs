//! Deterministic rows for the `sales.orders` fixture: the same seed, day and file always give the
//! same batch. No wall clock and no OS randomness.

use std::sync::Arc;

use datafusion::arrow::array::{
    ArrayRef, Decimal128Array, Int64Array, StringArray, TimestampMicrosecondArray,
};
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::error::ArrowError;
use datafusion::arrow::record_batch::RecordBatch;

/// 2026-01-01T00:00:00Z as days since the Unix epoch: day 0 of every fixture.
pub(crate) const BASE_EPOCH_DAY: i32 = 20_454;

const MICROS_PER_DAY: i64 = 86_400_000_000;
const TENANTS: [&str; 5] = ["acme", "globex", "initech", "umbrella", "stark"];
const REGIONS: [&str; 4] = ["ap-south-1", "eu-west-1", "us-east-1", "us-west-2"];
const STATUSES: [&str; 5] = ["placed", "paid", "shipped", "delivered", "refunded"];

/// SplitMix64: tiny, fast, and the same everywhere. Good enough for fixture data.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<'a>(&mut self, choices: &[&'a str]) -> &'a str {
        let index = usize::try_from(self.below(choices.len() as u64)).unwrap_or_default();
        choices.get(index).copied().unwrap_or_default()
    }
}

/// Which file of which day to generate, and how many rows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileSpec {
    pub(crate) seed: u64,
    pub(crate) day: u32,
    pub(crate) file: u32,
    pub(crate) files_per_day: u32,
    pub(crate) rows: u32,
}

/// One file's rows, in the column order of `schema` (the fixture's `sales.orders` schema).
pub(crate) fn batch(schema: SchemaRef, spec: FileSpec) -> Result<RecordBatch, ArrowError> {
    let stream = (u64::from(spec.day) << 32) | u64::from(spec.file);
    let mut rng = SplitMix64(spec.seed ^ stream.wrapping_mul(0xD6E8_FEB8_6659_FD93));
    let first_id = (i64::from(spec.day) * i64::from(spec.files_per_day) + i64::from(spec.file))
        * i64::from(spec.rows);
    let day_start =
        i64::from(BASE_EPOCH_DAY + i32::try_from(spec.day).unwrap_or(i32::MAX)) * MICROS_PER_DAY;

    let rows = usize::try_from(spec.rows).unwrap_or_default();
    let mut order_ids = Vec::with_capacity(rows);
    let mut tenants = Vec::with_capacity(rows);
    let mut customers = Vec::with_capacity(rows);
    let mut regions = Vec::with_capacity(rows);
    let mut statuses = Vec::with_capacity(rows);
    let mut amounts = Vec::with_capacity(rows);
    let mut created = Vec::with_capacity(rows);
    for row in 0..i64::from(spec.rows) {
        order_ids.push(first_id + row);
        tenants.push(rng.pick(&TENANTS));
        customers.push(format!("c-{:05}", rng.below(20_000)));
        regions.push(rng.pick(&REGIONS));
        statuses.push(rng.pick(&STATUSES));
        // 1.00 to 9,999.99, in cents.
        amounts.push(i128::from(100 + rng.below(999_900)));
        let offset = i64::try_from(rng.below(MICROS_PER_DAY.unsigned_abs())).unwrap_or_default();
        created.push(day_start + offset);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(order_ids)),
        Arc::new(StringArray::from(tenants)),
        Arc::new(StringArray::from(customers)),
        Arc::new(StringArray::from(regions)),
        Arc::new(StringArray::from(statuses)),
        Arc::new(Decimal128Array::from(amounts).with_precision_and_scale(14, 2)?),
        Arc::new(TimestampMicrosecondArray::from(created).with_timezone("+00:00")),
    ];
    RecordBatch::try_new(schema, columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_is_stable() {
        // First outputs of SplitMix64 seeded with 0, as published with the algorithm.
        let mut rng = SplitMix64(0);
        assert_eq!(rng.next(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next(), 0x6E78_9E6A_A1B9_65F4);
    }
}
