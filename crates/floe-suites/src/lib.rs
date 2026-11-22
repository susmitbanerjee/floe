//! Support for Floe's end-to-end suites, which live in the workspace root's `tests/`: an
//! in-process Floe, the SQL logic tests' dataset and runner, TPC-H data, and answer
//! comparison (docs/adr/0011-end-to-end-suites.md).

mod compare;
mod server;
mod shop;
mod slt;
mod tpch;

pub use compare::{Expected, cell_matches, compare, has_order_by, parse};
pub use server::{InProcess, SuiteError, connect};
pub use shop::{days, micros, shop};
pub use slt::{FloeDb, FloeDbError, run_file};
pub use tpch::tpch;

/// The workspace root, where `tests/` lives.
pub fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
