//! Turns Apache Iceberg tables into DataFusion scans and counts what each scan read: snapshot,
//! files and bytes.
//!
//! This crate only reads. It must never write, delete or overwrite anything in storage: every
//! scan reads through a counting storage that refuses writes before they reach the real one,
//! and the S3 storage under it refuses them again before building any request.

mod catalog;
mod error;
mod exec;
mod io;
mod location;
mod provider;
mod receipt;
mod s3;
mod session;
mod settings;
mod storage;

pub use catalog::{LakeCatalogProvider, RefreshSummary};
pub use error::ScanError;
pub use exec::LakeScanExec;
pub use io::IoCounts;
pub use provider::LakeTableProvider;
pub use receipt::{QueryReceipt, ScanReceipt};
pub use session::{CATALOG_NAME, new_session};
pub use settings::StorageSettings;
