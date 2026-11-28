//! One table per rule: each case is valid, or names the error variant and a phrase its message
//! must contain.

use super::{Doc, Expect, check};
use crate::config::ConfigError;

#[test]
fn rule_1_pg_listen_must_be_loopback() {
    check(
        "rule 1",
        &[
            ("ipv4 loopback", |_| {}, Expect::Valid),
            (
                "ipv6 loopback",
                |d| d.pg_listen = r#""[::1]:5432""#,
                Expect::Valid,
            ),
            (
                "other 127/8 address",
                |d| d.pg_listen = r#""127.0.0.2:6432""#,
                Expect::Valid,
            ),
            (
                "all interfaces",
                |d| d.pg_listen = r#""0.0.0.0:5432""#,
                Expect::Invalid(
                    |e| matches!(e, ConfigError::NotLoopback { .. }),
                    "pg_listen is 0.0.0.0:5432, but this build has no authentication, so Floe \
                     only listens on loopback. fix: use pg_listen = \"127.0.0.1:5432\" (password \
                     login arrives in v0.1)",
                ),
            ),
            (
                "private network address",
                |d| d.pg_listen = r#""192.168.1.10:5432""#,
                Expect::Invalid(
                    |e| matches!(e, ConfigError::NotLoopback { .. }),
                    "only listens on loopback",
                ),
            ),
        ],
    );
}

#[test]
fn rule_2_limits_stay_in_range() {
    let out_of_range = |e: &ConfigError| matches!(e, ConfigError::OutOfRange { .. });
    let not_a_duration = |e: &ConfigError| matches!(e, ConfigError::InvalidDuration { .. });
    check(
        "rule 2",
        &[
            ("one connection", |d| d.max_connections = "1", Expect::Valid),
            (
                "1024 connections",
                |d| d.max_connections = "1024",
                Expect::Valid,
            ),
            (
                "one second",
                |d| d.statement_timeout = r#""1s""#,
                Expect::Valid,
            ),
            (
                "one hour",
                |d| d.statement_timeout = r#""1h""#,
                Expect::Valid,
            ),
            (
                "sixty minutes",
                |d| d.statement_timeout = r#""60m""#,
                Expect::Valid,
            ),
            (
                "zero connections",
                |d| d.max_connections = "0",
                Expect::Invalid(out_of_range, "set max_connections between 1 and 1024"),
            ),
            (
                "1025 connections",
                |d| d.max_connections = "1025",
                Expect::Invalid(out_of_range, "max_connections is 1025"),
            ),
            (
                "timeout under a second",
                |d| d.statement_timeout = r#""500ms""#,
                Expect::Invalid(out_of_range, "set statement_timeout between 1s and 1h"),
            ),
            (
                "timeout over an hour",
                |d| d.statement_timeout = r#""2h""#,
                Expect::Invalid(out_of_range, "statement_timeout is 2h"),
            ),
            (
                "timeout in words",
                |d| d.statement_timeout = r#""sixty""#,
                Expect::Invalid(not_a_duration, "is not a duration"),
            ),
            (
                "compound timeout",
                |d| d.statement_timeout = r#""1h 30m""#,
                Expect::Invalid(not_a_duration, "such as \"90m\""),
            ),
        ],
    );
}

#[test]
fn rule_3_urls_use_https_except_to_loopback_and_always_with_auth() {
    let bad_url = |e: &ConfigError| matches!(e, ConfigError::InvalidUrl { .. });
    check(
        "rule 3",
        &[
            ("plain http without auth", |_| {}, Expect::Valid),
            (
                "plain http to localhost",
                |d| d.uri = r#""http://localhost:8181/catalog""#,
                Expect::Valid,
            ),
            (
                "plain http to ipv6 loopback",
                |d| d.uri = r#""http://[::1]:8181/catalog""#,
                Expect::Valid,
            ),
            (
                "plain http to another host",
                |d| d.uri = r#""http://catalog.example.com/iceberg""#,
                Expect::Invalid(
                    bad_url,
                    "catalog.uri uses plain http to a host that isn't this machine. fix: change \
                     catalog.uri to https://, or to http://127.0.0.1 when the service runs on \
                     this machine",
                ),
            ),
            (
                "plain http to a private address",
                |d| d.uri = r#""http://10.0.0.5:8181/catalog""#,
                Expect::Invalid(bad_url, "catalog.uri uses plain http"),
            ),
            (
                "plain http to a name that only looks local",
                |d| d.uri = r#""http://127.0.0.1.nip.io:8181/catalog""#,
                Expect::Invalid(bad_url, "catalog.uri uses plain http"),
            ),
            (
                "https with a credential",
                |d| {
                    d.uri = r#""https://catalog.example.com/iceberg""#;
                    d.credential = Some(r#""env:FLOE_CATALOG_TOKEN""#);
                },
                Expect::Valid,
            ),
            (
                "ftp scheme",
                |d| d.uri = r#""ftp://127.0.0.1/catalog""#,
                Expect::Invalid(bad_url, "catalog.uri is not an http or https URL"),
            ),
            (
                "not a URL",
                |d| d.uri = r#""catalog""#,
                Expect::Invalid(bad_url, "catalog.uri is not an http or https URL"),
            ),
            (
                "http with a credential",
                |d| d.credential = Some(r#""env:FLOE_CATALOG_TOKEN""#),
                Expect::Invalid(bad_url, "catalog.uri must use https"),
            ),
            (
                "http with sigv4",
                |d| d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#)),
                Expect::Invalid(bad_url, "catalog.uri must use https"),
            ),
            (
                "storage endpoint that is not http",
                |d| d.endpoint = Some(r#""minio:9000""#),
                Expect::Invalid(bad_url, "storage.endpoint is not an http or https URL"),
            ),
            (
                "MinIO endpoint",
                |d| d.endpoint = Some(r#""http://127.0.0.1:9000""#),
                Expect::Valid,
            ),
            (
                "https storage endpoint on another host",
                |d| d.endpoint = Some(r#""https://minio.example.com""#),
                Expect::Valid,
            ),
            (
                "plain http storage endpoint on another host",
                |d| d.endpoint = Some(r#""http://minio.example.com:9000""#),
                Expect::Invalid(
                    bad_url,
                    "storage.endpoint uses plain http to a host that isn't this machine",
                ),
            ),
        ],
    );
}

#[test]
fn rule_3_a_url_can_opt_in_to_plain_http_on_a_private_network() {
    let bad_url = |e: &ConfigError| matches!(e, ConfigError::InvalidUrl { .. });
    check(
        "rule 3 opt-in",
        &[
            (
                "catalog by container service name, opted in",
                |d| {
                    d.uri = r#""http://lakekeeper:8181/catalog""#;
                    d.catalog_extra = "allow_plain_http = true";
                },
                Expect::Valid,
            ),
            (
                "catalog by container service name, not opted in",
                |d| d.uri = r#""http://lakekeeper:8181/catalog""#,
                Expect::Invalid(
                    bad_url,
                    "on a private network you control, such as between containers, set \
                     catalog.allow_plain_http = true",
                ),
            ),
            (
                "catalog opted in explicitly off",
                |d| {
                    d.uri = r#""http://lakekeeper:8181/catalog""#;
                    d.catalog_extra = "allow_plain_http = false";
                },
                Expect::Invalid(bad_url, "catalog.uri uses plain http"),
            ),
            (
                "the opt-in never lets a credential cross plain http",
                |d| {
                    d.uri = r#""http://lakekeeper:8181/catalog""#;
                    d.catalog_extra = "allow_plain_http = true";
                    d.credential = Some(r#""env:FLOE_CATALOG_TOKEN""#);
                },
                Expect::Invalid(bad_url, "catalog.uri must use https"),
            ),
            (
                "MinIO by container service name, opted in",
                |d| {
                    d.endpoint = Some(r#""http://minio:9000""#);
                    d.trailer = "allow_plain_http = true";
                },
                Expect::Valid,
            ),
            (
                "MinIO by container service name, not opted in",
                |d| d.endpoint = Some(r#""http://minio:9000""#),
                Expect::Invalid(bad_url, "set storage.allow_plain_http = true"),
            ),
            (
                "the catalog's opt-in doesn't cover storage",
                |d| {
                    d.endpoint = Some(r#""http://minio:9000""#);
                    d.catalog_extra = "allow_plain_http = true";
                },
                Expect::Invalid(bad_url, "storage.endpoint uses plain http"),
            ),
        ],
    );
}

#[test]
fn rule_4_sigv4_names_a_known_service_and_a_region() {
    let bad_service = |e: &ConfigError| matches!(e, ConfigError::InvalidSigV4Service { .. });
    let bad_region = |e: &ConfigError| matches!(e, ConfigError::InvalidRegion { .. });
    check(
        "rule 4",
        &[
            (
                "glue",
                |d| {
                    d.uri = r#""https://glue.ap-south-1.amazonaws.com/iceberg""#;
                    d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#));
                },
                Expect::Valid,
            ),
            (
                "s3tables in a GovCloud region",
                |d| {
                    d.uri = r#""https://s3tables.us-gov-west-1.amazonaws.com/iceberg""#;
                    d.sigv4 = Some((r#""us-gov-west-1""#, r#""s3tables""#));
                },
                Expect::Valid,
            ),
            (
                "unknown service",
                |d| {
                    d.uri = r#""https://glue.example.com""#;
                    d.sigv4 = Some((r#""ap-south-1""#, r#""s3""#));
                },
                Expect::Invalid(bad_service, "use \"glue\" or \"s3tables\""),
            ),
            (
                "empty region",
                |d| {
                    d.uri = r#""https://glue.example.com""#;
                    d.sigv4 = Some((r#""""#, r#""glue""#));
                },
                Expect::Invalid(bad_region, "catalog.sigv4.region is \"\""),
            ),
            (
                "region written as a city",
                |d| {
                    d.uri = r#""https://glue.example.com""#;
                    d.sigv4 = Some((r#""Mumbai""#, r#""glue""#));
                },
                Expect::Invalid(bad_region, "is not an AWS region name"),
            ),
            (
                "storage region with underscores",
                |d| d.region = r#""us_east_1""#,
                Expect::Invalid(bad_region, "storage.region is \"us_east_1\""),
            ),
            (
                "named AWS profile",
                |d| {
                    d.uri = r#""https://glue.ap-south-1.amazonaws.com/iceberg""#;
                    d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#));
                    d.sigv4_extra = "profile = \"analytics\"";
                },
                Expect::Valid,
            ),
            (
                "blank AWS profile",
                |d| {
                    d.uri = r#""https://glue.ap-south-1.amazonaws.com/iceberg""#;
                    d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#));
                    d.sigv4_extra = "profile = \" \"";
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::InvalidAwsProfile),
                    "catalog.sigv4.profile is empty",
                ),
            ),
            (
                "bearer credential and sigv4 together",
                |d| {
                    d.uri = r#""https://glue.ap-south-1.amazonaws.com/iceberg""#;
                    d.credential = Some(r#""env:FLOE_CATALOG_TOKEN""#);
                    d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#));
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::ConflictingCatalogAuth),
                    "are both set",
                ),
            ),
        ],
    );
}

#[test]
fn s3_reads_take_credentials_from_the_sigv4_profile() -> Result<(), ConfigError> {
    let mut doc = Doc {
        uri: r#""https://glue.ap-south-1.amazonaws.com/iceberg""#,
        sigv4: Some((r#""ap-south-1""#, r#""glue""#)),
        sigv4_extra: "profile = \"analytics\"",
        ..Doc::default()
    };
    let signed = doc.load()?;
    assert_eq!(signed.storage.aws_profile.as_deref(), Some("analytics"));

    doc.sigv4_extra = "";
    let default_chain = doc.load()?;
    assert_eq!(default_chain.storage.aws_profile, None);
    Ok(())
}

#[test]
fn rule_5_secrets_are_env_references_that_resolve() {
    check(
        "rule 5",
        &[
            (
                "variable that is set",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some(r#""env:FLOE_CATALOG_TOKEN""#);
                },
                Expect::Valid,
            ),
            (
                "plain token",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some(r#""ghp_plainSecret123""#);
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::PlainSecret { .. }),
                    "catalog.credential looks like a plain secret, and secrets never go in \
                     floe.toml. fix: put it in an environment variable and write env:NAME",
                ),
            ),
            (
                "lowercase variable name",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some(r#""env:floe_token""#);
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::InvalidSecretReference { .. }),
                    "uppercase letters, digits and underscores",
                ),
            ),
            (
                "variable that is not set",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some(r#""env:FLOE_MISSING""#);
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::MissingSecret { .. }),
                    "FLOE_MISSING, which is not set",
                ),
            ),
            (
                "variable that is empty",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some(r#""env:FLOE_EMPTY""#);
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::MissingSecret { .. }),
                    "FLOE_EMPTY, which is not set",
                ),
            ),
            (
                "credential that is a number",
                |d| {
                    d.uri = r#""https://catalog.example.com""#;
                    d.credential = Some("987654321");
                },
                Expect::Invalid(
                    |e| matches!(e, ConfigError::Parse { .. }),
                    "catalog.credential must be a string like \"env:NAME\"",
                ),
            ),
        ],
    );
}

#[test]
fn rule_6_allowed_prefixes_are_s3_folders() {
    let bad_prefix = |e: &ConfigError| matches!(e, ConfigError::InvalidPrefix { .. });
    check(
        "rule 6",
        &[
            ("bucket root", |_| {}, Expect::Valid),
            (
                "nested prefix",
                |d| d.allowed_prefixes = r#"["s3://warehouse/", "s3://lake-01/a/b/"]"#,
                Expect::Valid,
            ),
            (
                "empty list",
                |d| d.allowed_prefixes = "[]",
                Expect::Invalid(
                    |e| matches!(e, ConfigError::NoAllowedPrefixes),
                    "storage.allowed_prefixes is empty",
                ),
            ),
            (
                "no trailing slash",
                |d| d.allowed_prefixes = r#"["s3://warehouse/tables"]"#,
                Expect::Invalid(bad_prefix, "must end in a slash"),
            ),
            (
                "no scheme",
                |d| d.allowed_prefixes = r#"["warehouse/"]"#,
                Expect::Invalid(bad_prefix, "must start with s3://"),
            ),
            (
                "uppercase bucket",
                |d| d.allowed_prefixes = r#"["s3://Warehouse/"]"#,
                Expect::Invalid(bad_prefix, "is not a valid S3 bucket name"),
            ),
            (
                "local files without the flag",
                |d| d.allowed_prefixes = r#"["file:///tmp/floe/"]"#,
                Expect::Invalid(bad_prefix, "file:// needs allow_local_files = true"),
            ),
            (
                "local files with the flag",
                |d| {
                    d.allowed_prefixes = r#"["file:///tmp/floe/"]"#;
                    d.allow_local_files = "true";
                },
                Expect::Valid,
            ),
        ],
    );
}

#[test]
fn rule_7_unknown_keys_name_the_key_and_the_valid_keys() {
    let parse = |e: &ConfigError| matches!(e, ConfigError::Parse { .. });
    check(
        "rule 7",
        &[
            (
                "misspelt server key",
                |d| d.server_extra = "pg_listn = \"127.0.0.1:5432\"",
                Expect::Invalid(
                    parse,
                    "unknown field `pg_listn`, expected one of `pg_listen`, `max_connections`, \
                     `statement_timeout`",
                ),
            ),
            (
                "misspelt table",
                |d| d.trailer = "[servr]\nport = 1",
                Expect::Invalid(parse, "unknown field `servr`, expected one of `server`"),
            ),
            (
                "unknown sigv4 key",
                |d| {
                    d.uri = r#""https://glue.example.com""#;
                    d.sigv4 = Some((r#""ap-south-1""#, r#""glue""#));
                    d.sigv4_extra = "role_arn = \"arn:aws:iam::123456789012:role/floe\"";
                },
                Expect::Invalid(
                    parse,
                    "unknown field `role_arn`, expected one of `region`, `service`, `profile`",
                ),
            ),
        ],
    );
}
