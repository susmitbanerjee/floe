//! `LakeCatalog` against a mocked Iceberg REST API. Hermetic: wiremock on 127.0.0.1, no real
//! waiting (paused time where a test would otherwise sleep).

use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use floe_catalog::{
    CatalogError, CatalogSettings, CatalogTimeouts, LakeCatalog, NamespaceName, TableName,
};
use iceberg::CatalogBuilder;
use iceberg_catalog_rest::{REST_CATALOG_PROP_URI, RestCatalogBuilder};
use secrecy::SecretString;
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = Result<(), Box<dyn Error>>;

const SECRET: &str = "s3cr3t-catalog-token";
const USER_AGENT: &str = concat!("floe/", env!("CARGO_PKG_VERSION"));
const NAMESPACES: &str = r#"{"namespaces": [["sales"], ["marketing"]]}"#;
const TABLES: &str = r#"{"identifiers": [{"namespace": ["sales"], "name": "orders"}]}"#;
const METADATA: &str = include_str!("fixtures/orders.metadata.json");
const METADATA_LOCATION: &str = "s3://warehouse/sales/orders/metadata/00001.metadata.json";

fn json(body: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.into(), "application/json")
}

fn load_table_body() -> String {
    format!(
        r#"{{"metadata-location": "{METADATA_LOCATION}", "metadata": {METADATA}, "config": {{}}}}"#
    )
}

/// The `/v1/config` handshake and a namespace listing: what `connect` needs.
async fn mount_catalog(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .and(query_param("warehouse", "demo"))
        .respond_with(json(r#"{"defaults": {}, "overrides": {}}"#))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .respond_with(json(NAMESPACES))
        .mount(server)
        .await;
}

fn settings(uri: &str, credential: Option<&str>) -> Result<CatalogSettings, url::ParseError> {
    Ok(CatalogSettings {
        uri: Url::parse(uri)?,
        warehouse: "demo".to_owned(),
        credential: credential.map(SecretString::from),
        sigv4: None,
        timeouts: CatalogTimeouts::default(),
    })
}

fn orders() -> Result<TableName, CatalogError> {
    TableName::new(NamespaceName::new("sales")?, "orders")
}

#[tokio::test]
async fn connects_lists_and_loads() -> TestResult {
    let server = MockServer::start().await;
    // Every request must carry Floe's user agent, not iceberg-rs's.
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .and(header("user-agent", USER_AGENT))
        .respond_with(json(r#"{"defaults": {}, "overrides": {}}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .and(header("user-agent", USER_AGENT))
        .respond_with(json(NAMESPACES))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables"))
        .respond_with(json(TABLES))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables/orders"))
        .respond_with(json(load_table_body()))
        .mount(&server)
        .await;

    let catalog = LakeCatalog::connect(settings(&server.uri(), None)?).await?;
    let namespaces = catalog.list_namespaces().await?;
    let names: Vec<&str> = namespaces.iter().map(NamespaceName::as_str).collect();
    assert_eq!(names, ["sales", "marketing"]);

    let tables = catalog.list_tables(&NamespaceName::new("sales")?).await?;
    assert_eq!(tables, [orders()?]);

    let table = catalog.load_table(&orders()?).await?;
    assert_eq!(table.identifier().name(), "orders");
    assert_eq!(table.metadata_location(), Some(METADATA_LOCATION));
    assert_eq!(
        table.metadata().current_schema().as_struct().fields().len(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn unauthorized_points_at_the_credential() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"error": {"message": "invalid token", "type": "NotAuthorizedException", "code": 401}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let error = LakeCatalog::connect(settings(&server.uri(), Some(SECRET))?)
        .await
        .err();
    assert!(
        matches!(error, Some(CatalogError::Unauthorized { .. })),
        "{error:?}"
    );
    let fix = error.map(|error| error.fix()).unwrap_or_default();
    assert!(fix.contains("catalog.credential"), "{fix}");
    Ok(())
}

#[tokio::test]
async fn missing_table_is_not_found() -> TestResult {
    let server = MockServer::start().await;
    mount_catalog(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables/orders"))
        .respond_with(ResponseTemplate::new(404).set_body_raw(
            r#"{"error": {"message": "no table", "type": "NoSuchTableException", "code": 404}}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let catalog = LakeCatalog::connect(settings(&server.uri(), None)?).await?;
    let error = catalog.load_table(&orders()?).await.err();
    assert!(
        matches!(&error, Some(CatalogError::NotFound { what }) if what == "table sales.orders"),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retries_a_503_once_then_succeeds() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(json(r#"{"defaults": {}, "overrides": {}}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .respond_with(json(NAMESPACES))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    // A REST catalog on reqwest's default client, which has no request timeout: with the clock
    // paused, a pending timeout would fire while the request waits on the socket. The 200 ms
    // backoff then passes in paused time, so nothing really sleeps.
    let rest = RestCatalogBuilder::default()
        .load(
            "test",
            HashMap::from([(REST_CATALOG_PROP_URI.to_owned(), server.uri())]),
        )
        .await?;
    let catalog = LakeCatalog::from_catalog(Arc::new(rest));
    let namespaces = catalog.list_namespaces().await?;
    assert_eq!(namespaces.len(), 2);
    Ok(())
}

#[tokio::test]
async fn slow_answers_time_out() -> TestResult {
    let server = MockServer::start().await;
    mount_catalog(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables/orders"))
        .respond_with(json(load_table_body()).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let catalog = LakeCatalog::connect(settings(&server.uri(), None)?).await?;
    // From here the clock is paused: each attempt's 30 s timeout fires as soon as the runtime
    // is idle, and the backoffs pass instantly.
    tokio::time::pause();
    let error = catalog.load_table(&orders()?).await.err();
    assert!(
        matches!(
            error,
            Some(CatalogError::Timeout {
                operation: "load_table"
            })
        ),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn nested_namespaces_are_unsupported() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(json(r#"{"defaults": {}, "overrides": {}}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .respond_with(json(r#"{"namespaces": [["sales"], ["sales", "eu"]]}"#))
        .mount(&server)
        .await;

    let catalog = LakeCatalog::connect(settings(&server.uri(), None)?).await?;
    let error = catalog.list_namespaces().await.err();
    assert!(
        matches!(&error, Some(CatalogError::Unsupported { feature }) if feature.contains("sales.eu")),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn refused_connection_is_unreachable() -> TestResult {
    let address = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let uri = format!("http://{address}/catalog");
    let error = LakeCatalog::connect(settings(&uri, None)?).await.err();
    assert!(
        matches!(&error, Some(CatalogError::Unreachable { uri: shown }) if shown == &uri),
        "{error:?}"
    );
    Ok(())
}

/// Collects everything a `fmt` subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
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

#[tokio::test]
async fn the_credential_never_reaches_logs_debug_or_errors() -> TestResult {
    // Global, so reqwest's `log` records are bridged in too. nextest runs each test in its own
    // process, so this subscriber sees only this test.
    let capture = Capture::default();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;

    let server = MockServer::start().await;
    let bearer = format!("Bearer {SECRET}");
    // Only requests carrying the token match, so connect succeeding proves it was sent.
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(json(r#"{"defaults": {}, "overrides": {}}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(json(NAMESPACES))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces/sales/tables/orders"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let https_free = settings(&server.uri(), Some(SECRET))?;
    let catalog = LakeCatalog::connect(https_free).await?;
    catalog.list_namespaces().await?;
    let error = catalog.load_table(&orders()?).await.err();
    assert!(
        matches!(error, Some(CatalogError::Unauthorized { .. })),
        "{error:?}"
    );

    let logs = capture.text();
    assert!(
        logs.contains("catalog call"),
        "the capture saw no catalog events: {logs}"
    );
    let shown = format!(
        "{logs}\n{catalog:?}\n{error:?}\n{}",
        error.as_ref().map(ToString::to_string).unwrap_or_default()
    );
    assert!(!shown.contains(SECRET), "the credential leaked:\n{shown}");
    Ok(())
}
