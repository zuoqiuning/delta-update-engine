//! Download progress: cumulative byte counter and per-file throttled events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::engine::Progress;

use super::{StageContext, LIVE_PROGRESS_INTERVAL};

/// Report cumulative download bytes. The total is the planned download size;
/// a patch fallback may push the counter past it, so the value is clamped.
pub(super) fn report_download(context: &StageContext<'_>, file: &str, bytes: u64) {
    report_bytes(
        context.progress,
        context.downloaded,
        context.total_download,
        file,
        bytes,
    );
}

fn report_bytes(
    progress: &(dyn Fn(Progress) + Send + Sync),
    downloaded: &Mutex<u64>,
    total: u64,
    file: &str,
    bytes: u64,
) {
    // Hold the lock across the callback so concurrent downloads cannot emit
    // progress out of order (the UI would otherwise jump backwards).
    let mut completed = downloaded
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *completed = (*completed).saturating_add(bytes).min(total);
    (progress)(Progress::Downloading {
        file: file.to_string(),
        total,
        completed: *completed,
    });
}

/// Live byte counter for one file.
///
/// The download path reports absolute bytes written so far; this converts them
/// into deltas on the shared cumulative counter, throttled so a large object
/// still moves the progress bar without one event per 8 KiB chunk. `finish`
/// adds the planned size so the cumulative total always ends at the promised
/// download size even when the fetcher reported nothing.
pub(super) struct FileProgress<'a> {
    file: &'a str,
    progress: &'a (dyn Fn(Progress) + Send + Sync),
    downloaded: &'a Mutex<u64>,
    total: u64,
    received: AtomicU64,
    reported: AtomicU64,
    last_emit: Mutex<Instant>,
}

impl<'a> FileProgress<'a> {
    pub(super) fn for_file(
        progress: &'a (dyn Fn(Progress) + Send + Sync),
        downloaded: &'a Mutex<u64>,
        total: u64,
        file: &'a str,
    ) -> Self {
        Self {
            file,
            progress,
            downloaded,
            total,
            received: AtomicU64::new(0),
            reported: AtomicU64::new(0),
            last_emit: Mutex::new(Instant::now() - LIVE_PROGRESS_INTERVAL),
        }
    }

    pub(super) fn report(&self, written: u64) {
        self.received.fetch_max(written, Ordering::Relaxed);
        self.emit(false);
    }

    pub(super) fn finish(&self, planned: u64) {
        self.received.fetch_max(planned, Ordering::Relaxed);
        self.emit(true);
    }

    fn emit(&self, force: bool) {
        let received = self.received.load(Ordering::Relaxed);
        let reported = self.reported.load(Ordering::Relaxed);
        if received <= reported {
            return;
        }
        if !force {
            let Ok(mut last) = self.last_emit.lock() else {
                return;
            };
            if last.elapsed() < LIVE_PROGRESS_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let previous = self.reported.swap(received, Ordering::Relaxed);
        let delta = received.saturating_sub(previous);
        if delta == 0 {
            return;
        }
        report_bytes(self.progress, self.downloaded, self.total, self.file, delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_values(events: &Mutex<Vec<Progress>>) -> Vec<u64> {
        events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Progress::Downloading { completed, .. } => Some(*completed),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn live_progress_is_monotone_and_ends_at_the_planned_size() {
        let downloaded = Mutex::new(0_u64);
        let events = Mutex::new(Vec::new());
        let sink = |event: Progress| events.lock().unwrap().push(event);
        let progress = FileProgress::for_file(&sink, &downloaded, 100, "App.exe");

        progress.report(40);
        progress.report(30); // stale value from an out-of-order chunk
        progress.report(80); // throttled
        progress.finish(100);

        assert_eq!(completed_values(&events), vec![40, 100]);
        assert_eq!(*downloaded.lock().unwrap(), 100);

        // A fetcher that reports more than planned cannot exceed the total.
        progress.report(150);
        progress.finish(100);
        assert_eq!(*downloaded.lock().unwrap(), 100);
    }

    #[test]
    fn finish_counts_planned_bytes_when_the_fetcher_reported_nothing() {
        let downloaded = Mutex::new(10_u64);
        let events = Mutex::new(Vec::new());
        let sink = |event: Progress| events.lock().unwrap().push(event);
        let progress = FileProgress::for_file(&sink, &downloaded, 110, "App.exe");

        progress.finish(50);

        assert_eq!(completed_values(&events), vec![60]);
    }
}
