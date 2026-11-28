//! Storage for tables loaded by iceblink-catalog: none. iceberg-catalog-rest 0.11 won't load a
//! table without a `StorageFactory`, but object storage belongs to iceblink-scan. This factory lets
//! `load_table` return metadata while every read, write or delete through the table's FileIO
//! fails before it touches anything.

use std::sync::Arc;

use iceberg::ErrorKind;
use iceberg::io::{Storage, StorageConfig, StorageFactory};
use serde::{Deserialize, Serialize};

/// FileIO builds its storage lazily, on first use. Refusing here refuses every operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct MetadataOnlyStorage;

#[typetag::serde(name = "iceblink-metadata-only")]
impl StorageFactory for MetadataOnlyStorage {
    fn build(&self, _config: &StorageConfig) -> iceberg::Result<Arc<dyn Storage>> {
        Err(iceberg::Error::new(
            ErrorKind::FeatureUnsupported,
            "object storage is attached by iceblink-scan; iceblink-catalog loads table metadata \
             only",
        ))
    }
}

#[cfg(test)]
mod tests {
    use iceberg::io::FileIOBuilder;

    use super::*;

    #[tokio::test]
    async fn refuses_every_read_and_write() {
        let file_io = FileIOBuilder::new(Arc::new(MetadataOnlyStorage)).build();
        let path = "s3://warehouse/sales/orders/data/part-0.parquet";
        let refused = [
            file_io.new_input(path).err(),
            file_io.new_output(path).err(),
            file_io.exists(path).await.err(),
            file_io.delete(path).await.err(),
        ];
        for (index, error) in refused.iter().enumerate() {
            assert!(
                error
                    .as_ref()
                    .is_some_and(|error| error.kind() == ErrorKind::FeatureUnsupported),
                "operation {index}: {error:?}"
            );
        }
    }
}
