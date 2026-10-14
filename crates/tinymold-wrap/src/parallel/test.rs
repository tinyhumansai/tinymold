//! Unit tests for running per-item work on scoped worker threads.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Mutex;

use super::*;
use crate::Result;

#[test]
fn every_item_is_mapped_and_results_keep_the_item_order() {
    let items: Vec<u32> = (0..100).collect();
    let doubled = map(&items, "doubling", |n| Ok::<_, Error>(n * 2)).unwrap();
    assert_eq!(doubled, items.iter().map(|n| n * 2).collect::<Vec<_>>());
}

#[test]
fn items_are_spread_over_more_than_one_thread() {
    if workers() < 2 {
        return;
    }
    let items: Vec<u32> = (0..64).collect();
    let threads = Mutex::new(HashSet::new());
    map(&items, "recording", |_| {
        threads.lock().unwrap().insert(std::thread::current().id());
        Ok::<_, Error>(())
    })
    .unwrap();
    assert!(threads.into_inner().unwrap().len() > 1);
}

#[test]
fn no_items_need_no_work() {
    let none: Vec<u32> = Vec::new();
    let mapped = map(&none, "nothing", |_| -> Result<u32> { panic!("no items") }).unwrap();
    assert!(mapped.is_empty());
}

#[test]
fn the_first_failing_item_fails_the_whole_map() {
    let items: Vec<u32> = (0..100).collect();
    let error = map(&items, "failing", |n| {
        if *n == 37 || *n == 90 {
            Err(Error::CorruptRecord {
                path: "r".into(),
                line: *n as usize,
            })
        } else {
            Ok(*n)
        }
    })
    .unwrap_err();
    assert!(
        matches!(error, Error::CorruptRecord { line: 37, .. }),
        "{error}"
    );
}

#[test]
fn a_panicking_worker_is_an_error_naming_the_task() {
    let items: Vec<u32> = (0..100).collect();
    let error = map(&items, "restoring outputs", |n| {
        if *n == 50 {
            panic!("worker gave up");
        }
        Ok::<_, Error>(*n)
    })
    .unwrap_err();
    assert!(
        matches!(
            error,
            Error::WorkerPanicked {
                task: "restoring outputs"
            }
        ),
        "{error}"
    );
}

#[test]
fn a_panic_after_another_worker_failed_is_still_an_error() {
    let items: Vec<u32> = (0..100).collect();
    let error = map(&items, "mixed", |n| {
        if *n == 0 {
            return Err(Error::CorruptRecord {
                path: "r".into(),
                line: 1,
            });
        }
        if *n == 99 {
            panic!("worker gave up");
        }
        Ok(*n)
    })
    .unwrap_err();
    assert!(matches!(error, Error::CorruptRecord { .. }), "{error}");
}
