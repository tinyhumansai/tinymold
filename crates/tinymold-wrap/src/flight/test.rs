//! Unit tests for per-key locks between concurrent wrappers.

#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;

fn key() -> Digest {
    Digest::of_bytes(b"unit")
}

#[test]
fn a_free_key_is_claimed_at_once() {
    let store = tempfile::tempdir().unwrap();
    assert!(matches!(
        claim(store.path(), &key(), Duration::from_secs(1)),
        Claim::Held(_)
    ));
    assert!(lock_path(store.path(), &key()).is_file());
}

#[test]
fn a_held_key_is_waited_for_until_it_is_released() {
    let store = tempfile::tempdir().unwrap();
    let Claim::Held(first) = claim(store.path(), &key(), Duration::from_secs(1)) else {
        panic!("the first claim must hold");
    };
    std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(first);
        });
        match claim(store.path(), &key(), Duration::from_secs(30)) {
            Claim::Waited(_, waited) => assert!(waited > Duration::ZERO),
            other => panic!("expected to wait, got {other:?}"),
        }
    });
}

#[test]
fn a_wait_gives_up_at_the_limit() {
    let store = tempfile::tempdir().unwrap();
    let _first = claim(store.path(), &key(), Duration::from_secs(1));
    assert!(matches!(
        claim(store.path(), &key(), Duration::from_millis(20)),
        Claim::Unlocked
    ));
}

#[test]
fn a_store_that_cannot_hold_locks_leaves_the_compile_unlocked() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("plain");
    fs::write(&store, b"not a directory").unwrap();
    assert!(matches!(
        claim(&store, &key(), Duration::from_secs(1)),
        Claim::Unlocked
    ));
}

#[test]
fn a_lock_is_held_only_while_a_claim_lives() {
    let store = tempfile::tempdir().unwrap();
    let path = lock_path(store.path(), &key());
    assert!(!lock_held(&path), "a missing lock is not held");
    let first = claim(store.path(), &key(), Duration::from_secs(1));
    assert!(lock_held(&path));
    drop(first);
    assert!(!lock_held(&path));
}

#[cfg(unix)]
#[test]
fn an_unreadable_lock_counts_as_held() {
    use std::os::unix::fs::PermissionsExt;

    let store = tempfile::tempdir().unwrap();
    let path = lock_path(store.path(), &key());
    drop(claim(store.path(), &key(), Duration::from_secs(1)));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads anything; the rule only matters where permissions apply.
    if File::open(&path).is_err() {
        assert!(lock_held(&path));
    }
}
