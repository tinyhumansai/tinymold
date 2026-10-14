//! Unit tests for scheduling automatic collections.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn a_collection_is_due_once_per_interval() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    assert!(claim(dir.path(), now));
    assert!(!claim(dir.path(), now + Duration::from_secs(1)));
    assert!(!claim(dir.path(), now + INTERVAL - Duration::from_secs(1)));
    assert!(claim(dir.path(), now + INTERVAL + Duration::from_secs(1)));
}

#[test]
fn a_store_that_does_not_exist_is_never_collected() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!claim(&dir.path().join("missing"), SystemTime::now()));
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn a_stamp_from_the_future_does_not_claim_a_collection() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    assert!(claim(dir.path(), now + 10 * INTERVAL));
    assert!(!claim(dir.path(), now));
}

#[test]
fn automatic_collection_is_on_unless_switched_off() {
    let set =
        |value: &'static str| move |name: &str| (name == "TINYMOLD_AUTO_GC").then(|| value.into());
    assert!(enabled(|_| None));
    assert!(enabled(set("1")));
    for off in ["0", "false", "No", "OFF"] {
        assert!(!enabled(set(off)), "{off}");
    }
}

#[test]
fn the_collection_keeps_recent_generations_of_the_given_store() {
    let args = arguments(Path::new("/stores/main"));
    let parsed = <crate::Cli as clap::Parser>::try_parse_from(
        std::iter::once(OsString::from("tinymold")).chain(args),
    )
    .unwrap();
    assert_eq!(parsed.store.as_deref(), Some(Path::new("/stores/main")));
    match parsed.command {
        crate::Command::Gc(gc) => assert_eq!(gc.keep_generations, Some(KEEP_GENERATIONS)),
        other => panic!("expected gc, got {other:?}"),
    }
}
