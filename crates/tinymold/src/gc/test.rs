//! Unit tests for garbage collection, against hand-built stores.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::path::PathBuf;

use super::*;

const DAY: Duration = Duration::from_secs(86_400);

/// A store with helpers to add back-dated records and objects.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    now: SystemTime,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let store = Store::open(&root).unwrap();
        Self {
            _dir: dir,
            root,
            store,
            now: SystemTime::now(),
        }
    }

    /// Stores `bytes` as an object committed `age` ago.
    fn object(&self, bytes: &[u8], age: Duration) -> Digest {
        let digest = self.store.insert_bytes(bytes).unwrap();
        backdate(&self.store.object_path(&digest), self.now - age);
        digest
    }

    /// Writes an action naming `outputs`, last used `age` ago.
    fn action(&self, name: &str, outputs: &[Digest], age: Duration) -> PathBuf {
        let empty = self.object(b"", 2 * DAY);
        let mut text = format!("tinymold-action 1\nstdout {empty}\nstderr {empty}\n");
        for (index, digest) in outputs.iter().enumerate() {
            writeln!(text, "output {digest} 0 0 out{index}").unwrap();
        }
        let path = self
            .root
            .join("entries/demo")
            .join(Digest::of_bytes(name.as_bytes()).to_hex());
        write(&path, &text, self.now - age);
        path
    }

    fn policy(&self) -> Policy {
        Policy {
            now: self.now,
            ..Policy::garbage_only()
        }
    }
}

fn write(path: &Path, text: &str, modified: SystemTime) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    backdate(path, modified);
}

fn backdate(path: &Path, when: SystemTime) {
    fs::File::open(path).unwrap().set_modified(when).unwrap();
}

#[test]
fn ages_parse_with_every_unit_and_reject_anything_else() {
    assert_eq!("90s".parse::<Age>().unwrap().0, Duration::from_secs(90));
    assert_eq!("30m".parse::<Age>().unwrap().0, Duration::from_secs(1800));
    assert_eq!("36h".parse::<Age>().unwrap().0, Duration::from_secs(129_600));
    assert_eq!("14d".parse::<Age>().unwrap().0, 14 * DAY);
    for bad in ["", "14", "d", "14w", "-1d", "1.5d", "99999999999999999999d"] {
        assert!(
            matches!(bad.parse::<Age>(), Err(Error::InvalidDuration(text)) if text == bad),
            "{bad:?} parsed"
        );
    }
}

#[test]
fn sizes_parse_plain_or_with_a_binary_suffix_and_reject_anything_else() {
    assert_eq!("512".parse::<ByteSize>().unwrap().0, 512);
    assert_eq!("512B".parse::<ByteSize>().unwrap().0, 512);
    assert_eq!("4K".parse::<ByteSize>().unwrap().0, 4096);
    assert_eq!("3M".parse::<ByteSize>().unwrap().0, 3 << 20);
    assert_eq!("10G".parse::<ByteSize>().unwrap().0, 10 << 30);
    assert_eq!("2T".parse::<ByteSize>().unwrap().0, 2 << 40);
    for bad in ["", "G", "10GB", "1.5G", "-1", "99999999999T"] {
        assert!(
            matches!(bad.parse::<ByteSize>(), Err(Error::InvalidSize(text)) if text == bad),
            "{bad:?} parsed"
        );
    }
}

#[test]
fn sizes_display_in_the_largest_fitting_unit() {
    assert_eq!(ByteSize(0).to_string(), "0 B");
    assert_eq!(ByteSize(1023).to_string(), "1023 B");
    assert_eq!(ByteSize(1536).to_string(), "1.5 KiB");
    assert_eq!(ByteSize(5 << 30).to_string(), "5.0 GiB");
    assert_eq!(ByteSize(3 << 50).to_string(), "3072.0 TiB");
}

#[test]
fn an_action_unused_past_the_cutoff_goes_and_a_recent_one_stays() {
    let fixture = Fixture::new();
    let old_output = fixture.object(b"old rlib", 3 * DAY);
    let new_output = fixture.object(b"new rlib", 3 * DAY);
    let old = fixture.action("old", &[old_output], 2 * DAY);
    let new = fixture.action("new", &[new_output], Duration::from_secs(60));

    let report = collect(
        &fixture.root,
        &Policy {
            max_age: Some(DAY),
            ..fixture.policy()
        },
    )
    .unwrap();

    assert!(!old.exists());
    assert!(new.exists());
    assert!(!fixture.store.contains(&old_output));
    assert!(fixture.store.contains(&new_output));
    assert_eq!(report.actions_removed, 1);
    assert_eq!(report.actions_kept, 1);
    assert_eq!(report.objects_removed.objects, 1);
    assert_eq!(report.objects_removed.bytes.0, 8);
}

#[test]
fn an_object_shared_by_two_actions_survives_until_neither_is_kept() {
    let fixture = Fixture::new();
    let shared = fixture.object(b"shared", 3 * DAY);
    fixture.action("old", &[shared], 2 * DAY);
    fixture.action("new", &[shared], Duration::from_secs(60));
    let policy = Policy {
        max_age: Some(DAY),
        ..fixture.policy()
    };

    collect(&fixture.root, &policy).unwrap();
    assert!(fixture.store.contains(&shared));

    let later = Policy {
        now: fixture.now + 2 * DAY,
        ..policy
    };
    collect(&fixture.root, &later).unwrap();
    assert!(!fixture.store.contains(&shared));
}

#[test]
fn a_size_budget_keeps_the_most_recently_used_actions_that_fit() {
    let fixture = Fixture::new();
    let a = fixture.object(&[b'a'; 100], 3 * DAY);
    let b = fixture.object(&[b'b'; 100], 3 * DAY);
    let c = fixture.object(&[b'c'; 100], 3 * DAY);
    let oldest = fixture.action("oldest", &[a], Duration::from_secs(300));
    let middle = fixture.action("middle", &[b], Duration::from_secs(200));
    let newest = fixture.action("newest", &[c], Duration::from_secs(100));

    let report = collect(
        &fixture.root,
        &Policy {
            max_size: Some(250),
            ..fixture.policy()
        },
    )
    .unwrap();

    assert!(!oldest.exists());
    assert!(middle.exists());
    assert!(newest.exists());
    assert!(!fixture.store.contains(&a));
    assert_eq!(report.actions_removed, 1);
    assert_eq!(report.actions_kept, 2);
}

#[test]
fn evicting_for_size_frees_only_objects_no_kept_action_shares() {
    let fixture = Fixture::new();
    let shared = fixture.object(&[b's'; 100], 3 * DAY);
    let own = fixture.object(&[b'o'; 100], 3 * DAY);
    fixture.action("old", &[shared, own], Duration::from_secs(300));
    fixture.action("new", &[shared], Duration::from_secs(100));

    collect(
        &fixture.root,
        &Policy {
            max_size: Some(150),
            ..fixture.policy()
        },
    )
    .unwrap();

    assert!(fixture.store.contains(&shared));
    assert!(!fixture.store.contains(&own));
}

#[test]
fn a_young_unreferenced_object_is_left_for_an_ingest_in_progress() {
    let fixture = Fixture::new();
    let young = fixture.object(b"being stored", Duration::from_secs(60));
    let orphan = fixture.object(b"orphaned", 2 * DAY);

    let report = collect(&fixture.root, &fixture.policy()).unwrap();

    assert!(fixture.store.contains(&young));
    assert!(!fixture.store.contains(&orphan));
    assert_eq!(report.objects_kept.objects, 1);
    assert_eq!(report.objects_removed.objects, 1);
}

#[test]
fn a_dry_run_reports_the_same_numbers_and_removes_nothing() {
    let fixture = Fixture::new();
    let output = fixture.object(b"rlib", 3 * DAY);
    let action = fixture.action("old", &[output], 2 * DAY);
    let policy = Policy {
        max_age: Some(DAY),
        ..fixture.policy()
    };

    let dry = collect(
        &fixture.root,
        &Policy {
            dry_run: true,
            ..policy
        },
    )
    .unwrap();
    assert!(action.exists());
    assert!(fixture.store.contains(&output));
    assert!(dry.to_string().starts_with("gc: would remove 1 actions"));

    let real = collect(&fixture.root, &policy).unwrap();
    assert_eq!(
        Report {
            dry_run: true,
            ..real
        },
        dry
    );
    assert!(real.to_string().starts_with("gc: removed 1 actions"));
}

#[test]
fn old_pre_keys_caches_and_temporaries_go_and_corrupt_actions_are_dropped() {
    let fixture = Fixture::new();
    let old = fixture.now - 2 * DAY;
    let recent = fixture.now - Duration::from_secs(60);
    let prekey = fixture
        .root
        .join("prekeys")
        .join(Digest::of_bytes(b"pre").to_hex());
    let fresh_prekey = fixture
        .root
        .join("prekeys")
        .join(Digest::of_bytes(b"fresh").to_hex());
    let stat = fixture.root.join("stat/ab/cdef");
    let temp = fixture.root.join("prekeys/.abc.1.tmp");
    let staging = fixture.root.join("tmp/1-0.tmp");
    let corrupt = fixture
        .root
        .join("entries/demo")
        .join(Digest::of_bytes(b"bad").to_hex());
    for path in [&prekey, &stat, &temp, &staging] {
        write(path, "x", old);
    }
    write(&fresh_prekey, "x", recent);
    write(&corrupt, "not an action", recent);

    let report = collect(
        &fixture.root,
        &Policy {
            max_age: Some(DAY),
            ..fixture.policy()
        },
    )
    .unwrap();

    for path in [&prekey, &stat, &temp, &staging, &corrupt] {
        assert!(!path.exists(), "{} survived", path.display());
    }
    assert!(fresh_prekey.exists());
    assert_eq!(report.prekeys_removed, 1);
    assert_eq!(report.caches_removed, 3);
    assert_eq!(report.actions_removed, 1);
}

#[test]
fn without_limits_only_garbage_goes() {
    let fixture = Fixture::new();
    let output = fixture.object(b"rlib", 300 * DAY);
    let action = fixture.action("ancient", &[output], 300 * DAY);

    let report = collect(&fixture.root, &fixture.policy()).unwrap();

    assert!(action.exists());
    assert!(fixture.store.contains(&output));
    assert_eq!(report.actions_kept, 1);
}

#[test]
fn snapshots_older_than_their_cutoff_go() {
    let fixture = Fixture::new();
    let dir = snapshot::snapshots_dir(&fixture.root);
    let old = dir.join("old");
    let new = dir.join("new");
    write(
        &crate::fresh::manifest_path(&old),
        "manifest",
        fixture.now - 10 * DAY,
    );
    write(&crate::fresh::manifest_path(&new), "manifest", fixture.now);
    fs::create_dir_all(dir.join(".kept-aside")).unwrap();

    let keep_all = collect(&fixture.root, &fixture.policy()).unwrap();
    assert_eq!(keep_all.snapshots_removed, 0);

    let report = collect(
        &fixture.root,
        &Policy {
            snapshots_max_age: Some(7 * DAY),
            ..fixture.policy()
        },
    )
    .unwrap();

    assert_eq!(report.snapshots_removed, 1);
    assert!(!old.exists());
    assert!(new.exists());
    assert!(dir.join(".kept-aside").exists());
}

#[test]
fn snapshot_expiry_without_a_snapshots_directory_removes_nothing() {
    let fixture = Fixture::new();
    let report = collect(
        &fixture.root,
        &Policy {
            snapshots_max_age: Some(DAY),
            ..fixture.policy()
        },
    )
    .unwrap();
    assert_eq!(report.snapshots_removed, 0);
}

#[test]
fn an_unreadable_snapshots_path_is_an_error() {
    let fixture = Fixture::new();
    write(&snapshot::snapshots_dir(&fixture.root), "a file", fixture.now);
    let result = collect(
        &fixture.root,
        &Policy {
            snapshots_max_age: Some(DAY),
            ..fixture.policy()
        },
    );
    assert!(matches!(result, Err(Error::Io { action: "read", .. })));
}

#[cfg(unix)]
#[test]
fn a_record_that_cannot_be_removed_is_an_error() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let output = fixture.object(b"rlib", 3 * DAY);
    let action = fixture.action("old", &[output], 2 * DAY);
    let crate_dir = action.parent().unwrap().to_path_buf();
    fs::set_permissions(&crate_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let result = collect(
        &fixture.root,
        &Policy {
            max_age: Some(DAY),
            ..fixture.policy()
        },
    );
    fs::set_permissions(&crate_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(result, Err(Error::Io { action: "remove", .. })));
}

#[test]
fn usage_counts_objects_and_bytes() {
    let fixture = Fixture::new();
    fixture.object(b"abc", DAY);
    fixture.object(b"de", DAY);
    let usage = usage(&fixture.root).unwrap();
    assert_eq!(usage.objects, 2);
    assert_eq!(usage.bytes.0, 5);
    assert_eq!(usage.to_string(), "2 objects, 5 B");
}

#[test]
fn usage_of_a_missing_store_is_zero_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none");
    assert_eq!(usage(&missing).unwrap(), StoreUsage::default());
    assert!(!missing.exists());
}
