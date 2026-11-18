use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A point-in-time immutable snapshot of media relay traffic counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaTrafficSnapshot {
    /// Total number of requests initiated through the relay.
    pub requests_total: u64,
    /// Number of requests that successfully returned response headers and body stream.
    pub requests_succeeded: u64,
    /// Number of requests that failed before yielding response headers to caller.
    pub requests_failed: u64,
    /// Total payload bytes forwarded from upstream servers to callers.
    pub bytes_forwarded: u64,
    /// Number of media streams currently active and in-flight.
    pub active_streams: u64,
    /// Number of streams cancelled prematurely before reaching EOF or failure.
    pub streams_cancelled: u64,
    /// Number of streams that encountered mid-stream failure after headers were returned.
    pub mid_stream_failures: u64,
    /// Number of successful pre-header expired URL refreshes and retries.
    pub pre_header_refreshes: u64,
}

#[derive(Debug, Default)]
struct CountersInner {
    requests_total: AtomicU64,
    requests_succeeded: AtomicU64,
    requests_failed: AtomicU64,
    bytes_forwarded: AtomicU64,
    active_streams: AtomicU64,
    streams_cancelled: AtomicU64,
    mid_stream_failures: AtomicU64,
    pre_header_refreshes: AtomicU64,
}

/// Thread-safe atomic counters tracking relay traffic and stream lifecycles.
#[derive(Debug, Clone, Default)]
pub struct MediaTrafficCounters {
    inner: Arc<CountersInner>,
}

impl MediaTrafficCounters {
    /// Create a new, zero-initialized set of counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture an immutable snapshot of all counter values.
    pub fn snapshot(&self) -> MediaTrafficSnapshot {
        MediaTrafficSnapshot {
            requests_total: self.inner.requests_total.load(Ordering::Relaxed),
            requests_succeeded: self.inner.requests_succeeded.load(Ordering::Relaxed),
            requests_failed: self.inner.requests_failed.load(Ordering::Relaxed),
            bytes_forwarded: self.inner.bytes_forwarded.load(Ordering::Relaxed),
            active_streams: self.inner.active_streams.load(Ordering::Relaxed),
            streams_cancelled: self.inner.streams_cancelled.load(Ordering::Relaxed),
            mid_stream_failures: self.inner.mid_stream_failures.load(Ordering::Relaxed),
            pre_header_refreshes: self.inner.pre_header_refreshes.load(Ordering::Relaxed),
        }
    }

    /// Increment total request count.
    pub fn inc_requests_total(&self) {
        self.inner.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment succeeded request count.
    pub fn inc_requests_succeeded(&self) {
        self.inner.requests_succeeded.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment failed request count.
    pub fn inc_requests_failed(&self) {
        self.inner.requests_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Add bytes to total bytes forwarded.
    pub fn add_bytes_forwarded(&self, bytes: u64) {
        self.inner.bytes_forwarded.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Increment number of active streams in-flight.
    pub fn inc_active_streams(&self) {
        self.inner.active_streams.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement number of active streams in-flight (saturating at 0).
    pub fn dec_active_streams(&self) {
        let _ = self.inner.active_streams.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |val| Some(val.saturating_sub(1)),
        );
    }

    /// Increment number of streams cancelled before completion.
    pub fn inc_streams_cancelled(&self) {
        self.inner.streams_cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment number of mid-stream failures.
    pub fn inc_mid_stream_failures(&self) {
        self.inner.mid_stream_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment number of pre-header refreshes.
    pub fn inc_pre_header_refreshes(&self) {
        self.inner.pre_header_refreshes.fetch_add(1, Ordering::Relaxed);
    }
}
