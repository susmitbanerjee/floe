//! Tests for floe.toml. The harness lives here: a valid document that each case edits, an
//! injected environment, and `check`, which runs a table of cases for one rule.

mod rules;
mod security;

use super::{ConfigError, ValidConfig, from_toml};

const SECRET: &str = "s3cr3t-token-value";

/// The injected environment: one real secret, one empty variable, nothing else.
fn environment(name: &str) -> Option<String> {
    match name {
        "FLOE_CATALOG_TOKEN" => Some(SECRET.to_owned()),
        "FLOE_EMPTY" => Some(String::new()),
        _ => None,
    }
}

/// A floe.toml written as raw TOML values, so a case can swap in any type. Defaults are valid.
#[derive(Clone, Copy)]
struct Doc {
    pg_listen: &'static str,
    max_connections: &'static str,
    statement_timeout: &'static str,
    server_extra: &'static str,
    uri: &'static str,
    warehouse: &'static str,
    credential: Option<&'static str>,
    sigv4: Option<(&'static str, &'static str)>,
    sigv4_extra: &'static str,
    region: &'static str,
    endpoint: Option<&'static str>,
    allowed_prefixes: &'static str,
    allow_local_files: &'static str,
    trailer: &'static str,
}

impl Default for Doc {
    fn default() -> Self {
        Self {
            pg_listen: r#""127.0.0.1:5432""#,
            max_connections: "64",
            statement_timeout: r#""60s""#,
            server_extra: "",
            uri: r#""http://127.0.0.1:8181/catalog""#,
            warehouse: r#""demo""#,
            credential: None,
            sigv4: None,
            sigv4_extra: "",
            region: r#""us-east-1""#,
            endpoint: None,
            allowed_prefixes: r#"["s3://warehouse/"]"#,
            allow_local_files: "false",
            trailer: "",
        }
    }
}

impl Doc {
    fn render(&self) -> String {
        let mut text = format!(
            "[server]\npg_listen = {}\nmax_connections = {}\nstatement_timeout = {}\n{}\n",
            self.pg_listen, self.max_connections, self.statement_timeout, self.server_extra
        );
        text += &format!(
            "[catalog]\nuri = {}\nwarehouse = {}\n",
            self.uri, self.warehouse
        );
        if let Some(credential) = self.credential {
            text += &format!("credential = {credential}\n");
        }
        if let Some((region, service)) = self.sigv4 {
            text += &format!(
                "[catalog.sigv4]\nregion = {region}\nservice = {service}\n{}\n",
                self.sigv4_extra
            );
        }
        text += &format!("[storage]\nregion = {}\n", self.region);
        if let Some(endpoint) = self.endpoint {
            text += &format!("endpoint = {endpoint}\n");
        }
        text += &format!(
            "path_style = false\nallowed_prefixes = {}\nallow_local_files = {}\n{}\n",
            self.allowed_prefixes, self.allow_local_files, self.trailer
        );
        text
    }

    fn load(&self) -> Result<ValidConfig, ConfigError> {
        from_toml(&self.render(), &environment)
    }
}

/// What a case expects: a valid config, or an error variant plus a phrase from its message.
enum Expect {
    Valid,
    Invalid(fn(&ConfigError) -> bool, &'static str),
}

type Case = (&'static str, fn(&mut Doc), Expect);

fn check(rule: &str, cases: &[Case]) {
    for (name, edit, expect) in cases {
        let mut doc = Doc::default();
        edit(&mut doc);
        let result = doc.load();
        match expect {
            Expect::Valid => {
                assert!(
                    result.is_ok(),
                    "{rule} / {name}: expected valid, got {result:?}"
                );
            }
            Expect::Invalid(is_variant, phrase) => {
                let error = result.err();
                assert!(
                    error.as_ref().is_some_and(is_variant),
                    "{rule} / {name}: wrong outcome {error:?}"
                );
                let message = error
                    .map(|error| format!("{error}. fix: {}", error.fix()))
                    .unwrap_or_default();
                assert!(
                    message.contains(phrase),
                    "{rule} / {name}: message {message:?} lacks {phrase:?}"
                );
            }
        }
    }
}
