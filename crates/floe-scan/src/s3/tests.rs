//! `S3Storage` against wiremock standing in for a path-style S3 endpoint such as MinIO.

use std::error::Error;

use aws_credential_types::provider::SharedCredentialsProvider;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

const FILE: &str = "s3://warehouse/sales/orders/data/part-0.parquet";
const KEY_PATH: &str = "/warehouse/sales/orders/data/part-0.parquet";
const BODY: &[u8] = b"0123456789";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

fn settings(endpoint: Option<&str>, path_style: bool) -> S3Settings {
    S3Settings {
        endpoint: endpoint.map(str::to_owned),
        region: "us-east-1".to_owned(),
        path_style,
        profile: None,
    }
}

#[test]
fn reads_with_the_aws_profile_from_the_storage_settings() -> Result<(), url::ParseError> {
    let storage = StorageSettings {
        endpoint: None,
        region: "ap-south-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse("s3://lake/")?],
        allow_local_files: false,
        aws_profile: Some("analytics".to_owned()),
    };
    assert_eq!(
        S3Settings::from(&storage).profile.as_deref(),
        Some("analytics")
    );
    Ok(())
}

fn storage(server: &MockServer) -> S3Storage {
    let credentials = Credentials::new("AKIDEXAMPLE", SECRET_KEY, None, None, "test");
    S3Storage::with_credentials(
        settings(Some(&server.uri()), true),
        AwsCredentials::new(SharedCredentialsProvider::new(credentials)),
    )
}

#[tokio::test]
async fn reads_sizes_ranges_and_whole_objects_with_signed_requests() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path(KEY_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(KEY_PATH))
        .and(header("range", "bytes=2-5"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(&BODY[2..6]))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(KEY_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .with_priority(2)
        .mount(&server)
        .await;

    let storage = storage(&server);
    assert_eq!(storage.metadata(FILE).await?.size, 10);
    assert!(storage.exists(FILE).await?);
    let reader = storage.reader(FILE).await?;
    assert_eq!(reader.read(2..6).await?.as_ref(), b"2345");
    assert!(reader.read(4..4).await?.is_empty());
    assert_eq!(storage.read(FILE).await?.as_ref(), BODY);

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 4, "{requests:?}");
    for request in &requests {
        let shown = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let authorization = shown("authorization");
        assert!(
            authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/")
                && authorization.contains("/us-east-1/s3/aws4_request"),
            "badly signed: {authorization}"
        );
        assert_eq!(shown("x-amz-content-sha256"), "UNSIGNED-PAYLOAD");
        assert!(!shown("x-amz-date").is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn a_store_that_ignores_ranges_still_gives_the_range() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;
    let reader = storage(&server).reader(FILE).await?;
    assert_eq!(reader.read(7..12).await?.as_ref(), b"789");
    Ok(())
}

#[tokio::test]
async fn a_missing_object_does_not_exist_and_a_refusal_names_the_file() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(
            "<Error><Code>AccessDenied</Code></Error>",
            "application/xml",
        ))
        .mount(&server)
        .await;

    let storage = storage(&server);
    assert!(!storage.exists(FILE).await?);
    let error = storage
        .read(FILE)
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains(FILE), "{error}");
    assert!(error.contains("HTTP 403"), "{error}");
    assert!(!error.contains(SECRET_KEY), "{error}");
    Ok(())
}

#[tokio::test]
async fn server_errors_are_retried_and_refusals_are_not() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .with_priority(2)
        .mount(&server)
        .await;
    let storage = storage(&server);
    assert_eq!(storage.read(FILE).await?.as_ref(), BODY);
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        3
    );

    let refusing = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&refusing)
        .await;
    assert!(self::storage(&refusing).read(FILE).await.is_err());
    assert_eq!(
        refusing.received_requests().await.unwrap_or_default().len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn writes_and_deletes_never_reach_s3() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let storage = storage(&server);
    assert!(storage.write(FILE, Bytes::from_static(BODY)).await.is_err());
    assert!(storage.writer(FILE).await.is_err());
    assert!(storage.delete(FILE).await.is_err());
    assert!(
        storage
            .delete_prefix("s3://warehouse/sales/")
            .await
            .is_err()
    );
    let paths: BoxStream<'static, String> = Box::pin(futures::stream::iter([FILE.to_owned()]));
    assert!(storage.delete_stream(paths).await.is_err());
    assert!(storage.new_output(FILE).is_err());

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.is_empty(), "requests reached S3: {requests:?}");
    Ok(())
}

#[test]
fn builds_each_url_style_and_encodes_keys_once() -> Result<(), String> {
    let key = "sales/orders/data/created_at_day=2026-01-01/a b+c%.parquet";
    let encoded = "sales/orders/data/created_at_day%3D2026-01-01/a%20b%2Bc%25.parquet";
    let cases = [
        (
            settings(None, false),
            format!("https://warehouse.s3.us-east-1.amazonaws.com/{encoded}"),
        ),
        (
            settings(None, true),
            format!("https://s3.us-east-1.amazonaws.com/warehouse/{encoded}"),
        ),
        (
            settings(Some("http://127.0.0.1:9000/"), true),
            format!("http://127.0.0.1:9000/warehouse/{encoded}"),
        ),
        (
            settings(Some("https://minio.example.com:9000/"), false),
            format!("https://warehouse.minio.example.com:9000/{encoded}"),
        ),
    ];
    for (settings, expected) in cases {
        assert_eq!(object_url(&settings, "warehouse", key)?, expected);
    }
    Ok(())
}

#[test]
fn only_s3_locations_with_a_bucket_and_key_are_read() {
    assert!(matches!(
        split(FILE),
        Ok(("warehouse", "sales/orders/data/part-0.parquet"))
    ));
    assert!(matches!(split("s3a://bucket/key"), Ok(("bucket", "key"))));
    for bad in [
        "file:///tmp/x",
        "s3://bucket",
        "s3:///key",
        "gs://bucket/key",
    ] {
        assert!(split(bad).is_err(), "{bad}");
    }
}
