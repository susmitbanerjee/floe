//! Which storage a table location reads through: S3 for `s3://` and `s3a://`, the local
//! filesystem for `file://` (tests only: the location guard keeps it to allowed prefixes).

use std::sync::Arc;

use iceberg::io::{LocalFsStorage, Storage};

use crate::s3::{S3Settings, S3Storage};
use crate::{ScanError, StorageSettings};

/// The storages a catalog's tables share: one S3 client set, one local filesystem.
#[derive(Debug, Clone)]
pub(crate) struct Storages {
    s3: Arc<dyn Storage>,
    local: Arc<dyn Storage>,
}

impl Storages {
    pub(crate) fn new(settings: &StorageSettings) -> Self {
        Self {
            s3: Arc::new(S3Storage::new(S3Settings::from(settings))),
            local: Arc::new(LocalFsStorage),
        }
    }

    /// The storage for files under `location`.
    ///
    /// # Errors
    ///
    /// [`ScanError::Io`] for a scheme Floe doesn't read, such as `gs://`.
    pub(crate) fn for_location(&self, location: &str) -> Result<Arc<dyn Storage>, ScanError> {
        let scheme = location.split_once("://").map(|(scheme, _)| scheme);
        match scheme {
            Some("s3" | "s3a") => Ok(Arc::clone(&self.s3)),
            Some("file") => Ok(Arc::clone(&self.local)),
            None if location.starts_with('/') => Ok(Arc::clone(&self.local)),
            _ => Err(ScanError::Io {
                message: format!(
                    "{} storage isn't supported: Floe reads s3:// locations, and file:// ones in \
                     tests",
                    scheme.unwrap_or("this")
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    #[test]
    fn routes_each_scheme() -> Result<(), url::ParseError> {
        let storages = Storages::new(&StorageSettings {
            endpoint: Some(Url::parse("http://127.0.0.1:9000")?),
            region: "us-east-1".to_owned(),
            path_style: true,
            allowed_prefixes: vec![Url::parse("s3://warehouse/")?],
            allow_local_files: false,
        });
        let is_s3 = |location: &str| {
            storages
                .for_location(location)
                .is_ok_and(|storage| format!("{storage:?}").starts_with("S3Storage"))
        };
        let is_local = |location: &str| {
            storages
                .for_location(location)
                .is_ok_and(|storage| format!("{storage:?}").starts_with("LocalFsStorage"))
        };
        assert!(is_s3("s3://warehouse/sales/orders"));
        assert!(is_s3("s3a://warehouse/sales/orders"));
        assert!(is_local("file:///tmp/warehouse/sales/orders"));
        assert!(is_local("/tmp/warehouse/sales/orders"));
        let refused = storages.for_location("gs://bucket/sales/orders").err();
        assert!(
            refused.is_some_and(|error| error.to_string().contains("gs storage isn't supported")),
            "gs:// was accepted"
        );
        Ok(())
    }
}
