//! The HTTP client Floe hands to iceberg's REST catalog, and the static bearer-token auth.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use iceberg::ErrorKind;
use iceberg_catalog_rest::{AuthManager, AuthSession, HttpClient, HttpRequest};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use secrecy::{ExposeSecret, SecretString};

use crate::{CatalogError, CatalogTimeouts};

/// Sent with every catalog request, as `User-Agent`.
pub(crate) const USER_AGENT: &str = concat!("floe/", env!("CARGO_PKG_VERSION"));

/// Builds the client every catalog request goes through: rustls with compiled-in roots, a
/// connect timeout and a total per-request timeout, Floe's user agent, and no redirects, so an
/// untrusted catalog can't bounce the credential to another host.
///
/// The webpki roots are named explicitly, so a dependency that turns on reqwest's
/// native-roots feature can't widen what Floe trusts (docs/adr/0009-s3-reads.md).
pub(crate) fn build_client(timeouts: &CatalogTimeouts) -> Result<reqwest::Client, CatalogError> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .tls_built_in_webpki_certs(true)
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.total)
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| CatalogError::Protocol {
            message: format!("could not set up the HTTP client: {error}"),
        })
}

/// Sends `catalog.credential` as `Authorization: Bearer …` on every catalog request.
pub(crate) struct BearerAuth {
    token: SecretString,
}

impl BearerAuth {
    pub(crate) fn new(token: SecretString) -> Self {
        Self { token }
    }

    fn session(&self) -> iceberg::Result<BearerSession> {
        let mut header = HeaderValue::from_str(&format!("Bearer {}", self.token.expose_secret()))
            .map_err(|_| {
            iceberg::Error::new(
                ErrorKind::DataInvalid,
                "catalog.credential contains characters that can't go in an HTTP header",
            )
        })?;
        // Keeps the value out of reqwest's and hyper's Debug output.
        header.set_sensitive(true);
        Ok(BearerSession { header })
    }
}

impl fmt::Debug for BearerAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerAuth([REDACTED])")
    }
}

#[async_trait]
impl AuthManager for BearerAuth {
    async fn init_session(
        &self,
        _client: &HttpClient,
        _props: &HashMap<String, String>,
    ) -> iceberg::Result<Box<dyn AuthSession>> {
        Ok(Box::new(self.session()?))
    }

    async fn catalog_session(
        &self,
        _client: &HttpClient,
        _props: &HashMap<String, String>,
    ) -> iceberg::Result<Arc<dyn AuthSession>> {
        Ok(Arc::new(self.session()?))
    }
}

struct BearerSession {
    header: HeaderValue,
}

impl fmt::Debug for BearerSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerSession([REDACTED])")
    }
}

#[async_trait]
impl AuthSession for BearerSession {
    async fn authenticate(&self, request: &mut HttpRequest) -> iceberg::Result<()> {
        request
            .headers_mut()
            .insert(AUTHORIZATION, self.header.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "s3cr3t-catalog-token";

    #[test]
    fn debug_never_shows_the_token() -> Result<(), iceberg::Error> {
        let auth = BearerAuth::new(SecretString::from(SECRET));
        let session = auth.session()?;
        let shown = format!("{auth:?} {session:?} {:?}", session.header);
        assert!(!shown.contains(SECRET), "{shown}");
        Ok(())
    }

    #[test]
    fn refuses_tokens_that_cannot_be_a_header() {
        let auth = BearerAuth::new(SecretString::from("line\nbreak"));
        let error = auth.session().err().map(|error| error.to_string());
        assert!(
            error
                .as_deref()
                .is_some_and(|message| message.contains("can't go in an HTTP header")),
            "{error:?}"
        );
    }
}
