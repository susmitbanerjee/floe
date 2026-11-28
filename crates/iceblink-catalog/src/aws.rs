//! AWS credentials from the standard chain, cached until they near expiry. The catalog signs
//! with them (SigV4, docs/adr/0005-sigv4.md) and iceblink-scan reads S3 with them
//! (docs/adr/0009-s3-reads.md).

use std::fmt;
use std::time::{Duration, SystemTime};

use aws_config::timeout::TimeoutConfig;
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
use tokio::sync::Mutex;

use crate::{CatalogError, CatalogTimeouts};

/// Credentials expiring within this window are refreshed before they are used.
const REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(5 * 60);

/// Credentials from aws-config's default chain: environment, profile (the given one or
/// `AWS_PROFILE`), SSO, web identity, and container or instance roles.
///
/// The chain itself doesn't cache: AWS SDK clients cache in an `IdentityCache` that can't be
/// used outside them. So this keeps the last credentials until five minutes before they
/// expire, refreshing one caller at a time.
pub struct AwsCredentials {
    provider: SharedCredentialsProvider,
    cached: Mutex<Option<Credentials>>,
}

impl AwsCredentials {
    /// The default chain for `region`, optionally pinned to `profile`. AWS's own HTTP client,
    /// used for SSO and instance roles, runs on rustls with ring and these timeouts.
    ///
    /// # Errors
    ///
    /// [`CatalogError::NoAwsCredentials`] if the chain yields no provider at all.
    pub async fn from_environment(
        region: &str,
        profile: Option<&str>,
        timeouts: &CatalogTimeouts,
    ) -> Result<Self, CatalogError> {
        let http_client = aws_smithy_http_client::Builder::new()
            .tls_provider(Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let timeout = TimeoutConfig::builder()
            .connect_timeout(timeouts.connect)
            .operation_timeout(timeouts.total)
            .build();
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region.to_owned()))
            .http_client(http_client)
            .timeout_config(timeout);
        if let Some(profile) = profile {
            loader = loader.profile_name(profile);
        }
        let provider = loader
            .load()
            .await
            .credentials_provider()
            .ok_or(CatalogError::NoAwsCredentials)?;
        Ok(Self::new(provider))
    }

    /// Wraps any provider, such as fixed test credentials.
    pub fn new(provider: SharedCredentialsProvider) -> Self {
        Self {
            provider,
            cached: Mutex::new(None),
        }
    }

    /// The current credentials: the cached ones while they have more than five minutes left,
    /// otherwise fresh ones from the provider.
    ///
    /// # Errors
    ///
    /// Whatever the provider reports, such as no credentials anywhere in the chain.
    pub async fn credentials(&self) -> Result<Credentials, CredentialsError> {
        let mut cached = self.cached.lock().await;
        if let Some(current) = cached.as_ref().filter(|c| is_fresh(c, SystemTime::now())) {
            return Ok(current.clone());
        }
        let fetched = self.provider.provide_credentials().await?;
        *cached = Some(fetched.clone());
        Ok(fetched)
    }
}

impl fmt::Debug for AwsCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AwsCredentials")
            .finish_non_exhaustive()
    }
}

/// Long-term credentials never expire; temporary ones are fresh until five minutes before.
fn is_fresh(credentials: &Credentials, now: SystemTime) -> bool {
    credentials.expiry().is_none_or(|expiry| {
        expiry
            .duration_since(now)
            .is_ok_and(|left| left > REFRESH_BEFORE_EXPIRY)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_credentials_five_minutes_before_expiry() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        let expiring =
            |after: Duration| Credentials::new("AKID", "secret", None, Some(now + after), "test");
        let long_term = Credentials::new("AKID", "secret", None, None, "test");
        assert!(
            is_fresh(&long_term, now),
            "long-term credentials never expire"
        );
        assert!(is_fresh(&expiring(Duration::from_secs(3600)), now));
        assert!(!is_fresh(&expiring(Duration::from_secs(240)), now));
        let expired = Credentials::new("AKID", "secret", None, Some(now), "test");
        assert!(!is_fresh(&expired, now + Duration::from_secs(1)));
    }
}
