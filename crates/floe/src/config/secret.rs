//! `env:NAME` references to secrets. The file names an environment variable; the value lives
//! only in a `SecretString`, never in the file, a log line, Debug output or an error message.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

use super::ConfigError;

/// What floe.toml says for a secret, kept secret until checked: the user may have pasted the
/// secret itself instead of `env:NAME`.
#[derive(Debug)]
pub(super) struct SecretReference(SecretString);

impl SecretReference {
    /// Returns NAME from `env:NAME`. Refuses a plain value, or a NAME that isn't an uppercase
    /// variable name, without repeating it: a lowercase "name" is often a pasted token.
    pub(super) fn variable_name(&self, key: &'static str) -> Result<&str, ConfigError> {
        let Some(name) = self.0.expose_secret().strip_prefix("env:") else {
            return Err(ConfigError::PlainSecret { key });
        };
        if is_variable_name(name) {
            Ok(name)
        } else {
            Err(ConfigError::InvalidSecretReference { key })
        }
    }

    /// Reads the variable through `lookup`. An unset or empty variable is refused by name.
    pub(super) fn resolve(
        &self,
        key: &'static str,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<SecretString, ConfigError> {
        let name = self.variable_name(key)?;
        match lookup(name) {
            Some(value) if !value.is_empty() => Ok(SecretString::from(value)),
            _ => Err(ConfigError::MissingSecret {
                key,
                variable: name.to_owned(),
            }),
        }
    }
}

fn is_variable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_uppercase() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Shown for any non-string value. Serde's default would quote the value, which may be the secret.
const NOT_A_STRING: &str = "catalog.credential must be a string like \"env:NAME\"";

impl<'de> Deserialize<'de> for SecretReference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ReferenceVisitor)
    }
}

struct ReferenceVisitor;

impl<'de> Visitor<'de> for ReferenceVisitor {
    type Value = SecretReference;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string like \"env:NAME\"")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(SecretReference(SecretString::from(value)))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(SecretReference(SecretString::from(value)))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Err(E::custom(NOT_A_STRING))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Err(E::custom(NOT_A_STRING))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Err(E::custom(NOT_A_STRING))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Err(E::custom(NOT_A_STRING))
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Self::Value, E> {
        Err(E::custom(NOT_A_STRING))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
        Err(de::Error::custom(NOT_A_STRING))
    }

    fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<Self::Value, A::Error> {
        Err(de::Error::custom(NOT_A_STRING))
    }
}
