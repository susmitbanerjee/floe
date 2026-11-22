//! SigV4: known answers from an independent implementation, double encoding, temporary
//! credentials, the credential cache, and log hygiene.
//!
//! The expected signatures come from curl 8.7.1's own SigV4 code. That method reproduces AWS's
//! published `get-vanilla` vector exactly (signature `5fa00fa3…3fbf31`) with
//! `--aws-sigv4 "aws:amz:us-east-1:service"`, user `AKIDEXAMPLE:wJalr…EXAMPLEKEY`,
//! `-H "X-Amz-Date: 20150830T123600Z" -H "User-Agent:" -H "Accept:"`. For the Glue requests below:
//!
//! ```text
//! nc -l 127.0.0.1 48124 &
//! curl --aws-sigv4 "aws:amz:ap-south-1:glue" \
//!   --user "AKIDEXAMPLE:wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY" \
//!   -H "X-Amz-Date: 20150830T123600Z" \
//!   -H "x-amz-content-sha256: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" \
//!   -H "User-Agent:" -H "Accept:" \
//!   --connect-to glue.ap-south-1.amazonaws.com:80:127.0.0.1:48124 \
//!   "http://glue.ap-south-1.amazonaws.com/iceberg/v1/config?warehouse=123456789012"
//! ```
//!
//! and read the `Authorization` line `nc` prints. The session-token case adds
//! `-H "x-amz-security-token: AQoDYXdzEPT//////////wEXAMPLEtoken"`. The double-encoding case
//! passes `--path-as-is` and the already double-encoded path
//! `/iceberg/v1/catalogs/123456789012/namespaces/sales/tables/my%2520table%2525`, since SigV4's
//! canonical request encodes the wire path `my%20table%25` once more.

use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, UNIX_EPOCH};

use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider, future};
use iceberg::CatalogBuilder;
use iceberg_catalog_rest::{REST_CATALOG_PROP_URI, RestCatalogBuilder};
use reqwest::{Method, Request};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::storage::MetadataOnlyStorage;
use crate::{LakeCatalog, NamespaceName, TableName};

type TestResult = Result<(), Box<dyn Error>>;

const ACCESS_KEY: &str = "AKIDEXAMPLE";
const SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const SESSION_TOKEN: &str = "AQoDYXdzEPT//////////wEXAMPLEtoken";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const GLUE_CONFIG: &str =
    "https://glue.ap-south-1.amazonaws.com/iceberg/v1/config?warehouse=123456789012";
const CREDENTIAL_SCOPE: &str = "Credential=AKIDEXAMPLE/20150830/ap-south-1/glue/aws4_request";

/// 2015-08-30T12:36:00Z, the timestamp of AWS's published SigV4 test suite.
fn signed_at() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_440_938_160)
}

fn long_term() -> Credentials {
    Credentials::new(ACCESS_KEY, SECRET_KEY, None, None, "test")
}

fn temporary() -> Credentials {
    Credentials::new(
        ACCESS_KEY,
        SECRET_KEY,
        Some(SESSION_TOKEN.to_owned()),
        None,
        "test",
    )
}

fn glue_signer(credentials: Credentials) -> AwsSigV4Signer {
    AwsSigV4Signer::new(
        "ap-south-1".to_owned(),
        SigV4Service::Glue,
        SharedCredentialsProvider::new(credentials),
    )
}

fn signed_get(url: &str, credentials: &Credentials) -> Result<HttpRequest, Box<dyn Error>> {
    let mut request = HttpRequest::new(Request::new(Method::GET, Url::parse(url)?));
    glue_signer(credentials.clone()).sign(&mut request, credentials, signed_at())?;
    Ok(request)
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

#[test]
fn signs_a_glue_get_like_curl_does() -> TestResult {
    let request = signed_get(GLUE_CONFIG, &long_term())?;
    let expected = format!(
        "AWS4-HMAC-SHA256 {CREDENTIAL_SCOPE}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
         Signature=78241ba33fff1530edc1a01b56d94486a049c8b9e29306b1f21fa797947f29ed"
    );
    assert_eq!(header(&request, "authorization"), Some(expected.as_str()));
    assert_eq!(header(&request, "x-amz-date"), Some("20150830T123600Z"));
    assert_eq!(header(&request, "x-amz-content-sha256"), Some(EMPTY_SHA256));
    assert_eq!(
        header(&request, "x-amz-security-token"),
        None,
        "long-term keys have no token"
    );
    Ok(())
}

#[test]
fn temporary_credentials_add_and_sign_the_security_token() -> TestResult {
    let request = signed_get(GLUE_CONFIG, &temporary())?;
    let expected = format!(
        "AWS4-HMAC-SHA256 {CREDENTIAL_SCOPE}, \
         SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, \
         Signature=2ad9d52d7f743e277a3c854a7f62ba13f40763b6eeb4e6ee575b033f586ae12c"
    );
    assert_eq!(header(&request, "authorization"), Some(expected.as_str()));
    assert_eq!(
        header(&request, "x-amz-security-token"),
        Some(SESSION_TOKEN)
    );
    Ok(())
}

#[test]
fn double_encodes_a_table_name_with_a_space_and_a_percent() -> TestResult {
    let url = "https://glue.ap-south-1.amazonaws.com/iceberg/v1/catalogs/123456789012/namespaces/\
               sales/tables/my%20table%25";
    let request = signed_get(url, &long_term())?;
    let signature = header(&request, "authorization")
        .and_then(|value| value.rsplit_once("Signature="))
        .map(|(_, signature)| signature);
    assert_eq!(
        signature,
        Some("231e89fa31d0107e124f546e7f74db87e5d23386c31571f93f25614feb696a42")
    );
    Ok(())
}

#[test]
fn debug_shows_region_and_service_only() {
    let shown = format!("{:?}", glue_signer(temporary()));
    assert!(
        shown.contains("ap-south-1") && shown.contains("Glue"),
        "{shown}"
    );
    for secret in [ACCESS_KEY, SECRET_KEY, SESSION_TOKEN] {
        assert!(!shown.contains(secret), "{shown}");
    }
}

/// Hands out temporary credentials that live for `lifetime`, counting each call.
#[derive(Debug)]
struct Counting {
    calls: Arc<AtomicUsize>,
    lifetime: Duration,
}

impl ProvideCredentials for Counting {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let expiry = SystemTime::now() + self.lifetime;
        future::ProvideCredentials::ready(Ok(Credentials::new(
            ACCESS_KEY,
            SECRET_KEY,
            Some(SESSION_TOKEN.to_owned()),
            Some(expiry),
            "counting",
        )))
    }
}

#[tokio::test]
async fn caches_credentials_until_they_near_expiry() -> TestResult {
    for (lifetime, expected_calls) in [(3600, 1), (60, 2)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let signer = AwsSigV4Signer::new(
            "ap-south-1".to_owned(),
            SigV4Service::Glue,
            SharedCredentialsProvider::new(Counting {
                calls: Arc::clone(&calls),
                lifetime: Duration::from_secs(lifetime),
            }),
        );
        signer.credentials().await?;
        signer.credentials().await?;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected_calls,
            "credentials living {lifetime}s"
        );
    }
    Ok(())
}

/// Collects everything a `fmt` subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<StdMutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    /// Sends this thread's TRACE events, from every crate, here until the guard drops.
    fn install(&self) -> tracing::subscriber::DefaultGuard {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(self.clone())
            .finish();
        tracing::subscriber::set_default(subscriber)
    }
}

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("capture lock poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn signing_logs_no_key_token_or_signature() -> TestResult {
    let capture = Capture::default();
    let request = {
        let _guard = capture.install();
        signed_get(GLUE_CONFIG, &temporary())?
    };
    let logs = capture.text();
    assert!(
        logs.contains("signing request"),
        "aws-sigv4's events weren't captured: {logs}"
    );
    let signature = header(&request, "authorization")
        .and_then(|value| value.rsplit_once("Signature="))
        .map(|(_, signature)| signature.to_owned())
        .unwrap_or_default();
    assert!(!signature.is_empty());
    for secret in [SECRET_KEY, SESSION_TOKEN, signature.as_str()] {
        assert!(!logs.contains(secret), "a secret reached the logs:\n{logs}");
    }
    Ok(())
}

#[tokio::test]
async fn odd_names_reach_the_wire_encoded_and_sign_double_encoded() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .and(header_exists("authorization"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"defaults": {}, "overrides": {}}"#, "application/json"),
        )
        .mount(&server)
        .await;
    let metadata = include_str!("../../tests/fixtures/orders.metadata.json");
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables/my%20table%25"))
        .and(header_exists("authorization"))
        .and(header_exists("x-amz-content-sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(r#"{{"metadata-location": "s3://warehouse/m.json", "metadata": {metadata}}}"#),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let signer = Arc::new(glue_signer(long_term()));
    let rest = RestCatalogBuilder::default()
        .with_auth_manager(SigV4Auth::new(signer))
        .with_storage_factory(Arc::new(MetadataOnlyStorage))
        .load(
            "test",
            HashMap::from([(REST_CATALOG_PROP_URI.to_owned(), server.uri())]),
        )
        .await?;
    let catalog = LakeCatalog::from_catalog(Arc::new(rest));
    let name = TableName::new(NamespaceName::new("sales")?, "my table%")?;

    let capture = Capture::default();
    let table = {
        let _guard = capture.install();
        catalog.load_table(&name).await?
    };
    assert_eq!(
        table.identifier().name(),
        "my table%",
        "the real name comes back"
    );
    let logs = capture.text();
    assert!(
        logs.contains("/v1/namespaces/sales/tables/my%2520table%2525"),
        "the canonical request didn't double-encode the path:\n{logs}"
    );
    Ok(())
}
