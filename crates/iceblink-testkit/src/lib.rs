//! Test fixtures for Iceblink: real Iceberg tables, written with iceberg-rust's Parquet writer into
//! an in-memory catalog over local files in a temp dir. `TableFixture` builds the generated
//! `sales.orders` table; `Warehouse` holds tables built from any Arrow batches.
//!
//! Data files are deterministic: the same seed gives the same bytes. Table metadata isn't,
//! because iceberg generates the table UUID, snapshot IDs and commit timestamps itself.

mod fixture;
mod rows;
mod warehouse;

pub use fixture::{Fixture, TableFixture};
pub use warehouse::Warehouse;
