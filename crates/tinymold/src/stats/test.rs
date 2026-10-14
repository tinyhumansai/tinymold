//! Unit tests for summarizing the wrapper log.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const LOG: &str = "\
hit serde 1.5ms \n\
remote tokio 40.0ms \n\
miss demo 1200.0ms stored in 3.0ms\n\
miss build_script_build 300.0ms not stored: rustc failed\n\
pass - 2.0ms not rustc\n\
error other 1.0ms failed to read x\n\
\n\
garbage\n\
hit x notatime\n\
strange y 1.0ms\n";

#[test]
fn counts_every_outcome_and_skips_lines_it_cannot_read() {
    let stats = Stats::parse(LOG);
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.remote_hits, 1);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.unstored, 1);
    assert_eq!(stats.passes, 1);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.unparsed, 3);
    assert!((stats.hit_millis - 41.5).abs() < 1e-9);
    assert!((stats.miss_millis - 1500.0).abs() < 1e-9);
}

#[test]
fn the_hit_rate_counts_cacheable_invocations_only() {
    let stats = Stats::parse(LOG);
    assert_eq!(stats.cacheable(), 5);
    assert!((stats.hit_rate().unwrap() - 40.0).abs() < 1e-9);
    assert_eq!(Stats::parse("pass - 1.0ms x\n").hit_rate(), None);
}

#[test]
fn keeps_the_slowest_misses_slowest_first() {
    let log = (1..=7)
        .map(|n| format!("miss c{n} {n}00.0ms"))
        .collect::<Vec<_>>()
        .join("\n");
    let stats = Stats::parse(&log);
    let names: Vec<_> = stats
        .slowest
        .iter()
        .map(|m| m.crate_name.as_str())
        .collect();
    assert_eq!(names, ["c7", "c6", "c5", "c4", "c3"]);
}

#[test]
fn renders_text_and_markdown() {
    let stats = Stats::parse(LOG);
    assert_eq!(
        stats.to_string(),
        "40.0% hit rate: 2 of 5 cacheable compiles served from cache\n\
         hits 2 (remote 1), misses 2 (1 not stored), not cacheable 1, errors 1\n\
         time on hits 0.04s, on misses 1.50s"
    );
    let markdown = stats.markdown();
    assert!(
        markdown.starts_with("### tinymold\n\n40.0% hit rate"),
        "{markdown}"
    );
    assert!(markdown.contains("| Remote hits | 1 |  |"), "{markdown}");
    assert!(
        markdown.contains("| Misses (compiled) | 2 | 1.50s |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("Slowest misses: `demo` 1.20s, `build_script_build` 0.30s"),
        "{markdown}"
    );
}

#[test]
fn an_empty_log_says_so() {
    let stats = Stats::parse("");
    assert_eq!(stats, Stats::default());
    assert!(stats.to_string().starts_with("no cacheable compiles"));
    assert!(!stats.markdown().contains("Slowest"));
}

#[test]
fn test_runs_are_counted_apart_from_compiles() {
    let log = "hit demo 1.0ms\n\
               test-hit demo 0.8ms\n\
               test-hit other 0.9ms\n\
               test-miss demo 120.0ms recorded in 0.3ms\n\
               test-pass - 0.1ms a bench run\n\
               test-error - 0.1ms failed to read x\n";
    let stats = Stats::parse(log);
    assert_eq!(stats.hits, 1);
    assert_eq!(stats.cacheable(), 1);
    assert_eq!(stats.unparsed, 0);
    assert_eq!(
        (stats.test_hits, stats.test_misses, stats.test_passes, stats.test_errors),
        (2, 1, 1, 1)
    );
    let text = stats.to_string();
    assert!(
        text.contains("tests replayed 2, run 1, not cacheable 1, errors 1"),
        "{text}"
    );
    assert!(stats.markdown().contains("| Test runs replayed | 2 |"));

    // A log without test runs says nothing about them.
    let compiles = Stats::parse("hit demo 1.0ms\n");
    assert!(!compiles.to_string().contains("tests"));
    assert!(!compiles.markdown().contains("Test runs"));
}
