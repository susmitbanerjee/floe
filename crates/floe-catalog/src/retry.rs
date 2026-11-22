//! Retries for idempotent catalog reads: at most three attempts, waiting 200 ms and then 400 ms.
//! Also logs the one debug event each call gets.

use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::error::{Call, Subject};

/// First try plus two retries.
const MAX_ATTEMPTS: u32 = 3;

/// The wait before retry number `attempt`: 200 ms after the first attempt, 400 ms after the second.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200_u64 << attempt.saturating_sub(1).min(8))
}

/// Runs `attempt` until it succeeds, fails with an error `retryable` refuses, or has run three
/// times. Generic over the error so it can be tested without a network.
pub(crate) async fn retry<T, E, F, Fut>(
    call: &Call<'_>,
    retryable: impl Fn(&E) -> bool,
    mut attempt: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let started = Instant::now();
    let mut attempts = 1;
    let result = loop {
        match attempt().await {
            Err(error) if attempts < MAX_ATTEMPTS && retryable(&error) => {
                sleep(backoff(attempts)).await;
                attempts += 1;
            }
            done => break done,
        }
    };
    log_call(call, started.elapsed(), attempts, result.is_ok());
    result
}

/// One debug event per call. Never headers, bodies or the credential.
fn log_call(call: &Call<'_>, elapsed: Duration, attempts: u32, succeeded: bool) {
    let (namespace, table) = match call.subject {
        Subject::Catalog => ("-".to_owned(), "-".to_owned()),
        Subject::Namespace(namespace) => (namespace.to_string(), "-".to_owned()),
        Subject::Table(table) => (table.namespace().to_string(), table.name().to_owned()),
    };
    tracing::debug!(
        operation = call.operation,
        namespace,
        table,
        duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        attempt = attempts,
        outcome = if succeeded { "ok" } else { "error" },
        "catalog call"
    );
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum Fake {
        Busy,
        Denied,
    }

    const CALL: Call<'static> = Call {
        operation: "list_namespaces",
        subject: Subject::Catalog,
    };

    /// Runs `retry` over a scripted list of results and reports the result, the attempts made
    /// and the (paused) time spent.
    async fn run(script: &[Result<u8, Fake>]) -> (Result<u8, Fake>, usize, Duration) {
        let calls = Cell::new(0);
        let started = Instant::now();
        let result = retry(
            &CALL,
            |error| *error == Fake::Busy,
            || {
                let index = calls.get();
                calls.set(index + 1);
                let next = match script.get(index) {
                    Some(Ok(value)) => Ok(*value),
                    Some(Err(Fake::Busy)) => Err(Fake::Busy),
                    Some(Err(Fake::Denied)) | None => Err(Fake::Denied),
                };
                async move { next }
            },
        )
        .await;
        (result, calls.get(), started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn returns_at_once_on_success() {
        assert_eq!(run(&[Ok(7)]).await, (Ok(7), 1, Duration::ZERO));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_once_after_200_ms() {
        let outcome = run(&[Err(Fake::Busy), Ok(7)]).await;
        assert_eq!(outcome, (Ok(7), 2, Duration::from_millis(200)));
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_three_attempts_and_600_ms() {
        let busy = [Err(Fake::Busy), Err(Fake::Busy), Err(Fake::Busy), Ok(7)];
        let outcome = run(&busy).await;
        assert_eq!(outcome, (Err(Fake::Busy), 3, Duration::from_millis(600)));
    }

    #[tokio::test(start_paused = true)]
    async fn never_retries_a_refused_error() {
        let outcome = run(&[Err(Fake::Denied), Ok(7)]).await;
        assert_eq!(outcome, (Err(Fake::Denied), 1, Duration::ZERO));
    }
}
