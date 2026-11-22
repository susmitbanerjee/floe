//! Receipts: what each statement read, for the statement's log line.
//!
//! A server puts one [`QueryReceipt`] into a session's config. Every scan planned in that
//! session records itself there, and the server takes the scans once a statement is planned.
//! Byte counts stay live, so reading them after the rows have streamed gives the final total.

use std::sync::{Arc, Mutex, PoisonError};

use crate::IoCounts;
use crate::io::IoStats;

/// The scans planned in one session since the last [`take`](Self::take).
#[derive(Debug, Default)]
pub struct QueryReceipt {
    scans: Mutex<Vec<ScanReceipt>>,
}

impl QueryReceipt {
    /// Hands over the scans recorded so far and starts an empty list.
    pub fn take(&self) -> Vec<ScanReceipt> {
        std::mem::take(&mut *self.scans.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn record(&self, scan: ScanReceipt) {
        self.scans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scan);
    }
}

/// One table scan: the pinned snapshot, the files left after pruning, and the storage counters.
#[derive(Debug, Clone)]
pub struct ScanReceipt {
    table: String,
    snapshot_id: i64,
    files_scanned: usize,
    stats: Arc<IoStats>,
}

impl ScanReceipt {
    pub(crate) fn new(
        table: String,
        snapshot_id: i64,
        files_scanned: usize,
        stats: Arc<IoStats>,
    ) -> Self {
        Self {
            table,
            snapshot_id,
            files_scanned,
            stats,
        }
    }

    /// The table as people write it, such as `sales.orders`.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// The snapshot the scan pinned.
    pub fn snapshot_id(&self) -> i64 {
        self.snapshot_id
    }

    /// Data files left after pruning.
    pub fn files_scanned(&self) -> usize {
        self.files_scanned
    }

    /// Everything the scan has read from storage so far, planning included.
    pub fn io_counts(&self) -> IoCounts {
        self.stats.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_hands_over_the_scans_and_empties_the_list() {
        let receipt = QueryReceipt::default();
        receipt.record(ScanReceipt::new(
            "sales.orders".to_owned(),
            7,
            3,
            Arc::default(),
        ));
        let scans = receipt.take();
        assert_eq!(scans.len(), 1);
        assert_eq!(scans.first().map(ScanReceipt::snapshot_id), Some(7));
        assert!(receipt.take().is_empty());
    }
}
