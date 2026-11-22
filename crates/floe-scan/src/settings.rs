use url::Url;

/// Where table data lives and which locations Floe may read, already checked by the binary's
/// config loader.
#[derive(Debug, Clone)]
pub struct StorageSettings {
    /// S3-compatible endpoint such as MinIO. `None` means AWS S3 itself.
    pub endpoint: Option<Url>,
    /// AWS region name used to sign storage requests, such as `us-east-1`.
    pub region: String,
    /// Address buckets as `endpoint/bucket` instead of `bucket.endpoint`. MinIO needs this.
    pub path_style: bool,
    /// Table and file locations must sit under one of these. Each ends in a slash.
    pub allowed_prefixes: Vec<Url>,
    /// Whether `file://` prefixes are allowed. Only for local tests.
    pub allow_local_files: bool,
}
