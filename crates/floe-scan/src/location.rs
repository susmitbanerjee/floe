//! The location guard: Floe reads only under `storage.allowed_prefixes`.

use serde::{Deserialize, Serialize};

use crate::{ScanError, StorageSettings};

/// Decides whether a table or file location may be read, before any storage request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocationGuard {
    /// Each ends in a slash, so `s3://data/` can't match `s3://data2/`.
    prefixes: Vec<String>,
    allow_local_files: bool,
}

impl LocationGuard {
    pub(crate) fn new(settings: &StorageSettings) -> Self {
        Self {
            prefixes: settings
                .allowed_prefixes
                .iter()
                .map(ToString::to_string)
                .collect(),
            allow_local_files: settings.allow_local_files,
        }
    }

    /// Allows `path` only under an allowed prefix, and local paths only when local files are
    /// allowed. `..` segments are refused outright, so a path can't climb out of its prefix.
    pub(crate) fn check(&self, path: &str) -> Result<(), ScanError> {
        let local = path.starts_with("file:") || path.starts_with('/');
        let climbs = path.split('/').any(|segment| segment == "..");
        let under_prefix = self
            .prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()));
        if climbs || (local && !self.allow_local_files) || !under_prefix {
            return Err(ScanError::LocationNotAllowed {
                path_prefix: path_prefix(path),
            });
        }
        Ok(())
    }
}

/// The part of a location safe to show: the scheme and its first segment, such as `s3://bucket/`
/// or `file:///var/`. Never the full path.
fn path_prefix(path: &str) -> String {
    let first_segment = |rest: &str| {
        rest.trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    match path.split_once("://") {
        // `file:///tmp/x` keeps its root slash: `file:///tmp/`.
        Some(("file", rest)) => format!("file:///{}/", first_segment(rest)),
        Some((scheme, rest)) => format!("{scheme}://{}/", first_segment(rest)),
        None => format!("/{}/", first_segment(path)),
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    fn guard(prefixes: &[&str], allow_local_files: bool) -> Result<LocationGuard, url::ParseError> {
        let allowed_prefixes = prefixes
            .iter()
            .map(|prefix| Url::parse(prefix))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LocationGuard::new(&StorageSettings {
            endpoint: None,
            region: "us-east-1".to_owned(),
            path_style: false,
            allowed_prefixes,
            allow_local_files,
        }))
    }

    #[test]
    fn allows_only_paths_under_a_prefix() -> Result<(), url::ParseError> {
        let s3 = guard(&["s3://warehouse/sales/"], false)?;
        let local = guard(&["file:///tmp/fixture/"], true)?;
        let local_off = guard(&["file:///tmp/fixture/"], false)?;
        let cases: [(&LocationGuard, &str, Option<&str>); 7] = [
            (&s3, "s3://warehouse/sales/orders/data/a.parquet", None),
            (
                &s3,
                "s3://warehouse/marketing/x.parquet",
                Some("s3://warehouse/"),
            ),
            (
                &s3,
                "s3://warehouse-2/sales/x.parquet",
                Some("s3://warehouse-2/"),
            ),
            (
                &s3,
                "s3://warehouse/sales/../hr/x.parquet",
                Some("s3://warehouse/"),
            ),
            (&local, "file:///tmp/fixture/sales/orders/m.json", None),
            (
                &local_off,
                "file:///tmp/fixture/sales/orders/m.json",
                Some("file:///tmp/"),
            ),
            (&s3, "/etc/passwd", Some("/etc/")),
        ];
        for (guard, path, refused) in cases {
            let result = guard.check(path);
            match refused {
                None => assert!(result.is_ok(), "{path}: {result:?}"),
                Some(prefix) => assert!(
                    matches!(&result, Err(ScanError::LocationNotAllowed { path_prefix }) if path_prefix == prefix),
                    "{path}: {result:?}"
                ),
            }
        }
        Ok(())
    }
}
