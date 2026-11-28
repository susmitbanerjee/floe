//! Secrets never leak, the loader's limits hold, and the example file stays valid.

use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use secrecy::ExposeSecret;

use super::{Doc, SECRET, environment};
use crate::config::validate::parse_duration;
use crate::config::{ConfigError, MAX_CONFIG_BYTES, from_toml, read_limited};

#[test]
fn parse_errors_carry_line_and_column() {
    let text = "[server]\npg_listen = \"127.0.0.1:5432\"\n  pg_listn = 1\n";
    let error = from_toml(text, &environment).err();
    assert!(
        matches!(
            error,
            Some(ConfigError::Parse {
                line: 3,
                column: 3,
                ..
            })
        ),
        "got {error:?}"
    );
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(
        message.starts_with("iceblink.toml line 3, column 3: "),
        "got {message:?}"
    );
}

#[test]
fn refuses_files_over_64_kib() {
    let path = Path::new("iceblink.toml");
    let at_limit = read_limited(io::repeat(b' ').take(MAX_CONFIG_BYTES), path);
    assert!(at_limit.is_ok(), "got {at_limit:?}");

    let over = read_limited(io::repeat(b' ').take(MAX_CONFIG_BYTES + 1), path).err();
    assert!(
        matches!(over, Some(ConfigError::TooLarge { .. })),
        "got {over:?}"
    );
    let message = over.map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("larger than 64 KiB"), "got {message:?}");
}

#[test]
fn debug_output_never_contains_the_credential() {
    let mut doc = Doc {
        uri: r#""https://catalog.example.com""#,
        ..Doc::default()
    };
    doc.credential = Some(r#""env:ICEBLINK_CATALOG_TOKEN""#);
    let config = doc.load();
    let credential = config
        .as_ref()
        .ok()
        .and_then(|config| config.catalog.credential.as_ref())
        .map(|secret| secret.expose_secret().to_owned());
    assert_eq!(
        credential.as_deref(),
        Some(SECRET),
        "the secret must resolve"
    );

    let debug = format!("{config:?}");
    assert!(!debug.contains(SECRET), "Debug leaked the secret: {debug}");
}

#[test]
fn errors_never_contain_the_credential() {
    let cases: [(&str, &str, &str); 5] = [
        (
            "plain token",
            r#""ghp_plainSecret123""#,
            "ghp_plainSecret123",
        ),
        (
            "token pasted as a name",
            r#""env:ghp_pastedToken""#,
            "ghp_pastedToken",
        ),
        ("number", "987654321", "987654321"),
        // A good reference over plain http is refused; the variable's value must not appear.
        (
            "set variable over http",
            r#""env:ICEBLINK_CATALOG_TOKEN""#,
            SECRET,
        ),
        ("variable not set", r#""env:ICEBLINK_MISSING""#, SECRET),
    ];
    for (name, credential, secret) in cases {
        let doc = Doc {
            credential: Some(credential),
            ..Doc::default()
        };
        let error = doc.load().err();
        assert!(error.is_some(), "{name}: expected an error");
        let shown = format!(
            "{error:?} {}",
            error.as_ref().map(ToString::to_string).unwrap_or_default()
        );
        assert!(
            !shown.contains(secret),
            "{name}: error leaked the secret: {shown}"
        );
    }
}

#[test]
fn example_config_loads_cleanly() {
    let example = include_str!("../../../../../iceblink.example.toml");
    let config = from_toml(example, &|_| None);
    assert!(config.is_ok(), "iceblink.example.toml failed: {config:?}");
}

#[test]
fn parses_durations_as_a_number_and_a_unit() {
    let cases: [(&str, Option<Duration>); 13] = [
        ("250ms", Some(Duration::from_millis(250))),
        ("1s", Some(Duration::from_secs(1))),
        ("60s", Some(Duration::from_secs(60))),
        ("90m", Some(Duration::from_secs(5400))),
        ("1h", Some(Duration::from_secs(3600))),
        ("0s", Some(Duration::ZERO)),
        ("", None),
        ("s", None),
        ("10", None),
        ("1.5h", None),
        ("1 s", None),
        ("1h30m", None),
        ("18446744073709551615h", None),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_duration(text), expected, "parsing {text:?}");
    }
}
