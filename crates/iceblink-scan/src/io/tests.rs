//! `CountingStorage` over a spy: writes never reach it, reads are counted, bad paths stop early.

use std::sync::atomic::AtomicUsize;

use futures::stream;
use url::Url;

use super::*;
use crate::StorageSettings;

/// Records every call that reaches it, and serves ten fixed bytes for any read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Spy {
    #[serde(skip)]
    pub(crate) calls: Arc<AtomicUsize>,
}

impl Spy {
    fn touched(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

const TEN_BYTES: &[u8] = b"0123456789";

struct SpyRead;

#[async_trait]
impl FileRead for SpyRead {
    async fn read(&self, range: Range<u64>) -> iceberg::Result<Bytes> {
        let start = usize::try_from(range.start).unwrap_or_default();
        let end = usize::try_from(range.end).unwrap_or_default();
        Ok(Bytes::from_static(
            TEN_BYTES.get(start..end).unwrap_or_default(),
        ))
    }
}

#[async_trait]
#[typetag::serde(name = "iceblink-test-spy")]
impl Storage for Spy {
    async fn exists(&self, _path: &str) -> iceberg::Result<bool> {
        self.touched();
        Ok(true)
    }
    async fn metadata(&self, _path: &str) -> iceberg::Result<FileMetadata> {
        self.touched();
        Ok(FileMetadata { size: 10 })
    }
    async fn read(&self, _path: &str) -> iceberg::Result<Bytes> {
        self.touched();
        Ok(Bytes::from_static(TEN_BYTES))
    }
    async fn reader(&self, _path: &str) -> iceberg::Result<Box<dyn FileRead>> {
        self.touched();
        Ok(Box::new(SpyRead))
    }
    async fn write(&self, _path: &str, _bytes: Bytes) -> iceberg::Result<()> {
        self.touched();
        Ok(())
    }
    async fn writer(&self, _path: &str) -> iceberg::Result<Box<dyn FileWrite>> {
        self.touched();
        Err(iceberg::Error::new(
            iceberg::ErrorKind::Unexpected,
            "spy has no writer",
        ))
    }
    async fn delete(&self, _path: &str) -> iceberg::Result<()> {
        self.touched();
        Ok(())
    }
    async fn delete_prefix(&self, _path: &str) -> iceberg::Result<()> {
        self.touched();
        Ok(())
    }
    async fn delete_stream(&self, _paths: BoxStream<'static, String>) -> iceberg::Result<()> {
        self.touched();
        Ok(())
    }
    fn new_input(&self, path: &str) -> iceberg::Result<InputFile> {
        self.touched();
        Ok(InputFile::new(Arc::new(self.clone()), path.to_owned()))
    }
    fn new_output(&self, path: &str) -> iceberg::Result<OutputFile> {
        self.touched();
        Ok(OutputFile::new(Arc::new(self.clone()), path.to_owned()))
    }
}

pub(crate) fn guard(prefix: &str) -> Result<LocationGuard, url::ParseError> {
    Ok(LocationGuard::new(&StorageSettings {
        endpoint: None,
        region: "us-east-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse(prefix)?],
        allow_local_files: true,
        aws_profile: None,
    }))
}

fn counting(spy: &Spy, stats: &Arc<IoStats>) -> Result<CountingStorage, url::ParseError> {
    Ok(CountingStorage::new(
        Arc::new(spy.clone()),
        Arc::clone(stats),
        guard("s3://warehouse/")?,
    ))
}

const PATH: &str = "s3://warehouse/sales/orders/data/a.parquet";

fn is_read_only(result: iceberg::Result<impl Sized>) -> bool {
    result
        .err()
        .is_some_and(|error| matches!(ScanError::from_iceberg(&error), ScanError::ReadOnly))
}

#[tokio::test]
async fn writes_are_refused_before_the_inner_storage() -> Result<(), Box<dyn std::error::Error>> {
    let spy = Spy::default();
    let stats = Arc::new(IoStats::default());
    let storage = counting(&spy, &stats)?;

    assert!(is_read_only(
        storage.write(PATH, Bytes::from_static(b"x")).await
    ));
    assert!(is_read_only(storage.writer(PATH).await));
    assert!(is_read_only(storage.delete(PATH).await));
    assert!(is_read_only(storage.delete_prefix("s3://warehouse/").await));
    assert!(is_read_only(
        storage.delete_stream(Box::pin(stream::empty())).await
    ));
    assert!(is_read_only(storage.new_output(PATH)));

    assert_eq!(spy.call_count(), 0, "no write may reach the inner storage");
    assert_eq!(stats.snapshot().write_attempts, 6);
    Ok(())
}

#[tokio::test]
async fn reads_pass_through_and_are_counted() -> Result<(), Box<dyn std::error::Error>> {
    let spy = Spy::default();
    let stats = Arc::new(IoStats::default());
    let storage = counting(&spy, &stats)?;

    assert_eq!(storage.read(PATH).await?.len(), 10);
    let reader = storage.reader(PATH).await?;
    assert_eq!(reader.read(2..6).await?.len(), 4);
    assert!(storage.exists(PATH).await?);
    let input = storage.new_input(PATH)?;
    assert_eq!(input.read().await?.len(), 10);

    assert_eq!(
        stats.snapshot(),
        IoCounts {
            get_requests: 3,
            bytes_read: 24,
            list_requests: 1,
            write_attempts: 0,
        }
    );
    Ok(())
}

#[tokio::test]
async fn locations_outside_the_prefixes_never_reach_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let spy = Spy::default();
    let stats = Arc::new(IoStats::default());
    let storage = counting(&spy, &stats)?;
    let outside = "s3://other-bucket/secret/data.parquet";

    let error = storage.read(outside).await.err();
    let mapped = error.as_ref().map(ScanError::from_iceberg);
    assert!(
        matches!(&mapped, Some(ScanError::LocationNotAllowed { path_prefix }) if path_prefix == "s3://other-bucket/"),
        "{mapped:?}"
    );
    assert!(storage.reader(outside).await.is_err());
    assert!(storage.new_input(outside).is_err());
    assert_eq!(spy.call_count(), 0);
    assert_eq!(stats.snapshot(), IoCounts::default());
    Ok(())
}
