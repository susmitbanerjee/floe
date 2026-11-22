//! Serde types that mirror floe.toml key for key. Every table refuses unknown keys, so a typo
//! fails with the key's name and the keys that table accepts.

use std::net::SocketAddr;

use serde::Deserialize;

use super::secret::SecretReference;

/// floe.toml as written, before any rule is checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(super) server: ServerTable,
    pub(super) catalog: CatalogTable,
    pub(super) storage: StorageTable,
}

/// The `[server]` table: Floe's Postgres listener.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ServerTable {
    pub(super) pg_listen: SocketAddr,
    pub(super) max_connections: u32,
    pub(super) statement_timeout: String,
}

/// The `[catalog]` table: the Iceberg REST catalog.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogTable {
    pub(super) uri: String,
    pub(super) warehouse: String,
    pub(super) credential: Option<SecretReference>,
    pub(super) sigv4: Option<SigV4Table>,
}

/// The `[catalog.sigv4]` table: AWS request signing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SigV4Table {
    pub(super) region: String,
    pub(super) service: String,
    pub(super) profile: Option<String>,
}

/// The `[storage]` table: where table data lives and what Floe may read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageTable {
    pub(super) region: String,
    pub(super) endpoint: Option<String>,
    pub(super) path_style: bool,
    pub(super) allowed_prefixes: Vec<String>,
    pub(super) allow_local_files: bool,
}
