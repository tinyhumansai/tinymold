//! Unit tests for the wrapper's trace lines: their exact encoding, which
//! `bench/profile.py` parses, and how steps are timed.

#![allow(clippy::unwrap_used)]

use super::*;

fn fixed() -> Trace {
    Trace {
        epoch_us: 1_760_443_200_000_000,
        origin: Instant::now(),
        spans: vec![
            Span {
                name: "pre_key",
                start_us: 15,
                dur_us: 812,
            },
            Span {
                name: "rustc",
                start_us: 900,
                dur_us: 41_000,
            },
        ],
    }
}

#[test]
fn each_step_is_one_chrome_trace_complete_event() {
    let lines = fixed().lines(4242, "syn", "miss");
    let lines: Vec<&str> = lines.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with(
        r#"{"name":"invocation","cat":"wrapper","ph":"X","ts":1760443200000000,"dur":"#
    ));
    assert_eq!(
        lines[1],
        r#"{"name":"pre_key","cat":"wrapper","ph":"X","ts":1760443200000015,"dur":812,"pid":4242,"tid":0,"args":{"crate":"syn","outcome":"miss"}}"#
    );
    assert_eq!(
        lines[2],
        r#"{"name":"rustc","cat":"wrapper","ph":"X","ts":1760443200000900,"dur":41000,"pid":4242,"tid":0,"args":{"crate":"syn","outcome":"miss"}}"#
    );
}

#[test]
fn labels_are_escaped_as_json_strings() {
    assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
    assert_eq!(escape("tab\there"), r"tab\u0009here");
    assert_eq!(escape("plain_crate"), "plain_crate");
}

#[test]
fn a_span_returns_its_value_and_records_in_order() {
    let mut trace = Trace::start();
    assert_eq!(trace.span("lookup", || 7), 7);
    trace.span("restore", || {
        std::thread::sleep(std::time::Duration::from_millis(2))
    });
    let names: Vec<_> = trace.spans.iter().map(|span| span.name).collect();
    assert_eq!(names, ["lookup", "restore"]);
    assert!(trace.spans[1].dur_us >= 2_000);
    assert!(trace.spans[1].start_us >= trace.spans[0].start_us);
}

#[test]
fn writing_appends_and_skips_an_unwritable_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    fixed().write(&path, "demo", "hit");
    fixed().write(&path, "demo", "hit");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 6);
    assert!(
        text.lines()
            .all(|line| line.ends_with(r#""outcome":"hit"}}"#))
    );

    // A directory is not a file: nothing is written and nothing panics.
    fixed().write(dir.path(), "demo", "hit");
}
