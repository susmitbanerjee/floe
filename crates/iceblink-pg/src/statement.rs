//! One statement's life, from planning to its last row: its query ID, deadline, receipt, and
//! the span that says what it read.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use datafusion::sql::sqlparser::ast::Statement;
use datafusion_postgres::pgwire::api::results::{QueryResponse, Response};
use datafusion_postgres::pgwire::error::{PgWireError, PgWireResult};
use datafusion_postgres::pgwire::messages::data::DataRow;
use floe_scan::{QueryReceipt, ScanReceipt};
use futures::{Stream, StreamExt, stream};
use tokio::time::{Instant, Sleep};
use tracing::{Instrument, Span, field};

use crate::PgError;
use crate::errors::{sqlstate, to_error_info};
use crate::fingerprint::fingerprint;

/// Hands out query IDs, unique within one server run. A counter, not a random number, so tests
/// stay deterministic; the ID only has to match a log line to an error.
#[derive(Debug, Default)]
pub(crate) struct QueryIds(AtomicU64);

impl QueryIds {
    pub(crate) fn next(&self) -> QueryId {
        QueryId(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// One statement's ID, shown as `q-00002a` in logs and in internal-error messages.
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueryId(u64);

impl fmt::Display for QueryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "q-{:06x}", self.0)
    }
}

/// What one connection's hook and handler share: query IDs, the session's receipt, and the
/// statement timeout, which starts at the server limit and can only go down.
#[derive(Debug)]
pub(crate) struct Statements {
    ids: Arc<QueryIds>,
    receipt: Arc<QueryReceipt>,
    limit: Duration,
    timeout_ms: AtomicU64,
}

impl Statements {
    pub(crate) fn new(ids: Arc<QueryIds>, receipt: Arc<QueryReceipt>, limit: Duration) -> Self {
        Self {
            ids,
            receipt,
            limit,
            timeout_ms: AtomicU64::new(millis(limit)),
        }
    }

    /// The server's statement timeout.
    pub(crate) fn limit(&self) -> Duration {
        self.limit
    }

    /// This connection's statement timeout.
    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed))
    }

    /// Sets this connection's timeout; the caller has checked it against the limit.
    pub(crate) fn set_timeout(&self, timeout: Duration) {
        self.timeout_ms.store(millis(timeout), Ordering::Relaxed);
    }

    /// Starts a statement: a new query ID, its span, and its deadline.
    pub(crate) fn start(&self, statement: &Statement) -> Run {
        let id = self.ids.next();
        let span = tracing::info_span!(
            "statement",
            query_id = %id,
            fingerprint = %fingerprint(statement),
            snapshot_ids = field::Empty,
            files_scanned = field::Empty,
            bytes_read = field::Empty,
            rows = field::Empty,
            elapsed_ms = field::Empty,
            sqlstate = field::Empty,
        );
        tracing::debug!(parent: &span, sql = %statement, "statement text");
        // Scans left over from a statement that never finished planning belong to no one.
        drop(self.receipt.take());
        let started = Instant::now();
        Run {
            id,
            span,
            started,
            deadline: started + self.timeout(),
            receipt: Arc::clone(&self.receipt),
            scans: Vec::new(),
            rows: 0,
            finished: false,
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// One statement in flight. It finishes exactly once: when its last row is sent, when it
/// fails, or, if dropped before either (the client went away), as cancelled.
pub(crate) struct Run {
    id: QueryId,
    span: Span,
    started: Instant,
    deadline: Instant,
    receipt: Arc<QueryReceipt>,
    scans: Vec<ScanReceipt>,
    rows: u64,
    finished: bool,
}

impl Run {
    /// Runs `work` inside the statement's span, under its deadline.
    ///
    /// # Errors
    ///
    /// [`PgError::StatementTimeout`] when the deadline passes first.
    pub(crate) async fn within<F: Future>(&self, work: F) -> Result<F::Output, PgError> {
        tokio::time::timeout_at(self.deadline, work.instrument(self.span.clone()))
            .await
            .map_err(|_| PgError::StatementTimeout)
    }

    /// Ends the statement with `error`, as the client will see it.
    pub(crate) fn fail(mut self, error: &(dyn Error + 'static)) -> PgWireError {
        self.error(error)
    }

    /// Ends the statement with an error that is already pgwire's: user errors pass through
    /// with their code recorded, and anything else is mapped.
    pub(crate) fn fail_pgwire(mut self, error: PgWireError) -> PgWireError {
        self.pgwire_error(error)
    }

    /// Hands back `response`. Rows are tracked until the last one is sent; anything else
    /// finishes the statement now.
    pub(crate) fn track(mut self, response: Response) -> Response {
        self.scans = self.receipt.take();
        match response {
            Response::Query(query) => Response::Query(Tracked::wrap(self, query)),
            Response::Error(info) => {
                self.finish(&info.code);
                Response::Error(info)
            }
            other => {
                self.finish(sqlstate::SUCCESS);
                other
            }
        }
    }

    fn error(&mut self, error: &(dyn Error + 'static)) -> PgWireError {
        let info = to_error_info(error, &self.id.to_string());
        self.finish(&info.code);
        PgWireError::UserError(Box::new(info))
    }

    fn pgwire_error(&mut self, error: PgWireError) -> PgWireError {
        match error {
            PgWireError::UserError(info) => {
                self.finish(&info.code);
                PgWireError::UserError(info)
            }
            other => self.error(&other),
        }
    }

    fn finish(&mut self, sqlstate: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.scans.is_empty() {
            self.scans = self.receipt.take();
        }
        let snapshot_ids: Vec<String> = self
            .scans
            .iter()
            .map(|scan| scan.snapshot_id().to_string())
            .collect();
        let files: usize = self.scans.iter().map(ScanReceipt::files_scanned).sum();
        let bytes: u64 = self
            .scans
            .iter()
            .map(|scan| scan.io_counts().bytes_read)
            .sum();
        let span = &self.span;
        span.record("snapshot_ids", snapshot_ids.join(",").as_str());
        span.record("files_scanned", files);
        span.record("bytes_read", bytes);
        span.record("rows", self.rows);
        span.record("elapsed_ms", millis(self.started.elapsed()));
        span.record("sqlstate", sqlstate);
        tracing::info!(parent: span, "statement finished");
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.finish(sqlstate::QUERY_CANCELED);
    }
}

type Rows = Pin<Box<dyn Stream<Item = PgWireResult<DataRow>> + Send>>;

/// A statement's rows on their way to the client: counted, cut off at the deadline, and the
/// statement finished when they end.
struct Tracked {
    run: Run,
    rows: Rows,
    deadline: Pin<Box<Sleep>>,
    done: bool,
}

impl Tracked {
    fn wrap(run: Run, mut query: QueryResponse) -> QueryResponse {
        let rows = std::mem::replace(query.data_rows(), Box::pin(stream::empty()));
        let deadline = Box::pin(tokio::time::sleep_until(run.deadline));
        *query.data_rows() = Box::pin(Self {
            run,
            rows,
            deadline,
            done: false,
        });
        query
    }

    /// Stops reading rows: dropping the stream stops DataFusion's work for this statement.
    fn stop(&mut self) {
        self.done = true;
        self.rows = Box::pin(stream::empty());
    }
}

impl Stream for Tracked {
    type Item = PgWireResult<DataRow>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if this.done {
            return Poll::Ready(None);
        }
        if this.deadline.as_mut().poll(context).is_ready() {
            this.stop();
            return Poll::Ready(Some(Err(this.run.error(&PgError::StatementTimeout))));
        }
        let polled = {
            let _entered = this.run.span.enter();
            this.rows.poll_next_unpin(context)
        };
        match polled {
            Poll::Ready(Some(Ok(row))) => {
                this.run.rows += 1;
                Poll::Ready(Some(Ok(row)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.stop();
                Poll::Ready(Some(Err(this.run.pgwire_error(error))))
            }
            Poll::Ready(None) => {
                this.stop();
                this.run.finish(sqlstate::SUCCESS);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
