//! Every statement's span says what it read: snapshot, files and bytes, rows, and SQLSTATE.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, PoisonError};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use crate::common::{TestResult, TestServer, failure, orders, rows};

type Fields = BTreeMap<String, String>;

/// Collects the fields of every closed `statement` span.
#[derive(Clone, Default)]
struct Statements(Arc<Mutex<Vec<Fields>>>);

impl Statements {
    fn closed(&self) -> Vec<Fields> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct Recorded(Fields);

impl Visit for Recorded {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for Statements
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if attributes.metadata().name() != "statement" {
            return;
        }
        let mut recorded = Recorded(Fields::new());
        attributes.record(&mut recorded);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(recorded);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        if let Some(span) = context.span(id)
            && let Some(recorded) = span.extensions_mut().get_mut::<Recorded>()
        {
            values.record(recorded);
        }
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        if let Some(span) = context.span(&id)
            && let Some(recorded) = span.extensions_mut().remove::<Recorded>()
        {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(recorded.0);
        }
    }
}

/// The server runs on this test's single thread, so the thread's subscriber sees its spans.
#[tokio::test(flavor = "current_thread")]
async fn each_statement_span_says_what_it_read() -> TestResult {
    let statements = Statements::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(statements.clone()));
    let fixture = orders().await?;
    let snapshot_id = fixture
        .load_table()
        .await?
        .metadata()
        .current_snapshot()
        .map(|snapshot| snapshot.snapshot_id().to_string())
        .ok_or("the fixture has no snapshot")?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;

    let sql = "SELECT count(*) FROM orders WHERE region = 'eu-west-1'";
    assert_eq!(rows(&client, sql).await?.len(), 1);
    let refused = failure(
        client
            .batch_execute("INSERT INTO orders (order_id) VALUES (7)")
            .await,
    )?;
    assert_eq!(refused.0, "25006");

    let closed = statements.closed();
    let [select, insert] = closed.as_slice() else {
        return Err(format!("expected two statement spans, got {closed:#?}").into());
    };
    let field = |span: &Fields, name: &str| span.get(name).cloned().unwrap_or_default();
    assert_eq!(
        field(select, "fingerprint"),
        "SELECT count(*) FROM orders WHERE region = ?"
    );
    assert!(field(select, "query_id").starts_with("q-"));
    assert_eq!(field(select, "snapshot_ids"), snapshot_id);
    assert_eq!(field(select, "files_scanned"), "90");
    assert!(field(select, "bytes_read").parse::<u64>()? > 0);
    assert_eq!(field(select, "rows"), "1");
    assert!(select.contains_key("elapsed_ms"));
    assert_eq!(field(select, "sqlstate"), "00000");

    assert_eq!(
        field(insert, "fingerprint"),
        "INSERT INTO orders (order_id) VALUES (?)"
    );
    assert_eq!(field(insert, "sqlstate"), "25006");
    assert_eq!(field(insert, "files_scanned"), "0");
    Ok(())
}
