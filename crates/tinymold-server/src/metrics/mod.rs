//! Counters the server exposes at `/metrics`, in Prometheus text format.
//!
//! Counters are per front-end, never per tenant or credential, so the
//! endpoint can stay unauthenticated without saying who uses the cache.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tinymold_store::{FrontEnd, Report};

/// Upper bounds of the request latency buckets, in seconds.
const BUCKETS: [f64; 10] = [
    0.000_25, 0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0,
];

/// What one request did, for counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A lookup that found its entry or blob.
    Hit,
    /// A lookup that did not.
    Miss,
    /// An upload that was stored.
    Write,
    /// A request refused for its credential.
    Denied,
    /// Anything else: a status query, a rejected body.
    Other,
}

#[derive(Debug, Default)]
struct Counters {
    requests: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    writes: AtomicU64,
    denied: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
}

/// The server's metrics.
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    front_ends: [Counters; FrontEnd::ALL.len()],
    buckets: [AtomicU64; BUCKETS.len()],
    latency_count: AtomicU64,
    latency_micros: AtomicU64,
    store_entries: AtomicU64,
    store_bytes: AtomicU64,
    evicted: AtomicU64,
}

impl Metrics {
    /// Counts one request through `front_end`.
    pub(crate) fn record(
        &self,
        front_end: FrontEnd,
        outcome: Outcome,
        bytes_in: u64,
        bytes_out: u64,
    ) {
        let Some(index) = FrontEnd::ALL.iter().position(|front| *front == front_end) else {
            return;
        };
        let counters = &self.front_ends[index];
        counters.requests.fetch_add(1, Ordering::Relaxed);
        let counter = match outcome {
            Outcome::Hit => Some(&counters.hits),
            Outcome::Miss => Some(&counters.misses),
            Outcome::Write => Some(&counters.writes),
            Outcome::Denied => Some(&counters.denied),
            Outcome::Other => None,
        };
        if let Some(counter) = counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        counters.bytes_in.fetch_add(bytes_in, Ordering::Relaxed);
        counters.bytes_out.fetch_add(bytes_out, Ordering::Relaxed);
    }

    /// Records how long one request took to answer.
    pub(crate) fn observe(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        for (bound, bucket) in BUCKETS.iter().zip(&self.buckets) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.latency_micros.fetch_add(micros, Ordering::Relaxed);
    }

    /// Records the store's size after a collection.
    pub(crate) fn record_collection(&self, report: &Report) {
        self.store_entries
            .store(report.entries_kept, Ordering::Relaxed);
        self.store_bytes.store(report.bytes_kept, Ordering::Relaxed);
        self.evicted
            .fetch_add(report.entries_removed, Ordering::Relaxed);
    }

    /// The metrics in Prometheus text exposition format.
    #[must_use]
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        let families: [(&str, &str, fn(&Counters) -> &AtomicU64); 7] = [
            ("tinymold_requests_total", "Requests received.", |c| {
                &c.requests
            }),
            (
                "tinymold_hits_total",
                "Lookups that found their entry or blob.",
                |c| &c.hits,
            ),
            ("tinymold_misses_total", "Lookups that did not.", |c| {
                &c.misses
            }),
            ("tinymold_writes_total", "Uploads stored.", |c| &c.writes),
            (
                "tinymold_denied_total",
                "Requests refused for their credential.",
                |c| &c.denied,
            ),
            (
                "tinymold_received_bytes_total",
                "Body bytes received.",
                |c| &c.bytes_in,
            ),
            ("tinymold_sent_bytes_total", "Body bytes sent.", |c| {
                &c.bytes_out
            }),
        ];
        for (name, help, counter) in families {
            let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} counter");
            for (front_end, counters) in FrontEnd::ALL.iter().zip(&self.front_ends) {
                let value = counter(counters).load(Ordering::Relaxed);
                let _ = writeln!(out, "{name}{{front_end=\"{front_end}\"}} {value}");
            }
        }

        let name = "tinymold_request_duration_seconds";
        let _ = writeln!(
            out,
            "# HELP {name} Time to answer a request.\n# TYPE {name} histogram"
        );
        for (bound, bucket) in BUCKETS.iter().zip(&self.buckets) {
            let value = bucket.load(Ordering::Relaxed);
            let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {value}");
        }
        let count = self.latency_count.load(Ordering::Relaxed);
        let micros = self.latency_micros.load(Ordering::Relaxed);
        let (seconds, fraction) = (micros / 1_000_000, micros % 1_000_000);
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {count}");
        let _ = writeln!(
            out,
            "{name}_sum {seconds}.{fraction:06}\n{name}_count {count}"
        );

        for (name, help, kind, value) in [
            (
                "tinymold_store_entries",
                "Entries kept at the last collection.",
                "gauge",
                &self.store_entries,
            ),
            (
                "tinymold_store_bytes",
                "Object bytes kept at the last collection.",
                "gauge",
                &self.store_bytes,
            ),
            (
                "tinymold_evicted_entries_total",
                "Entries removed by collections.",
                "counter",
                &self.evicted,
            ),
        ] {
            let value = value.load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}"
            );
        }
        out
    }
}

#[cfg(test)]
mod test;
