//! Unit tests for finding, storing and restoring incremental sessions.

#![allow(clippy::unwrap_used)]

use super::*;

/// An incremental directory holding one finished session of crate `demo`,
/// with an object file hardlinked into `out` as rustc does.
struct Sessions {
    _dir: tempfile::TempDir,
    incremental: PathBuf,
    out: PathBuf,
    store: Store,
}

const SESSION: &str = "s-hn7o7hfx6c-12wynqc-c5d16mu4pdgtlxvyqjr88pmeg";

impl Sessions {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let incremental = dir.path().join("incremental");
        let session = incremental.join("demo-2mhci14civbux").join(SESSION);
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("dep-graph.bin"), b"graph").unwrap();
        fs::write(session.join("0pw7qs82.o"), b"object").unwrap();
        fs::write(
            incremental.join("demo-2mhci14civbux/s-hn7o7hfx6c-12wynqc.lock"),
            b"",
        )
        .unwrap();
        let out = dir.path().join("deps");
        fs::create_dir_all(&out).unwrap();
        fs::hard_link(
            session.join("0pw7qs82.o"),
            out.join("demo-1.0pw7qs82.rcgu.o"),
        )
        .unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        Self {
            _dir: dir,
            incremental,
            out,
            store,
        }
    }

    fn output_inodes(&self) -> HashSet<(u64, u64)> {
        let meta = fs::metadata(self.out.join("demo-1.0pw7qs82.rcgu.o")).unwrap();
        HashSet::from([dev_ino(&meta)])
    }
}

#[test]
fn only_finished_session_directories_count() {
    assert!(is_finished(SESSION));
    assert!(!is_finished("s-hn7o7hfx6c-12wynqc.lock"));
    assert!(!is_finished("s-hn7o7hfx6c-12wynqc-working"));
    assert!(!is_finished("query-cache.bin"));
    assert!(!is_finished("s-too-few"));
}

#[cfg(unix)]
#[test]
fn the_units_session_is_the_one_sharing_an_inode_with_its_objects() {
    let sessions = Sessions::new();
    let inodes = sessions.output_inodes();
    // Written long after the session: only the inode identifies it.
    let later = u64::MAX;
    let (crate_dir, name) = find(&sessions.incremental, "demo", &inodes, later).unwrap();
    assert_eq!(crate_dir, sessions.incremental.join("demo-2mhci14civbux"));
    assert_eq!(name, SESSION);

    assert_eq!(find(&sessions.incremental, "other", &inodes, later), None);
    assert_eq!(
        find(&sessions.incremental, "demo", &HashSet::new(), later),
        None
    );
    assert_eq!(
        find(
            &sessions.incremental,
            "demo",
            &HashSet::from([(0, 1)]),
            later
        ),
        None
    );
    assert_eq!(
        find(&sessions.out.join("missing"), "demo", &inodes, later),
        None
    );
}

#[test]
fn without_objects_the_only_session_written_since_the_start_is_the_units() {
    let sessions = Sessions::new();
    let found = find(&sessions.incremental, "demo", &HashSet::new(), 0);
    assert_eq!(
        found,
        Some((
            sessions.incremental.join("demo-2mhci14civbux"),
            SESSION.to_owned()
        ))
    );

    // A second unit of the crate, compiled at the same time: no telling.
    let other = sessions
        .incremental
        .join("demo-0chpacq29fani/s-hn7o7hfx6d-1aaaaaa-0bbbbbbbbbbb");
    fs::create_dir_all(&other).unwrap();
    assert_eq!(
        find(&sessions.incremental, "demo", &HashSet::new(), 0),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_stored_session_is_restored_only_where_none_exists_at_its_own_path() {
    let sessions = Sessions::new();
    let crate_dir = sessions.incremental.join("demo-2mhci14civbux");
    let object = Digest::of_bytes(b"object");
    sessions.store.insert_bytes(b"object").unwrap();
    let known = HashMap::from([(sessions.output_inodes().into_iter().next().unwrap(), object)]);
    let record = store(&sessions.store, &crate_dir, SESSION, &known).unwrap();
    assert_eq!(
        record.files,
        vec![
            ("0pw7qs82.o".to_owned(), object),
            ("dep-graph.bin".to_owned(), Digest::of_bytes(b"graph")),
        ]
    );

    // Still there: rustc's own session is never replaced.
    assert!(!restore(&sessions.store, &record, &sessions.incremental));

    fs::remove_dir_all(&sessions.incremental).unwrap();
    assert!(!restore(&sessions.store, &record, &sessions.out));
    assert!(restore(&sessions.store, &record, &sessions.incremental));
    let restored = crate_dir.join(SESSION);
    assert_eq!(fs::read(restored.join("dep-graph.bin")).unwrap(), b"graph");
    assert_eq!(fs::read(restored.join("0pw7qs82.o")).unwrap(), b"object");
    assert!(crate_dir.join("s-hn7o7hfx6c-12wynqc.lock").is_file());
    // Writable, unlike the store's objects, since rustc rewrites a session.
    assert!(
        !fs::metadata(restored.join("dep-graph.bin"))
            .unwrap()
            .permissions()
            .readonly()
    );
}

#[cfg(unix)]
#[test]
fn a_session_whose_object_is_gone_leaves_nothing_behind() {
    let sessions = Sessions::new();
    let crate_dir = sessions.incremental.join("demo-2mhci14civbux");
    let record = store(&sessions.store, &crate_dir, SESSION, &HashMap::new()).unwrap();
    fs::remove_dir_all(&sessions.incremental).unwrap();
    fs::create_dir_all(&sessions.incremental).unwrap();
    assert!(sessions.store.remove(&Digest::of_bytes(b"graph")).unwrap());

    assert!(!restore(&sessions.store, &record, &sessions.incremental));
    assert!(!crate_dir.exists());
    assert!(!crate_dir.with_extension("tinymold-restoring").exists());
}
