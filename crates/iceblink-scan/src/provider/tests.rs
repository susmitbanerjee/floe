//! The provider stops before storage when a table sits outside the allowed prefixes.

use std::error::Error;

use datafusion::error::DataFusionError;
use datafusion::prelude::SessionContext;
use iceblink_testkit::TableFixture;
use url::Url;

use super::*;
use crate::io::tests::Spy;

/// The `ScanError` inside a DataFusion error, if there is one.
fn scan_error(error: &DataFusionError) -> Option<&ScanError> {
    match error.find_root() {
        DataFusionError::External(inner) => inner.downcast_ref::<ScanError>(),
        _ => None,
    }
}

#[tokio::test]
async fn a_table_outside_the_prefixes_fails_before_any_read() -> Result<(), Box<dyn Error>> {
    let fixture = TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(2)
        .rows_per_file(10)
        .build()
        .await?;
    let settings = StorageSettings {
        endpoint: None,
        region: "us-east-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse("s3://elsewhere/")?],
        allow_local_files: true,
        aws_profile: None,
    };
    let spy = Spy::default();
    let provider = LakeTableProvider::with_inner_storage(
        fixture.load_table().await?,
        &settings,
        Arc::new(spy.clone()),
    )?;

    let state = SessionContext::new().state();
    let error = provider.scan(&state, None, &[], None).await.err();
    let scan = error.as_ref().and_then(scan_error);
    assert!(
        matches!(scan, Some(ScanError::LocationNotAllowed { path_prefix }) if path_prefix.starts_with("file:///")),
        "{error:?}"
    );
    assert_eq!(spy.call_count(), 0, "nothing may reach storage");
    Ok(())
}
