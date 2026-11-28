//! `ScanError`, and its trips through iceberg's and DataFusion's error types.

use datafusion::error::DataFusionError;
use iceberg::ErrorKind;

/// Why a scan failed. Messages never carry a full object path or any row data.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ScanError {
    /// Something tried to write, delete or overwrite an object. Floe refused before storage.
    #[error(
        "Floe is read-only: a write, delete or overwrite was refused before it reached storage."
    )]
    ReadOnly,
    /// A table or file location sits outside `storage.allowed_prefixes`.
    #[error(
        "a location under {path_prefix} is outside storage.allowed_prefixes, so Floe won't read \
         it. Add the table's location to storage.allowed_prefixes if it should be readable."
    )]
    LocationNotAllowed {
        /// The scheme and first segment of the location, never the full path.
        path_prefix: String,
    },
    /// The table has no current snapshot, so there is nothing to read.
    #[error("the table has no current snapshot, so there is nothing to scan yet.")]
    SnapshotMissing,
    /// Storage itself failed, or isn't available for this location.
    #[error("storage failed: {message}")]
    Io {
        /// What went wrong.
        message: String,
    },
    /// iceberg reported an error while planning or reading.
    #[error("Iceberg failed: {message}")]
    Iceberg {
        /// What iceberg said.
        message: String,
    },
}

impl ScanError {
    /// Wraps this error for iceberg's storage traits, keeping it recoverable as the source.
    pub(crate) fn into_iceberg(self) -> iceberg::Error {
        let kind = match self {
            Self::ReadOnly | Self::LocationNotAllowed { .. } => ErrorKind::PreconditionFailed,
            Self::SnapshotMissing | Self::Io { .. } | Self::Iceberg { .. } => ErrorKind::Unexpected,
        };
        iceberg::Error::new(kind, self.to_string()).with_source(self)
    }

    /// Recovers a `ScanError` that crossed iceberg's error type, or wraps iceberg's own.
    pub(crate) fn from_iceberg(error: &iceberg::Error) -> Self {
        let mut cause = std::error::Error::source(error);
        while let Some(current) = cause {
            if let Some(scan) = current.downcast_ref::<Self>() {
                return scan.clone();
            }
            if current.downcast_ref::<std::io::Error>().is_some() {
                return Self::Io {
                    message: error.message().to_owned(),
                };
            }
            cause = current.source();
        }
        Self::Iceberg {
            message: format!("{}: {}", error.kind(), error.message()),
        }
    }

    /// Reports this error through DataFusion, still downcastable to `ScanError`.
    pub(crate) fn into_datafusion(self) -> DataFusionError {
        DataFusionError::External(Box::new(self))
    }
}

/// An iceberg error as DataFusion sees it.
pub(crate) fn iceberg_to_datafusion(error: &iceberg::Error) -> DataFusionError {
    ScanError::from_iceberg(error).into_datafusion()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn survives_a_trip_through_iceberg() {
        let original = ScanError::LocationNotAllowed {
            path_prefix: "s3://other/".to_owned(),
        };
        let back = ScanError::from_iceberg(&original.into_iceberg());
        assert!(
            matches!(&back, ScanError::LocationNotAllowed { path_prefix } if path_prefix == "s3://other/"),
            "{back:?}"
        );
    }

    #[test]
    fn plain_iceberg_errors_become_iceberg() {
        let error = iceberg::Error::new(ErrorKind::DataInvalid, "bad manifest");
        let mapped = ScanError::from_iceberg(&error);
        assert!(
            matches!(&mapped, ScanError::Iceberg { message } if message.contains("bad manifest")),
            "{mapped:?}"
        );
    }
}
