//! The `floe` binary as a shell sees it: output, exit codes, and fix lines. Catalogs are
//! wiremock servers, so nothing leaves the machine.

use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult = Result<(), Box<dyn Error>>;

/// The example config in the repository root.
fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../floe.example.toml")
}

/// The example config with `edit` applied, written to a temp dir.
fn config_with(edit: impl Fn(String) -> String) -> Result<(TempDir, PathBuf), Box<dyn Error>> {
    let dir = TempDir::new()?;
    let file = dir.path().join("floe.toml");
    std::fs::write(&file, edit(std::fs::read_to_string(example())?))?;
    Ok((dir, file))
}

/// Runs `floe` with `args`, optionally with NO_COLOR set.
fn floe(args: &[&str], no_color: bool) -> Result<Output, std::io::Error> {
    let mut command = cargo_bin_cmd!("floe");
    command.args(args).env_remove("RUST_LOG");
    if no_color {
        command.env("NO_COLOR", "1");
    } else {
        command.env_remove("NO_COLOR");
    }
    command.output()
}

/// `floe` run off the async runtime, which keeps serving wiremock meanwhile.
async fn floe_async(args: Vec<String>) -> Result<Output, Box<dyn Error>> {
    let output = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        floe(&args, false)
    })
    .await??;
    Ok(output)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn prints_name_and_version_with_version_flag() -> TestResult {
    let output = floe(&["--version"], false)?;
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(
        text(&output.stdout),
        format!("floe {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_lists_every_exit_code() -> TestResult {
    let output = floe(&["--help"], false)?;
    assert!(output.status.success());
    let help = text(&output.stdout);
    for line in [
        "  0   ok",
        "  2   bad usage",
        "  69  catalog unreachable or unauthorised",
        "  70  internal error",
        "  71  cannot bind the Postgres listener",
        "  78  config invalid",
    ] {
        assert!(help.contains(line), "--help lacks {line:?}:\n{help}");
    }
    Ok(())
}

#[test]
fn bad_usage_exits_2() -> TestResult {
    let output = floe(&["serve", "--no-such-flag"], false)?;
    assert_eq!(output.status.code(), Some(2));
    Ok(())
}

#[test]
fn config_check_accepts_the_example() -> TestResult {
    let example = example();
    let path = example.to_str().ok_or("non-UTF-8 path")?;
    let output = floe(&["config", "check", "--config", path], false)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("ok     config    "), "{stdout}");
    assert!(
        stdout.contains(&format!("next   floe serve --config {path}")),
        "{stdout}"
    );
    Ok(())
}

#[test]
fn a_non_loopback_listener_exits_78_with_the_loopback_fix() -> TestResult {
    let (_dir, file) = config_with(|text| text.replace("127.0.0.1:5432", "0.0.0.0:5432"))?;
    let file = file.to_str().ok_or("non-UTF-8 path")?;
    let output = floe(&["config", "check", "--config", file], false)?;
    assert_eq!(output.status.code(), Some(78));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("fail   config    pg_listen is 0.0.0.0:5432"),
        "{stderr}"
    );
    assert!(
        stderr.contains(r#"fix: use pg_listen = "127.0.0.1:5432""#),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_missing_config_exits_78_and_suggests_the_example() -> TestResult {
    let dir = TempDir::new()?;
    let missing = dir.path().join("nope.toml");
    let output = floe(
        &["serve", "--config", missing.to_str().ok_or("path")?],
        false,
    )?;
    assert_eq!(output.status.code(), Some(78));
    assert!(text(&output.stderr).contains("floe.example.toml"));
    Ok(())
}

#[test]
fn output_has_no_escape_codes_when_piped_or_under_no_color() -> TestResult {
    let (_dir, bad) = config_with(|text| text.replace("127.0.0.1:5432", "0.0.0.0:5432"))?;
    let example = example();
    let runs = [
        vec![
            "config",
            "check",
            "--config",
            example.to_str().ok_or("path")?,
        ],
        vec!["config", "check", "--config", bad.to_str().ok_or("path")?],
        vec!["--help"],
        vec!["serve", "--no-such-flag"],
    ];
    for args in &runs {
        for no_color in [false, true] {
            let output = floe(args, no_color)?;
            let shown = format!("{}{}", text(&output.stdout), text(&output.stderr));
            assert!(
                !shown.contains('\x1b'),
                "{args:?} (NO_COLOR {no_color}): {shown:?}"
            );
        }
    }
    Ok(())
}

/// A catalog that answers the handshake and lists no namespaces.
async fn empty_catalog() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"defaults": {}, "overrides": {}}"#, "application/json"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/namespaces"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"{"namespaces": []}"#, "application/json"),
        )
        .mount(&server)
        .await;
    server
}

/// The example config pointed at `catalog`, listening on `listen`.
fn serve_config(catalog: &str, listen: SocketAddr) -> Result<(TempDir, String), Box<dyn Error>> {
    let (dir, file) = config_with(|text| {
        text.replace("http://127.0.0.1:8181/catalog", catalog)
            .replace("127.0.0.1:5432", &listen.to_string())
    })?;
    let file = file.to_str().ok_or("non-UTF-8 path")?.to_owned();
    Ok((dir, file))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_port_in_use_exits_71_with_a_fix_naming_the_port() -> TestResult {
    let catalog = empty_catalog().await;
    let taken = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let listen = taken.local_addr()?;
    let (_dir, file) = serve_config(&catalog.uri(), listen)?;

    let output = floe_async(vec!["serve".into(), "--config".into(), file]).await?;
    assert_eq!(output.status.code(), Some(71), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("ok     catalog   0 namespaces, 0 tables"),
        "{stdout}"
    );
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains(&format!("fail   postgres  cannot listen on {listen}")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("fix: port {} is already in use", listen.port())),
        "{stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unauthorised_catalog_exits_69_with_a_fix() -> TestResult {
    let catalog = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/config"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&catalog)
        .await;
    let (_dir, file) = serve_config(&catalog.uri(), "127.0.0.1:0".parse()?)?;

    let output = floe_async(vec!["serve".into(), "--config".into(), file]).await?;
    assert_eq!(output.status.code(), Some(69), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains(&format!("fail   catalog   HTTP 401 from {}", catalog.uri())),
        "{stderr}"
    );
    assert!(
        stderr.contains("fix: this catalog needs a token"),
        "{stderr}"
    );
    Ok(())
}
