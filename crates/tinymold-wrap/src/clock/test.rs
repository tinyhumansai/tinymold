//! Unit tests for the filesystem clock and the racily-clean rule.

#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn a_timestamps_tick_is_inferred_from_its_trailing_zeros() {
    assert_eq!(tick_of(1_234_567_891), 1);
    assert_eq!(tick_of(1_234_567_890), 10);
    // exFAT keeps hundredths of a second.
    assert_eq!(tick_of(1_230_000_000), 10_000_000);
    // A whole second may come from FAT, which keeps only even ones.
    assert_eq!(tick_of(5 * SECOND_NS), COARSEST_TICK_NS);
    assert_eq!(tick_of(0), COARSEST_TICK_NS);
}

#[test]
fn a_fine_timestamp_is_written_before_any_later_instant() {
    assert!(written_before(10 * SECOND_NS + 1, 10 * SECOND_NS + 2));
    assert!(!written_before(10 * SECOND_NS + 1, 10 * SECOND_NS + 1));
}

#[test]
fn a_whole_second_is_written_before_only_once_two_seconds_have_passed() {
    assert!(!written_before(10 * SECOND_NS, 10 * SECOND_NS + SECOND_NS / 2));
    assert!(!written_before(10 * SECOND_NS, 12 * SECOND_NS - 1));
    assert!(written_before(10 * SECOND_NS, 12 * SECOND_NS));
}

#[test]
fn a_timestamp_at_the_end_of_time_is_never_written_before() {
    assert!(!written_before(u64::MAX - 1, u64::MAX - 1));
}

#[test]
fn the_filesystem_clock_is_read_from_a_file_it_leaves_no_trace_of() {
    let dir = tempfile::tempdir().unwrap();
    let probes = dir.path().join("stat");
    let first = filesystem_now(&probes).unwrap();
    let second = filesystem_now(&probes).unwrap();
    assert!(first > 0);
    assert!(second >= first);
    assert_eq!(fs::read_dir(&probes).unwrap().count(), 0);
}

#[test]
fn the_filesystem_clock_fails_where_no_file_can_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("plain");
    fs::write(&file, b"not a directory").unwrap();
    assert!(filesystem_now(&file).is_err());
}

#[test]
fn nanos_saturate_before_the_epoch() {
    assert_eq!(nanos(UNIX_EPOCH - std::time::Duration::from_secs(1)), 0);
    assert_eq!(nanos(UNIX_EPOCH + std::time::Duration::from_nanos(7)), 7);
}
