//! Unit tests for the metrics registry.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn requests_are_counted_per_front_end_and_outcome() {
    let metrics = Metrics::default();
    metrics.record(FrontEnd::Turbo, Outcome::Hit, 0, 100);
    metrics.record(FrontEnd::Turbo, Outcome::Miss, 0, 0);
    metrics.record(FrontEnd::Nx, Outcome::Write, 40, 0);
    let text = metrics.render();

    assert!(text.contains("tinymold_requests_total{front_end=\"turbo\"} 2"));
    assert!(text.contains("tinymold_hits_total{front_end=\"turbo\"} 1"));
    assert!(text.contains("tinymold_misses_total{front_end=\"turbo\"} 1"));
    assert!(text.contains("tinymold_writes_total{front_end=\"nx\"} 1"));
    assert!(text.contains("tinymold_received_bytes_total{front_end=\"nx\"} 40"));
    assert!(text.contains("tinymold_sent_bytes_total{front_end=\"turbo\"} 100"));
}

#[test]
fn latency_lands_in_every_bucket_at_or_above_it() {
    let metrics = Metrics::default();
    metrics.observe(Duration::from_micros(800));
    let text = metrics.render();

    assert!(text.contains("tinymold_request_duration_seconds_bucket{le=\"0.0005\"} 0"));
    assert!(text.contains("tinymold_request_duration_seconds_bucket{le=\"0.001\"} 1"));
    assert!(text.contains("tinymold_request_duration_seconds_bucket{le=\"+Inf\"} 1"));
    assert!(text.contains("tinymold_request_duration_seconds_count 1"));
}

#[test]
fn a_collection_sets_the_store_gauges() {
    let metrics = Metrics::default();
    metrics.record_collection(&Report {
        entries_kept: 7,
        bytes_kept: 4096,
        entries_removed: 2,
        ..Report::default()
    });
    let text = metrics.render();

    assert!(text.contains("tinymold_store_entries 7"));
    assert!(text.contains("tinymold_store_bytes 4096"));
    assert!(text.contains("tinymold_evicted_entries_total 2"));
}
