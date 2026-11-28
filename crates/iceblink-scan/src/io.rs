//! Counting, read-only storage under iceberg's `FileIO` (docs/adr/0006-scan-metrics.md).
//!
//! Every query gets its own `IoStats` and its own `FileIO`, built from a
//! `CountingStorageFactory` over the provider's one shared inner storage. Planning reads
//! (manifest list, manifests) and data reads all go through it, so bytes and requests belong to
//! exactly one query even when queries run at the same time.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use iceberg::io::{
    FileIO, FileIOBuilder, FileMetadata, FileRead, FileWrite, InputFile, OutputFile, Storage,
    StorageConfig, StorageFactory,
};
use serde::{Deserialize, Serialize};

use crate::ScanError;
use crate::location::LocationGuard;

/// One query's storage counters.
#[derive(Debug, Default)]
pub(crate) struct IoStats {
    get_requests: AtomicU64,
    bytes_read: AtomicU64,
    /// Metadata requests (`exists`, `metadata`). iceberg's storage trait has no listing call.
    list_requests: AtomicU64,
    write_attempts: AtomicU64,
}

/// A point-in-time copy of a query's storage counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IoCounts {
    /// GET requests: whole-object reads and ranged reads.
    pub get_requests: u64,
    /// Bytes those GETs returned.
    pub bytes_read: u64,
    /// Metadata requests (`exists` and `metadata`).
    pub list_requests: u64,
    /// Writes, deletes and overwrites attempted, and refused. Always zero in a healthy scan.
    pub write_attempts: u64,
}

impl IoStats {
    pub(crate) fn snapshot(&self) -> IoCounts {
        IoCounts {
            get_requests: self.get_requests.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            list_requests: self.list_requests.load(Ordering::Relaxed),
            write_attempts: self.write_attempts.load(Ordering::Relaxed),
        }
    }

    fn record_get(&self, bytes: usize) {
        self.get_requests.fetch_add(1, Ordering::Relaxed);
        self.bytes_read
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

/// Wraps the real storage: reads pass through and are counted, writes never reach it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CountingStorage {
    inner: Arc<dyn Storage>,
    /// Counters stay with the query that made them; a deserialized copy starts at zero.
    #[serde(skip)]
    stats: Arc<IoStats>,
    guard: LocationGuard,
}

impl CountingStorage {
    pub(crate) fn new(inner: Arc<dyn Storage>, stats: Arc<IoStats>, guard: LocationGuard) -> Self {
        Self {
            inner,
            stats,
            guard,
        }
    }

    fn check(&self, path: &str) -> iceberg::Result<()> {
        self.guard.check(path).map_err(ScanError::into_iceberg)
    }

    /// Every write, delete and overwrite ends here, before the inner storage.
    fn refuse(&self) -> iceberg::Error {
        self.stats.write_attempts.fetch_add(1, Ordering::Relaxed);
        ScanError::ReadOnly.into_iceberg()
    }
}

#[async_trait]
#[typetag::serde(name = "floe-counting")]
impl Storage for CountingStorage {
    async fn exists(&self, path: &str) -> iceberg::Result<bool> {
        self.check(path)?;
        self.stats.list_requests.fetch_add(1, Ordering::Relaxed);
        self.inner.exists(path).await
    }

    async fn metadata(&self, path: &str) -> iceberg::Result<FileMetadata> {
        self.check(path)?;
        self.stats.list_requests.fetch_add(1, Ordering::Relaxed);
        self.inner.metadata(path).await
    }

    async fn read(&self, path: &str) -> iceberg::Result<Bytes> {
        self.check(path)?;
        let bytes = self.inner.read(path).await?;
        self.stats.record_get(bytes.len());
        Ok(bytes)
    }

    async fn reader(&self, path: &str) -> iceberg::Result<Box<dyn FileRead>> {
        self.check(path)?;
        let inner = self.inner.reader(path).await?;
        Ok(Box::new(CountingRead {
            inner,
            stats: Arc::clone(&self.stats),
        }))
    }

    async fn write(&self, _path: &str, _bytes: Bytes) -> iceberg::Result<()> {
        Err(self.refuse())
    }

    async fn writer(&self, _path: &str) -> iceberg::Result<Box<dyn FileWrite>> {
        Err(self.refuse())
    }

    async fn delete(&self, _path: &str) -> iceberg::Result<()> {
        Err(self.refuse())
    }

    async fn delete_prefix(&self, _path: &str) -> iceberg::Result<()> {
        Err(self.refuse())
    }

    async fn delete_stream(&self, _paths: BoxStream<'static, String>) -> iceberg::Result<()> {
        Err(self.refuse())
    }

    fn new_input(&self, path: &str) -> iceberg::Result<InputFile> {
        self.check(path)?;
        Ok(InputFile::new(Arc::new(self.clone()), path.to_owned()))
    }

    fn new_output(&self, _path: &str) -> iceberg::Result<OutputFile> {
        Err(self.refuse())
    }
}

/// Counts each ranged read: one GET, and the bytes it returned.
struct CountingRead {
    inner: Box<dyn FileRead>,
    stats: Arc<IoStats>,
}

#[async_trait]
impl FileRead for CountingRead {
    async fn read(&self, range: Range<u64>) -> iceberg::Result<Bytes> {
        let bytes = self.inner.read(range).await?;
        self.stats.record_get(bytes.len());
        Ok(bytes)
    }
}

/// Hands one query's `FileIO` a counting storage over the shared inner storage.
#[derive(Debug, Serialize, Deserialize)]
struct CountingStorageFactory {
    storage: CountingStorage,
}

#[typetag::serde(name = "floe-counting-factory")]
impl StorageFactory for CountingStorageFactory {
    fn build(&self, _config: &StorageConfig) -> iceberg::Result<Arc<dyn Storage>> {
        Ok(Arc::new(self.storage.clone()))
    }
}

/// A `FileIO` for one query: its reads go through `storage` and count into its stats.
pub(crate) fn query_file_io(storage: CountingStorage) -> FileIO {
    FileIOBuilder::new(Arc::new(CountingStorageFactory { storage })).build()
}

#[cfg(test)]
pub(crate) mod tests;
