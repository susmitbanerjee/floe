//! Checked names for namespaces and tables. Phase 1 supports single-level namespaces only,
//! because Glue's REST endpoint allows one level.

use std::fmt;

use iceberg::{NamespaceIdent, TableIdent};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::CatalogError;

const MAX_NAME_BYTES: usize = 255;

/// Everything but the unreserved characters of RFC 3986 (`A-Z a-z 0-9 - _ . ~`).
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// iceberg-catalog-rest 0.11 joins names into request paths raw, so a `%`, `/`, `?` or `#` in a
/// name would corrupt the URL. Iceblink encodes each name itself before handing it over.
fn path_segment(name: &str) -> String {
    utf8_percent_encode(name, PATH_SEGMENT).to_string()
}

/// A single-level namespace name such as `sales`: 1 to 255 bytes, no control characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NamespaceName(String);

impl NamespaceName {
    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidName`] if the name is empty, longer than 255 bytes, or contains a
    /// control character.
    pub fn new(name: impl Into<String>) -> Result<Self, CatalogError> {
        let name = name.into();
        check("namespace", &name)?;
        Ok(Self(name))
    }

    /// The name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn to_ident(&self) -> NamespaceIdent {
        NamespaceIdent::new(self.0.clone())
    }

    /// The identifier to put in a request path: the name percent-encoded.
    pub(crate) fn to_path_ident(&self) -> NamespaceIdent {
        NamespaceIdent::new(path_segment(&self.0))
    }

    /// Reads a namespace the catalog returned. Nested namespaces are refused as unsupported;
    /// a name Iceblink would refuse from a caller is a protocol error.
    pub(crate) fn from_ident(ident: &NamespaceIdent) -> Result<Self, CatalogError> {
        match ident.as_ref().as_slice() {
            [single] => Self::new(single.clone()).map_err(from_catalog),
            [] => Err(CatalogError::Protocol {
                message: "the catalog returned a namespace with no name".to_owned(),
            }),
            levels => Err(CatalogError::Unsupported {
                feature: format!("multi-level namespace {}", levels.join(".").escape_debug()),
            }),
        }
    }
}

impl fmt::Display for NamespaceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A table in a single-level namespace, such as `sales.orders`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableName {
    namespace: NamespaceName,
    name: String,
}

impl TableName {
    /// Checks the table's own name and pairs it with its namespace.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidName`] if the name is empty, longer than 255 bytes, or contains a
    /// control character.
    pub fn new(namespace: NamespaceName, name: impl Into<String>) -> Result<Self, CatalogError> {
        let name = name.into();
        check("table", &name)?;
        Ok(Self { namespace, name })
    }

    /// The namespace that holds the table.
    pub fn namespace(&self) -> &NamespaceName {
        &self.namespace
    }

    /// The table's own name, without its namespace.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn to_ident(&self) -> TableIdent {
        TableIdent::new(self.namespace.to_ident(), self.name.clone())
    }

    /// The identifier to put in a request path: both names percent-encoded.
    pub(crate) fn to_path_ident(&self) -> TableIdent {
        TableIdent::new(self.namespace.to_path_ident(), path_segment(&self.name))
    }

    /// Reads a table identifier the catalog returned.
    pub(crate) fn from_ident(ident: &TableIdent) -> Result<Self, CatalogError> {
        let namespace = NamespaceName::from_ident(ident.namespace())?;
        Self::new(namespace, ident.name()).map_err(from_catalog)
    }
}

impl fmt::Display for TableName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.namespace, self.name)
    }
}

fn check(kind: &str, name: &str) -> Result<(), CatalogError> {
    let reason = if name.is_empty() {
        format!("the {kind} name is empty")
    } else if name.len() > MAX_NAME_BYTES {
        format!("the {kind} name is {} bytes long", name.len())
    } else if name.chars().any(char::is_control) {
        format!("the {kind} name contains a control character")
    } else {
        return Ok(());
    };
    Err(CatalogError::InvalidName { reason })
}

/// A name that fails Iceblink's checks but came from the catalog is the catalog's fault.
fn from_catalog(error: CatalogError) -> CatalogError {
    match error {
        CatalogError::InvalidName { reason } => CatalogError::Protocol {
            message: format!("the catalog returned a name Iceblink can't use: {reason}"),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_length_and_characters() {
        let long = "a".repeat(MAX_NAME_BYTES);
        let too_long = "a".repeat(MAX_NAME_BYTES + 1);
        let cases: [(&str, Option<&str>); 6] = [
            ("sales", None),
            ("sales.eu", None),
            (&long, None),
            ("", Some("is empty")),
            (&too_long, Some("256 bytes long")),
            ("sa\nles", Some("control character")),
        ];
        for (name, refused) in cases {
            let result = NamespaceName::new(name);
            match refused {
                None => assert!(result.is_ok(), "{name:?}: {result:?}"),
                Some(phrase) => assert!(
                    matches!(&result, Err(CatalogError::InvalidName { reason }) if reason.contains(phrase)),
                    "{name:?}: {result:?}"
                ),
            }
        }
    }

    #[test]
    fn refuses_nested_namespaces_from_the_catalog() {
        let nested = NamespaceIdent::from_strs(["sales", "eu"]);
        let result = nested.map(|ident| NamespaceName::from_ident(&ident));
        assert!(
            matches!(
                &result,
                Ok(Err(CatalogError::Unsupported { feature })) if feature.contains("sales.eu")
            ),
            "{result:?}"
        );
    }

    #[test]
    fn writes_tables_as_namespace_dot_name() -> Result<(), CatalogError> {
        let table = TableName::new(NamespaceName::new("sales")?, "orders")?;
        assert_eq!(table.to_string(), "sales.orders");
        assert_eq!(TableName::from_ident(&table.to_ident())?, table);
        Ok(())
    }

    #[test]
    fn encodes_names_for_request_paths() -> Result<(), CatalogError> {
        let cases = [
            ("orders", "orders"),
            ("my table%", "my%20table%25"),
            ("a/b?c#d", "a%2Fb%3Fc%23d"),
            ("50%25off", "50%2525off"),
            ("v1.2-beta_~", "v1.2-beta_~"),
            ("café", "caf%C3%A9"),
        ];
        let sales = NamespaceName::new("sales")?;
        for (name, encoded) in cases {
            let table = TableName::new(sales.clone(), name)?;
            let ident = table.to_path_ident();
            assert_eq!(ident.name(), encoded, "{name:?}");
            assert_eq!(
                table.to_ident().name(),
                name,
                "the real identity stays unencoded"
            );
        }
        let spaced = NamespaceName::new("q1 sales")?.to_path_ident();
        assert_eq!(spaced.as_ref().as_slice(), ["q1%20sales"]);
        Ok(())
    }
}
