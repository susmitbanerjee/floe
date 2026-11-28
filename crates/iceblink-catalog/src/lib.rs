//! Talks to Apache Iceberg REST catalogs: lists namespaces and tables and loads table metadata
//! for a snapshot.
//!
//! This crate only reads. It must never create, update, commit to or drop anything in a catalog.
//! [`LakeCatalog`] has no method that could.

mod aws;
mod error;
mod http;
mod lake_catalog;
mod names;
mod retry;
mod settings;
mod sigv4;
mod storage;

pub use aws::AwsCredentials;
pub use error::CatalogError;
/// The iceberg catalog interface, re-exported for [`LakeCatalog::from_catalog`].
pub use iceberg::Catalog;
/// A loaded Iceberg table: its metadata, and file IO that refuses every operation until
/// iceblink-scan attaches storage.
pub use iceberg::table::Table;
pub use lake_catalog::LakeCatalog;
pub use names::{NamespaceName, TableName};
pub use settings::{CatalogSettings, CatalogTimeouts, SigV4Service, SigV4Settings};
