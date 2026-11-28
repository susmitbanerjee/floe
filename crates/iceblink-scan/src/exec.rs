//! `LakeScanExec`: streams one pinned snapshot's planned files and reports what the query read.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::error::{DataFusionError, Result as DFResult};
use datafusion::execution::{SendableRecordBatchStream, TaskContext};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::metrics::{
    BaselineMetrics, ExecutionPlanMetricsSet, Gauge, MetricBuilder, MetricsSet,
};
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, Partitioning, PlanProperties,
};
use futures::{Stream, StreamExt, TryStreamExt, stream};
use iceberg::Runtime;
use iceberg::arrow::ArrowReaderBuilder;
use iceberg::io::FileIO;
use iceberg::scan::{FileScanTask, FileScanTaskStream};

use crate::error::iceberg_to_datafusion;
use crate::io::{IoCounts, IoStats};
use crate::receipt::ScanReceipt;

/// Read the last 8 bytes, then exactly the footer: no byte is fetched twice, even in files
/// smaller than DataFusion's 512 KiB default prefetch (docs/adr/0006-scan-metrics.md).
const FOOTER_HINT_BYTES: usize = 8;

/// What the provider planned for one query.
pub(crate) struct ScanPlan {
    /// The table as people write it, such as `sales.orders`.
    pub(crate) table: String,
    pub(crate) snapshot_id: i64,
    /// From the snapshot summary's `total-data-files`, when the writer recorded it.
    pub(crate) files_total: Option<u64>,
    pub(crate) tasks: Vec<FileScanTask>,
    pub(crate) file_io: FileIO,
    pub(crate) runtime: Runtime,
    pub(crate) stats: Arc<IoStats>,
    /// The projected output schema.
    pub(crate) schema: SchemaRef,
}

/// Scans one pinned snapshot of an Iceberg table and reports, in EXPLAIN ANALYZE, the files it
/// planned against the snapshot's total and every byte and request it made to storage.
pub struct LakeScanExec {
    table: String,
    snapshot_id: i64,
    files_total: Option<u64>,
    tasks: Arc<Vec<FileScanTask>>,
    file_io: FileIO,
    runtime: Runtime,
    stats: Arc<IoStats>,
    properties: Arc<PlanProperties>,
    metrics: ExecutionPlanMetricsSet,
    bytes_read: Gauge,
    get_requests: Gauge,
}

impl LakeScanExec {
    pub(crate) fn new(plan: ScanPlan) -> Self {
        let metrics = ExecutionPlanMetricsSet::new();
        if let Some(total) = plan.files_total {
            MetricBuilder::new(&metrics)
                .global_counter("files_total")
                .add(usize::try_from(total).unwrap_or(usize::MAX));
        }
        MetricBuilder::new(&metrics)
            .global_counter("files_scanned")
            .add(plan.tasks.len());
        let bytes_read = MetricBuilder::new(&metrics).global_gauge("bytes_read");
        let get_requests = MetricBuilder::new(&metrics).global_gauge("get_requests");
        let properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(plan.schema),
            Partitioning::UnknownPartitioning(1),
            EmissionType::Incremental,
            Boundedness::Bounded,
        ));
        let exec = Self {
            table: plan.table,
            snapshot_id: plan.snapshot_id,
            files_total: plan.files_total,
            tasks: Arc::new(plan.tasks),
            file_io: plan.file_io,
            runtime: plan.runtime,
            stats: plan.stats,
            properties,
            metrics,
            bytes_read,
            get_requests,
        };
        // Planning already read the manifest list and manifests: show that even before execution.
        publish(&exec.stats, &exec.bytes_read, &exec.get_requests);
        exec
    }

    /// The snapshot this query is pinned to.
    pub fn snapshot_id(&self) -> i64 {
        self.snapshot_id
    }

    /// Data files left after pruning.
    pub fn files_scanned(&self) -> usize {
        self.tasks.len()
    }

    /// Data files in the snapshot, if its summary records the total.
    pub fn files_total(&self) -> Option<u64> {
        self.files_total
    }

    /// The summed size of the files left after pruning: an upper bound for the data bytes read.
    pub fn planned_file_bytes(&self) -> u64 {
        self.tasks
            .iter()
            .map(FileScanTask::file_size_in_bytes)
            .sum()
    }

    /// Everything this query has read from storage so far, planning included.
    pub fn io_counts(&self) -> IoCounts {
        self.stats.snapshot()
    }

    /// This scan's line on the statement's receipt, sharing its live storage counters.
    pub(crate) fn receipt(&self) -> ScanReceipt {
        ScanReceipt::new(
            self.table.clone(),
            self.snapshot_id,
            self.tasks.len(),
            Arc::clone(&self.stats),
        )
    }
}

impl fmt::Debug for LakeScanExec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LakeScanExec")
            .field("table", &self.table)
            .field("snapshot_id", &self.snapshot_id)
            .field("files_scanned", &self.tasks.len())
            .field("files_total", &self.files_total)
            .finish_non_exhaustive()
    }
}

impl DisplayAs for LakeScanExec {
    fn fmt_as(&self, _format: DisplayFormatType, formatter: &mut fmt::Formatter) -> fmt::Result {
        let total = self
            .files_total
            .map_or_else(|| "?".to_owned(), |total| total.to_string());
        write!(
            formatter,
            "LakeScanExec: {} snapshot={} files={}/{total}",
            self.table,
            self.snapshot_id,
            self.tasks.len()
        )
    }
}

impl ExecutionPlan for LakeScanExec {
    fn name(&self) -> &str {
        "LakeScanExec"
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![]
    }

    fn with_new_children(
        self: Arc<Self>,
        _children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        Ok(self)
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }

    fn metrics(&self) -> Option<MetricsSet> {
        Some(self.metrics.clone_inner())
    }

    fn execute(
        &self,
        partition: usize,
        _context: Arc<TaskContext>,
    ) -> DFResult<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(DataFusionError::Internal(format!(
                "LakeScanExec has one partition, not {partition}"
            )));
        }
        let tasks: FileScanTaskStream = Box::pin(stream::iter(
            self.tasks.iter().cloned().map(Ok).collect::<Vec<_>>(),
        ));
        let batches = ArrowReaderBuilder::new(self.file_io.clone(), self.runtime.clone())
            .with_metadata_size_hint(FOOTER_HINT_BYTES)
            .build()
            .read(tasks)
            .map_err(|error| iceberg_to_datafusion(&error))?
            .stream()
            .map_err(|error| iceberg_to_datafusion(&error));
        let metered = Metered {
            inner: Box::pin(batches),
            baseline: BaselineMetrics::new(&self.metrics, partition),
            stats: Arc::clone(&self.stats),
            bytes_read: self.bytes_read.clone(),
            get_requests: self.get_requests.clone(),
        };
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.properties.eq_properties.schema().clone(),
            metered,
        )))
    }
}

fn publish(stats: &IoStats, bytes_read: &Gauge, get_requests: &Gauge) {
    let counts = stats.snapshot();
    bytes_read.set(usize::try_from(counts.bytes_read).unwrap_or(usize::MAX));
    get_requests.set(usize::try_from(counts.get_requests).unwrap_or(usize::MAX));
}

type BatchStream = Pin<Box<dyn Stream<Item = DFResult<RecordBatch>> + Send>>;

/// Passes batches through, one at a time, keeping the query's metrics current.
struct Metered {
    inner: BatchStream,
    baseline: BaselineMetrics,
    stats: Arc<IoStats>,
    bytes_read: Gauge,
    get_requests: Gauge,
}

impl Stream for Metered {
    type Item = DFResult<RecordBatch>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.poll_next_unpin(context);
        if polled.is_ready() {
            publish(&self.stats, &self.bytes_read, &self.get_requests);
        }
        // Records output rows, bytes and batches, and marks the stream done at its end.
        self.baseline.record_poll(polled)
    }
}
