//! Everything `floe` prints for people, and the only module allowed to print.
//!
//! Phase 1 is plain text: a tag, a label column, then the detail. v0.1 styles the tags here
//! without touching callers, and must stay plain when stdout isn't a terminal or NO_COLOR is
//! set; the CLI tests check that no escape code ever appears. Startup lines go to stdout, failures
//! to stderr.

use std::io::{self, Write};
use std::time::Duration;

/// Wide enough for the longest tag, `ready`, and two spaces.
const TAG_WIDTH: usize = 7;
/// Wide enough for the longest label, `postgres`, and two spaces.
const LABEL_WIDTH: usize = 10;
/// Where an elapsed time starts, so durations line up.
const ELAPSED_COLUMN: usize = 57;

/// A step that worked: `ok     catalog   6 namespaces, 42 tables          310ms`.
pub(crate) fn ok(label: &str, detail: &str, elapsed: Option<Duration>) {
    emit(&mut io::stdout().lock(), &ok_line(label, detail, elapsed));
}

/// A step that failed, and what to do about it, on two lines.
pub(crate) fn fail(label: &str, what: &str, fix: &str) {
    emit(&mut io::stderr().lock(), &fail_lines(label, what, fix));
}

/// Where clients connect: `ready  postgres://127.0.0.1:5432/lake`.
pub(crate) fn ready(url: &str) {
    emit(&mut io::stdout().lock(), &tagged("ready", url));
}

/// The command to run next: `next   psql -h 127.0.0.1 -U floe lake`.
pub(crate) fn next(command: &str) {
    emit(&mut io::stdout().lock(), &tagged("next", command));
}

fn ok_line(label: &str, detail: &str, elapsed: Option<Duration>) -> String {
    let line = format!("{:<TAG_WIDTH$}{label:<LABEL_WIDTH$}{detail}", "ok");
    match elapsed {
        Some(elapsed) => format!("{line:<ELAPSED_COLUMN$}  {}ms", elapsed.as_millis()),
        None => line,
    }
}

fn fail_lines(label: &str, what: &str, fix: &str) -> String {
    let indent = TAG_WIDTH + LABEL_WIDTH;
    format!(
        "{:<TAG_WIDTH$}{label:<LABEL_WIDTH$}{what}\n{:indent$}fix: {fix}",
        "fail", ""
    )
}

fn tagged(tag: &str, text: &str) -> String {
    format!("{tag:<TAG_WIDTH$}{text}")
}

/// Writes one line. A closed stdout or stderr (such as `floe … | head`) is not worth failing
/// the command over, so write errors are dropped.
fn emit(out: &mut impl Write, text: &str) {
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_have_a_tag_a_label_column_and_the_detail() {
        assert_eq!(
            ok_line("config", "./floe.toml", None),
            "ok     config    ./floe.toml"
        );
        assert_eq!(
            ok_line(
                "catalog",
                "6 namespaces, 42 tables",
                Some(Duration::from_millis(310))
            ),
            "ok     catalog   6 namespaces, 42 tables                   310ms"
        );
        assert_eq!(
            tagged("ready", "postgres://127.0.0.1:5432/lake"),
            "ready  postgres://127.0.0.1:5432/lake"
        );
        assert_eq!(
            tagged("next", "psql -h 127.0.0.1 -U floe lake"),
            "next   psql -h 127.0.0.1 -U floe lake"
        );
    }

    #[test]
    fn a_failure_puts_its_fix_under_the_detail() {
        assert_eq!(
            fail_lines(
                "catalog",
                "HTTP 401 from https://x/iceberg",
                "check the token"
            ),
            "fail   catalog   HTTP 401 from https://x/iceberg\n                 fix: check the token"
        );
    }

    #[test]
    fn nothing_carries_an_escape_code() {
        let printed = [
            ok_line("catalog", "1 namespace", Some(Duration::from_secs(1))),
            fail_lines("postgres", "port in use", "free it"),
            tagged("ready", "postgres://[::1]:5432/lake"),
        ];
        assert!(printed.iter().all(|line| !line.contains('\x1b')));
    }
}
