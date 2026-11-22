use std::time::Duration;

use secrecy::SecretString;
use url::Url;

/// How Floe reaches an Iceberg REST catalog, already checked by the binary's config loader.
#[derive(Debug, Clone)]
pub struct CatalogSettings {
    /// The catalog's base URI. https whenever a credential or SigV4 signing is set.
    pub uri: Url,
    /// The warehouse to open, as the catalog names it. For AWS Glue, the account ID.
    pub warehouse: String,
    /// Bearer token for catalogs that need one, resolved from an environment variable.
    pub credential: Option<SecretString>,
    /// AWS SigV4 request signing, for Glue and S3 Tables endpoints.
    pub sigv4: Option<SigV4Settings>,
    /// Limits for every call Floe makes to the catalog.
    pub timeouts: CatalogTimeouts,
}

/// AWS SigV4 signing for catalog requests. Credentials come from the AWS environment, not here.
#[derive(Debug, Clone)]
pub struct SigV4Settings {
    /// AWS region name, such as `ap-south-1`.
    pub region: String,
    /// The AWS service the catalog endpoint belongs to.
    pub service: SigV4Service,
    /// The AWS profile to read credentials from. `None` follows `AWS_PROFILE` and the rest of
    /// the default chain.
    pub profile: Option<String>,
}

/// The AWS services whose Iceberg REST endpoints Floe can sign for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigV4Service {
    /// AWS Glue Data Catalog (signing name `glue`).
    Glue,
    /// Amazon S3 Tables (signing name `s3tables`).
    S3Tables,
}

/// Connect and total timeouts for catalog calls. Every outbound call needs both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogTimeouts {
    /// Longest wait for a connection to the catalog.
    pub connect: Duration,
    /// Longest wait for a whole request, including the connection.
    pub total: Duration,
}

impl Default for CatalogTimeouts {
    /// Phase 1 has no setting for these: 5 seconds to connect, 30 seconds per request.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            total: Duration::from_secs(30),
        }
    }
}
