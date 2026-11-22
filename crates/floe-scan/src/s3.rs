//! Reads table files from S3 and S3-compatible stores such as MinIO
//! (docs/adr/0009-s3-reads.md).
//!
//! Floe's own reader: HEAD for sizes, GET (whole or ranged) for bytes, each signed with SigV4.
//! No other request exists in this module, so it can't write; writes, deletes and output files
//! fail here, behind `CountingStorage`'s own refusal. Credentials come from the standard AWS
//! chain on the first read, so startup needs no network. Responses are never parsed beyond
//! their status and headers.

use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
    UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use bytes::Bytes;
use floe_catalog::{AwsCredentials, CatalogError, CatalogTimeouts};
use futures::stream::BoxStream;
use iceberg::ErrorKind;
use iceberg::io::{FileMetadata, FileRead, FileWrite, InputFile, OutputFile, Storage};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{CONTENT_LENGTH, HeaderName, HeaderValue, RANGE};
use reqwest::{Method, Response, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{ScanError, StorageSettings};

#[cfg(test)]
mod tests;

/// Connecting to S3 (or MinIO) gives up after this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// One request, connect to last byte, gives up after this. Statement timeouts cut shorter.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// First try plus two retries, for timeouts, connection failures, 429 and 5xx.
const MAX_ATTEMPTS: u32 = 3;

/// Sent with every S3 request, as `User-Agent`.
const USER_AGENT: &str = concat!("floe/", env!("CARGO_PKG_VERSION"));

/// S3's URI encoding of a key segment: everything but unreserved characters.
const KEY_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Where and how to reach S3, from `[storage]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct S3Settings {
    /// An S3-compatible endpoint such as MinIO, or `None` for AWS S3. Already a checked URL.
    endpoint: Option<String>,
    region: String,
    path_style: bool,
}

impl From<&StorageSettings> for S3Settings {
    fn from(settings: &StorageSettings) -> Self {
        Self {
            endpoint: settings.endpoint.as_ref().map(Url::to_string),
            region: settings.region.clone(),
            path_style: settings.path_style,
        }
    }
}

/// iceberg storage over S3, shared by every table of a catalog. Clones share their client
/// and credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct S3Storage {
    settings: S3Settings,
    /// Built on first use, so a deserialized copy rebuilds them.
    #[serde(skip)]
    state: Arc<OnceLock<S3State>>,
}

#[derive(Debug)]
struct S3State {
    /// `Err` holds why the HTTP client couldn't be built, reported on every read.
    client: Result<reqwest::Client, String>,
    credentials: tokio::sync::OnceCell<AwsCredentials>,
}

/// One object: its location as written, and the URL Floe requests.
#[derive(Debug, Clone)]
struct Object {
    location: String,
    url: String,
}

impl S3Storage {
    pub(crate) fn new(settings: S3Settings) -> Self {
        Self {
            settings,
            state: Arc::default(),
        }
    }

    /// Storage that signs with `credentials` instead of the AWS chain, for tests.
    #[cfg(test)]
    pub(crate) fn with_credentials(settings: S3Settings, credentials: AwsCredentials) -> Self {
        let storage = Self::new(settings);
        let _ = storage.state.set(S3State {
            client: client(),
            credentials: tokio::sync::OnceCell::new_with(Some(credentials)),
        });
        storage
    }

    fn state(&self) -> &S3State {
        self.state.get_or_init(|| S3State {
            client: client(),
            credentials: tokio::sync::OnceCell::new(),
        })
    }

    /// The current credentials, looking up the AWS chain the first time.
    async fn credentials(&self) -> iceberg::Result<Credentials> {
        let no_credentials = |detail: String| {
            iceberg::Error::new(
                ErrorKind::Unexpected,
                format!(
                    "no AWS credentials for S3 ({detail}): {}",
                    CatalogError::NoAwsCredentials.fix()
                ),
            )
        };
        let chain = self
            .state()
            .credentials
            .get_or_try_init(|| async {
                let timeouts = CatalogTimeouts::default();
                AwsCredentials::from_environment(&self.settings.region, None, &timeouts).await
            })
            .await
            .map_err(|error| no_credentials(error.to_string()))?;
        chain
            .credentials()
            .await
            .map_err(|error| no_credentials(error.to_string()))
    }

    fn object(&self, location: &str) -> iceberg::Result<Object> {
        let (bucket, key) = split(location)?;
        let url = object_url(&self.settings, bucket, key).map_err(|message| {
            iceberg::Error::new(ErrorKind::DataInvalid, format!("{location}: {message}"))
        })?;
        Ok(Object {
            location: location.to_owned(),
            url,
        })
    }

    /// Sends one signed GET or HEAD, retrying timeouts, failed connections, 429 and 5xx.
    async fn send(
        &self,
        method: &Method,
        object: &Object,
        range: Option<&Range<u64>>,
    ) -> iceberg::Result<Response> {
        let client = self
            .state()
            .client
            .as_ref()
            .map_err(|message| read_error(&object.location, message))?;
        let credentials = self.credentials().await?;
        let mut attempt = 1;
        loop {
            let mut request = client
                .request(method.clone(), &object.url)
                .build()
                .map_err(|error| read_error(&object.location, &error))?;
            if let Some(range) = range {
                let value = format!("bytes={}-{}", range.start, range.end.saturating_sub(1));
                let value = HeaderValue::from_str(&value)
                    .map_err(|error| read_error(&object.location, &error))?;
                request.headers_mut().insert(RANGE, value);
            }
            sign_request(&mut request, &credentials, &self.settings.region)
                .map_err(|message| read_error(&object.location, &message))?;
            let retry = attempt < MAX_ATTEMPTS;
            match client.execute(request).await {
                Ok(response) if retry && is_retryable(response.status()) => {}
                Ok(response) => return Ok(response),
                Err(error) if retry && (error.is_timeout() || error.is_connect()) => {}
                Err(error) => return Err(read_error(&object.location, &error)),
            }
            tokio::time::sleep(Duration::from_millis(200 << (attempt - 1))).await;
            attempt += 1;
        }
    }

    async fn head(&self, location: &str) -> iceberg::Result<Response> {
        let object = self.object(location)?;
        self.send(&Method::HEAD, &object, None).await
    }
}

/// The HTTP client every S3 request uses: rustls with only the compiled-in webpki roots, a
/// connect and a total timeout, Floe's user agent, and no redirects.
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .tls_built_in_webpki_certs(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("couldn't set up the HTTP client for S3: {error}"))
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// The URL for `key` in `bucket`: virtual-hosted on AWS unless `path_style`, and on a custom
/// endpoint either `endpoint/bucket/key` (path style, MinIO) or `bucket.endpoint/key`.
fn object_url(settings: &S3Settings, bucket: &str, key: &str) -> Result<String, String> {
    let key = key
        .split('/')
        .map(|segment| utf8_percent_encode(segment, KEY_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/");
    let region = &settings.region;
    Ok(match (&settings.endpoint, settings.path_style) {
        (None, false) => format!("https://{bucket}.s3.{region}.amazonaws.com/{key}"),
        (None, true) => format!("https://s3.{region}.amazonaws.com/{bucket}/{key}"),
        (Some(endpoint), true) => format!("{}/{bucket}/{key}", endpoint.trim_end_matches('/')),
        (Some(endpoint), false) => {
            let endpoint = Url::parse(endpoint).map_err(|error| error.to_string())?;
            let host = endpoint.host_str().ok_or("storage.endpoint has no host")?;
            let port = endpoint
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            format!("{}://{bucket}.{host}{port}/{key}", endpoint.scheme())
        }
    })
}

/// `s3://bucket/key` (or `s3a://`) into the bucket and the raw key.
fn split(location: &str) -> iceberg::Result<(&str, &str)> {
    let invalid = |message: String| ScanError::Io { message }.into_iceberg();
    let rest = location
        .strip_prefix("s3://")
        .or_else(|| location.strip_prefix("s3a://"))
        .ok_or_else(|| invalid(format!("{location} is not an s3:// location")))?;
    match rest.split_once('/') {
        Some((bucket, key)) if !bucket.is_empty() && !key.is_empty() => Ok((bucket, key)),
        _ => Err(invalid(format!("{location} names no bucket and key"))),
    }
}

/// Signs `request` for S3: `host`, `x-amz-date`, `x-amz-content-sha256: UNSIGNED-PAYLOAD`,
/// and the session token for temporary credentials. S3 encodes the path once, unlike other
/// AWS services.
fn sign_request(
    request: &mut reqwest::Request,
    credentials: &Credentials,
    region: &str,
) -> Result<(), String> {
    let identity: Identity = credentials.clone().into();
    let mut settings = SigningSettings::default();
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("s3")
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|error| format!("invalid SigV4 parameters: {error}"))?
        .into();
    let url = request.url();
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => return Err("the S3 URL has no host".to_owned()),
    };
    let signable = SignableRequest::new(
        request.method().as_str(),
        url.as_str(),
        std::iter::once(("host", host.as_str())),
        SignableBody::UnsignedPayload,
    )
    .map_err(|error| format!("can't sign this request: {error}"))?;
    let (instructions, _signature) = sign(signable, &params)
        .map_err(|error| format!("SigV4 signing failed: {error}"))?
        .into_parts();
    for (name, value) in instructions.headers() {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?;
        let mut value = HeaderValue::from_str(value).map_err(|error| error.to_string())?;
        if name == reqwest::header::AUTHORIZATION || name.as_str() == "x-amz-security-token" {
            value.set_sensitive(true);
        }
        request.headers_mut().insert(name, value);
    }
    Ok(())
}

/// A failed read, naming the file. Never carries credentials or response bodies.
fn read_error(location: &str, error: &dyn std::fmt::Display) -> iceberg::Error {
    iceberg::Error::new(
        ErrorKind::Unexpected,
        format!("couldn't read {location} from S3: {error}"),
    )
}

/// What an unexpected status means for `location`, and what to check.
fn status_error(location: &str, status: StatusCode) -> iceberg::Error {
    let code = status.as_u16();
    let message = match code {
        403 => format!(
            "S3 refused access to {location} (HTTP 403): check that Floe's AWS identity may \
             s3:GetObject there"
        ),
        404 => format!("{location} does not exist in S3 (HTTP 404)"),
        301 | 307 | 400 => format!(
            "S3 answered HTTP {code} for {location}: check storage.region and storage.endpoint"
        ),
        _ => format!("S3 answered HTTP {code} for {location}"),
    };
    iceberg::Error::new(ErrorKind::Unexpected, message)
}

fn refused() -> iceberg::Error {
    ScanError::ReadOnly.into_iceberg()
}

#[async_trait]
#[typetag::serde(name = "floe-s3")]
impl Storage for S3Storage {
    async fn exists(&self, path: &str) -> iceberg::Result<bool> {
        let response = self.head(path).await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(false),
            status if status.is_success() => Ok(true),
            status => Err(status_error(path, status)),
        }
    }

    async fn metadata(&self, path: &str) -> iceberg::Result<FileMetadata> {
        let response = self.head(path).await?;
        if !response.status().is_success() {
            return Err(status_error(path, response.status()));
        }
        let size = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| read_error(path, &"the HEAD response has no Content-Length"))?;
        Ok(FileMetadata { size })
    }

    async fn read(&self, path: &str) -> iceberg::Result<Bytes> {
        let object = self.object(path)?;
        let response = self.send(&Method::GET, &object, None).await?;
        if !response.status().is_success() {
            return Err(status_error(path, response.status()));
        }
        response
            .bytes()
            .await
            .map_err(|error| read_error(path, &error))
    }

    async fn reader(&self, path: &str) -> iceberg::Result<Box<dyn FileRead>> {
        Ok(Box::new(S3Read {
            storage: self.clone(),
            object: self.object(path)?,
        }))
    }

    async fn write(&self, _path: &str, _bytes: Bytes) -> iceberg::Result<()> {
        Err(refused())
    }

    async fn writer(&self, _path: &str) -> iceberg::Result<Box<dyn FileWrite>> {
        Err(refused())
    }

    async fn delete(&self, _path: &str) -> iceberg::Result<()> {
        Err(refused())
    }

    async fn delete_prefix(&self, _path: &str) -> iceberg::Result<()> {
        Err(refused())
    }

    async fn delete_stream(&self, _paths: BoxStream<'static, String>) -> iceberg::Result<()> {
        Err(refused())
    }

    fn new_input(&self, path: &str) -> iceberg::Result<InputFile> {
        Ok(InputFile::new(Arc::new(self.clone()), path.to_owned()))
    }

    fn new_output(&self, _path: &str) -> iceberg::Result<OutputFile> {
        Err(refused())
    }
}

/// Ranged reads of one object, one GET each.
struct S3Read {
    storage: S3Storage,
    object: Object,
}

#[async_trait]
impl FileRead for S3Read {
    async fn read(&self, range: Range<u64>) -> iceberg::Result<Bytes> {
        if range.is_empty() {
            return Ok(Bytes::new());
        }
        let location = &self.object.location;
        let response = self
            .storage
            .send(&Method::GET, &self.object, Some(&range))
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(location, status));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| read_error(location, &error))?;
        if status == StatusCode::PARTIAL_CONTENT {
            return Ok(body);
        }
        // A store that ignored the Range header sent the whole object: cut the range out.
        let start = usize::try_from(range.start).unwrap_or(usize::MAX);
        let end = usize::try_from(range.end)
            .unwrap_or(usize::MAX)
            .min(body.len());
        if start > end {
            return Err(read_error(
                location,
                &"the range starts past the end of the object",
            ));
        }
        Ok(body.slice(start..end))
    }
}
