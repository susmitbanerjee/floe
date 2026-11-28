//! AWS Signature Version 4 for Glue and S3 Tables catalogs (docs/adr/0005-sigv4.md).
//!
//! Attaches through iceberg-catalog-rest's `AuthManager`/`AuthSession` hook. It signs `host`,
//! `x-amz-date`, `x-amz-content-sha256` (the real body hash: Glue refuses `UNSIGNED-PAYLOAD`)
//! and, for temporary credentials, `x-amz-security-token`. iceberg adds its own headers after
//! `authenticate`, so they stay outside the signature.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_sigv4::http_request::{
    PayloadChecksumKind, SignableBody, SignableRequest, SigningSettings, sign,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use iceberg::ErrorKind;
use iceberg_catalog_rest::{AuthManager, AuthSession, HttpClient, HttpRequest};
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};
use url::Url;

use crate::{AwsCredentials, CatalogError, CatalogTimeouts, SigV4Service, SigV4Settings};

#[cfg(test)]
mod tests;

const SECURITY_TOKEN: &str = "x-amz-security-token";

/// Signs catalog requests with SigV4, using credentials from the standard AWS chain.
pub(crate) struct AwsSigV4Signer {
    region: String,
    service: SigV4Service,
    credentials: AwsCredentials,
}

impl AwsSigV4Signer {
    /// Builds a signer over aws-config's default chain (see [`AwsCredentials`]).
    ///
    /// # Errors
    ///
    /// [`CatalogError::NoAwsCredentials`] if the chain was turned off and yields no provider.
    pub(crate) async fn from_environment(
        settings: &SigV4Settings,
        timeouts: &CatalogTimeouts,
    ) -> Result<Self, CatalogError> {
        let credentials = AwsCredentials::from_environment(
            &settings.region,
            settings.profile.as_deref(),
            timeouts,
        )
        .await?;
        Ok(Self {
            region: settings.region.clone(),
            service: settings.service,
            credentials,
        })
    }

    /// A signer over fixed credentials, for tests.
    #[cfg(test)]
    pub(crate) fn new(
        region: String,
        service: SigV4Service,
        credentials: aws_credential_types::provider::SharedCredentialsProvider,
    ) -> Self {
        Self {
            region,
            service,
            credentials: AwsCredentials::new(credentials),
        }
    }

    /// The current credentials, cached until they near expiry.
    pub(crate) async fn credentials(&self) -> Result<Credentials, CredentialsError> {
        self.credentials.credentials().await
    }

    /// Signs `request` as of `time`, adding `authorization`, `x-amz-date`,
    /// `x-amz-content-sha256` and, for temporary credentials, `x-amz-security-token`. The path
    /// is percent-encoded a second time for the canonical request, as SigV4 requires for every
    /// service except S3.
    pub(crate) fn sign(
        &self,
        request: &mut HttpRequest,
        credentials: &Credentials,
        time: SystemTime,
    ) -> iceberg::Result<()> {
        let headers = self.signed_headers(request, credentials, time)?;
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| signing_error("aws-sigv4 produced an invalid header name"))?;
            let mut value = HeaderValue::from_str(&value)
                .map_err(|_| signing_error("aws-sigv4 produced an invalid header value"))?;
            if name == AUTHORIZATION || name.as_str() == SECURITY_TOKEN {
                value.set_sensitive(true);
            }
            request.headers_mut().insert(name, value);
        }
        Ok(())
    }

    /// Computes the headers to add, without touching the request.
    fn signed_headers(
        &self,
        request: &HttpRequest,
        credentials: &Credentials,
        time: SystemTime,
    ) -> iceberg::Result<Vec<(String, String)>> {
        let host = authority(request.url_str())?;
        let body = request
            .body()
            .as_bytes()
            .ok_or_else(|| signing_error("a streaming request body can't be signed"))?;
        let identity: Identity = credentials.clone().into();
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(signing_name(self.service))
            .time(time)
            .settings(settings)
            .build()
            .map_err(|error| signing_error(format!("invalid SigV4 parameters: {error}")))?
            .into();
        let signable = SignableRequest::new(
            request.method().as_str(),
            request.url_str(),
            std::iter::once(("host", host.as_str())),
            SignableBody::Bytes(body),
        )
        .map_err(|error| signing_error(format!("can't sign this request: {error}")))?;
        let (instructions, _signature) = sign(signable, &params)
            .map_err(|error| signing_error(format!("SigV4 signing failed: {error}")))?
            .into_parts();
        Ok(instructions
            .headers()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect())
    }
}

impl fmt::Debug for AwsSigV4Signer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AwsSigV4Signer")
            .field("region", &self.region)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

fn signing_name(service: SigV4Service) -> &'static str {
    match service {
        SigV4Service::Glue => "glue",
        SigV4Service::S3Tables => "s3tables",
    }
}

/// The `host` header value the request will carry: host, plus the port when not the default.
fn authority(url: &str) -> iceberg::Result<String> {
    let url = Url::parse(url).map_err(|_| signing_error("the request URL can't be parsed"))?;
    let host = url
        .host_str()
        .ok_or_else(|| signing_error("the request URL has no host"))?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn signing_error(message: impl Into<String>) -> iceberg::Error {
    iceberg::Error::new(ErrorKind::DataInvalid, message)
}

/// Signs every catalog request, including the `/v1/config` handshake.
#[derive(Debug, Clone)]
pub(crate) struct SigV4Auth {
    signer: Arc<AwsSigV4Signer>,
}

impl SigV4Auth {
    pub(crate) fn new(signer: Arc<AwsSigV4Signer>) -> Self {
        Self { signer }
    }
}

#[async_trait]
impl AuthManager for SigV4Auth {
    async fn init_session(
        &self,
        _client: &HttpClient,
        _props: &HashMap<String, String>,
    ) -> iceberg::Result<Box<dyn AuthSession>> {
        Ok(Box::new(self.clone()))
    }

    async fn catalog_session(
        &self,
        _client: &HttpClient,
        _props: &HashMap<String, String>,
    ) -> iceberg::Result<Arc<dyn AuthSession>> {
        Ok(Arc::new(self.clone()))
    }
}

#[async_trait]
impl AuthSession for SigV4Auth {
    async fn authenticate(&self, request: &mut HttpRequest) -> iceberg::Result<()> {
        let credentials = self.signer.credentials().await.map_err(|error| {
            iceberg::Error::new(
                ErrorKind::Unexpected,
                format!("could not get AWS credentials: {error}"),
            )
        })?;
        self.signer.sign(request, &credentials, SystemTime::now())
    }
}
