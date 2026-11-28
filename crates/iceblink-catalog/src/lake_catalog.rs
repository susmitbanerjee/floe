//! `LakeCatalog`: the small, read-only face of an Iceberg REST catalog.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

use iceberg::table::Table;
use iceberg::{Catalog, CatalogBuilder, Runtime};
use iceberg_catalog_rest::{
    REST_CATALOG_PROP_URI, REST_CATALOG_PROP_WAREHOUSE, RestCatalogBuilder,
};
use secrecy::SecretString;
use url::Url;

use crate::error::{Call, Context, Subject, is_retryable, map_error};
use crate::http::{BearerAuth, USER_AGENT, build_client};
use crate::retry::retry;
use crate::sigv4::{AwsSigV4Signer, SigV4Auth};
use crate::storage::MetadataOnlyStorage;
use crate::{CatalogError, CatalogSettings, NamespaceName, TableName};

/// A connected Iceberg REST catalog that Iceblink can only read: list namespaces, list tables, and
/// load a table's metadata. It has no method that creates, updates, renames or drops anything.
pub struct LakeCatalog {
    inner: Arc<dyn Catalog>,
    /// The catalog URI without credentials or query string, safe to show in errors.
    uri: String,
    /// Kept only to scrub it from anything the catalog says back.
    credential: Option<SecretString>,
    /// Whether requests are SigV4-signed.
    signed: bool,
}

impl LakeCatalog {
    /// Connects to the REST catalog in `settings` and checks that it answers. A credential is
    /// sent as a bearer token; `sigv4` signs every request with AWS credentials from the
    /// standard chain (environment, profile, SSO, web identity, container or instance role).
    ///
    /// iceberg's REST client does its `/v1/config` handshake lazily, on the first request, so
    /// `connect` lists namespaces once: a wrong URI or credential fails here, not on first use.
    ///
    /// # Errors
    ///
    /// [`CatalogError::NoAwsCredentials`] if SigV4 is on and no AWS credentials can be found,
    /// [`CatalogError::Unsupported`] if both a credential and SigV4 are set, and otherwise
    /// whatever the handshake fails with: unreachable, timed out, unauthorized, or an
    /// unexpected answer. `connect` isn't retried.
    pub async fn connect(settings: CatalogSettings) -> Result<Self, CatalogError> {
        let builder = RestCatalogBuilder::default()
            .with_client(build_client(&settings.timeouts)?)
            .with_storage_factory(Arc::new(MetadataOnlyStorage));
        let builder = with_auth(builder, &settings).await?;
        let uri = shown_uri(&settings.uri);
        let signed = settings.sigv4.is_some();
        let call = Call {
            operation: "connect",
            subject: Subject::Catalog,
        };
        let context = Context {
            uri: &uri,
            credential: settings.credential.as_ref(),
            signed,
        };
        let rest = builder
            .load("iceblink", properties(&settings))
            .await
            .map_err(|error| map_error(&error, &call, &context))?;
        let catalog = Self {
            inner: Arc::new(rest),
            uri,
            credential: settings.credential,
            signed,
        };
        catalog
            .read(&call, |_| false, || catalog.inner.list_namespaces(None))
            .await?;
        Ok(catalog)
    }

    /// Wraps any iceberg catalog, for tests and fixtures. Errors name it as "the catalog".
    pub fn from_catalog(inner: Arc<dyn Catalog>) -> Self {
        Self {
            inner,
            uri: "the catalog".to_owned(),
            credential: None,
            signed: false,
        }
    }

    /// Lists the catalog's top-level namespaces.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Unsupported`] if the catalog returns a nested namespace, and the usual
    /// call failures after up to three attempts.
    pub async fn list_namespaces(&self) -> Result<Vec<NamespaceName>, CatalogError> {
        let call = Call {
            operation: "list_namespaces",
            subject: Subject::Catalog,
        };
        let idents = self
            .read(&call, is_retryable, || self.inner.list_namespaces(None))
            .await?;
        idents.iter().map(NamespaceName::from_ident).collect()
    }

    /// Lists the tables in `namespace`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::NotFound`] if the namespace doesn't exist, and the usual call failures
    /// after up to three attempts.
    pub async fn list_tables(
        &self,
        namespace: &NamespaceName,
    ) -> Result<Vec<TableName>, CatalogError> {
        let call = Call {
            operation: "list_tables",
            subject: Subject::Namespace(namespace),
        };
        let ident = namespace.to_path_ident();
        let idents = self
            .read(&call, is_retryable, || self.inner.list_tables(&ident))
            .await?;
        idents.iter().map(TableName::from_ident).collect()
    }

    /// Loads `table`'s current metadata. The returned table's file IO refuses every operation
    /// until iceblink-scan attaches storage.
    ///
    /// # Errors
    ///
    /// [`CatalogError::NotFound`] if the table doesn't exist, and the usual call failures after
    /// up to three attempts.
    pub async fn load_table(&self, table: &TableName) -> Result<Table, CatalogError> {
        let call = Call {
            operation: "load_table",
            subject: Subject::Table(table),
        };
        // The request carries the percent-encoded name; the returned table carries the real one.
        let ident = table.to_path_ident();
        let loaded = self
            .read(&call, is_retryable, || self.inner.load_table(&ident))
            .await?;
        with_identity(&loaded, table).map_err(|error| map_error(&error, &call, &self.context()))
    }

    fn context(&self) -> Context<'_> {
        Context {
            uri: &self.uri,
            credential: self.credential.as_ref(),
            signed: self.signed,
        }
    }

    /// Runs one call through the retry helper and maps a final error.
    async fn read<T, F, Fut>(
        &self,
        call: &Call<'_>,
        retryable: fn(&iceberg::Error) -> bool,
        attempt: F,
    ) -> Result<T, CatalogError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = iceberg::Result<T>>,
    {
        retry(call, retryable, attempt)
            .await
            .map_err(|error| map_error(&error, call, &self.context()))
    }
}

/// The REST catalog's properties: the URI (without a trailing slash), the warehouse, and Iceblink's
/// user agent, which overrides the `iceberg-rs/…` one iceberg sets on every request.
fn properties(settings: &CatalogSettings) -> HashMap<String, String> {
    let base = settings.uri.as_str().trim_end_matches('/').to_owned();
    HashMap::from([
        (REST_CATALOG_PROP_URI.to_owned(), base),
        (
            REST_CATALOG_PROP_WAREHOUSE.to_owned(),
            settings.warehouse.clone(),
        ),
        ("header.user-agent".to_owned(), USER_AGENT.to_owned()),
    ])
}

/// Adds bearer or SigV4 auth. SigV4 credentials are resolved once here, so a missing AWS
/// identity fails at connect rather than on the first request.
async fn with_auth(
    builder: RestCatalogBuilder,
    settings: &CatalogSettings,
) -> Result<RestCatalogBuilder, CatalogError> {
    match (&settings.credential, &settings.sigv4) {
        (Some(_), Some(_)) => Err(CatalogError::Unsupported {
            feature: "a bearer credential and SigV4 signing together".to_owned(),
        }),
        (Some(token), None) => Ok(builder.with_auth_manager(BearerAuth::new(token.clone()))),
        (None, Some(sigv4)) => {
            let signer = AwsSigV4Signer::from_environment(sigv4, &settings.timeouts).await?;
            if let Err(error) = signer.credentials().await {
                tracing::debug!(%error, "the AWS credential chain found nothing");
                return Err(CatalogError::NoAwsCredentials);
            }
            Ok(builder.with_auth_manager(SigV4Auth::new(Arc::new(signer))))
        }
        (None, None) => Ok(builder),
    }
}

/// Rebuilds a loaded table under its real, unencoded identifier: iceberg names it after the
/// identifier it was asked for, which carried the percent-encoded name.
fn with_identity(loaded: &Table, table: &TableName) -> iceberg::Result<Table> {
    let mut builder = Table::builder()
        .identifier(table.to_ident())
        .metadata(loaded.metadata_ref())
        .file_io(loaded.file_io().clone())
        .readonly(loaded.readonly())
        .runtime(Runtime::current());
    if let Some(location) = loaded.metadata_location() {
        builder = builder.metadata_location(location);
    }
    builder.build()
}

impl fmt::Debug for LakeCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LakeCatalog")
            .field("uri", &self.uri)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
}

/// The URI as errors show it: no user name, password, query or fragment.
fn shown_uri(uri: &Url) -> String {
    let mut shown = uri.clone();
    // Both setters only fail for URLs that can't have credentials, which already have none.
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.set_query(None);
    shown.set_fragment(None);
    shown.as_str().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_uri_drops_credentials_and_query() -> Result<(), url::ParseError> {
        let uri = Url::parse("https://user:pass@catalog.example.com/iceberg/?token=abc#frag")?;
        assert_eq!(shown_uri(&uri), "https://catalog.example.com/iceberg");
        Ok(())
    }
}
