//! Unit tests for serving invocations, run against the real rustc.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use tinymold_key::Classified;

use crate::remote::{Remote, RemoteError};

use super::*;

/// The rustc cargo is testing with.
fn rustc() -> PathBuf {
    std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from)
}

/// A one-file crate with its own out-dir and store.
struct Crate {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
}

impl Crate {
    fn new(source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        fs::write(root.join("src/lib.rs"), source).unwrap();
        let store = root.join("store");
        Self {
            _dir: dir,
            root,
            store,
        }
    }

    fn out(&self) -> PathBuf {
        self.root.join("target/debug/deps")
    }

    fn args(&self, extra: &[&str]) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--crate-name",
            "demo",
            "--edition=2021",
            "--crate-type",
            "lib",
            "--emit=dep-info,metadata,link",
            "--error-format=json",
            "--json=artifacts",
            "-C",
            "extra-filename=-1",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.push("--out-dir".into());
        args.push(self.out().into());
        args.push(self.root.join("src/lib.rs").into());
        args.extend(extra.iter().map(OsString::from));
        args
    }

    fn serve(&self, extra: &[&str]) -> Result<Served> {
        self.serve_with(None, extra)
    }

    fn serve_waiting(&self, wait: Duration, extra: &[&str]) -> Result<Served> {
        self.serve_full(None, wait, extra)
    }

    fn serve_with(&self, remote: Option<&dyn Remote>, extra: &[&str]) -> Result<Served> {
        self.serve_full(remote, LOCK_WAIT, extra)
    }

    fn serve_full(
        &self,
        remote: Option<&dyn Remote>,
        wait: Duration,
        extra: &[&str],
    ) -> Result<Served> {
        let args = self.args(extra);
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect();
        let Classified::Cacheable(invocation) = Invocation::parse(text) else {
            panic!("the test compile must be cacheable");
        };
        serve(
            &self.store,
            remote,
            &rustc(),
            &args,
            &invocation,
            wait,
            &mut Trace::start(),
        )
    }

    fn outputs(&self) -> Vec<(String, Vec<u8>)> {
        let mut found: Vec<_> = fs::read_dir(self.out())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, fs::read(&path).unwrap())
            })
            .collect();
        found.sort();
        found
    }

    fn clear_outputs(&self) {
        fs::remove_dir_all(self.out()).unwrap();
        fs::create_dir_all(self.out()).unwrap();
    }
}

fn assert_stored(served: &Served) {
    match served {
        Served::Miss {
            code: 0,
            stored: Ok(_),
            ..
        } => {}
        other => panic!("expected a stored miss, got {other:?}"),
    }
}

#[test]
fn a_miss_is_stored_and_the_same_compile_then_hits_with_identical_outputs() {
    let unit = Crate::new("pub fn answer() -> u32 { 42 }\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let built = unit.outputs();
    let names: Vec<_> = built.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["demo-1.d", "libdemo-1.rlib", "libdemo-1.rmeta"]);

    unit.clear_outputs();
    assert!(matches!(unit.serve(&[]).unwrap(), Served::Hit { .. }));
    assert_eq!(unit.outputs(), built);
}

#[test]
fn a_changed_source_misses_and_both_versions_then_hit() {
    let unit = Crate::new("pub fn answer() -> u32 { 1 }\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let first = unit.outputs();

    fs::write(
        unit.root.join("src/lib.rs"),
        "pub fn answer() -> u32 { 2 }\n",
    )
    .unwrap();
    assert_stored(&unit.serve(&[]).unwrap());
    assert_ne!(unit.outputs(), first);

    fs::write(
        unit.root.join("src/lib.rs"),
        "pub fn answer() -> u32 { 1 }\n",
    )
    .unwrap();
    assert!(matches!(unit.serve(&[]).unwrap(), Served::Hit { .. }));
    assert_eq!(unit.outputs(), first);
}

#[test]
fn a_failed_compile_is_not_stored() {
    let unit = Crate::new("pub fn broken( {\n");
    match unit.serve(&[]).unwrap() {
        Served::Miss { code, stored, .. } => {
            assert_ne!(code, 0);
            assert_eq!(stored.unwrap_err(), "rustc failed");
        }
        Served::Hit { .. } | Served::RemoteHit { .. } => panic!("a failing compile cannot hit"),
    }
}

#[test]
fn a_compile_without_dep_info_is_not_stored() {
    let unit = Crate::new("pub fn f() {}\n");
    let args = unit.args(&[]);
    let mut text: Vec<String> = args
        .iter()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect();
    text[5] = "--emit=metadata,link".to_owned();
    let Classified::Cacheable(invocation) = Invocation::parse(text.clone()) else {
        panic!("cacheable");
    };
    let args: Vec<OsString> = text.into_iter().map(OsString::from).collect();
    match serve(
        &unit.store,
        None,
        &rustc(),
        &args,
        &invocation,
        LOCK_WAIT,
        &mut Trace::start(),
    )
    .unwrap()
    {
        Served::Miss {
            code: 0, stored, ..
        } => assert_eq!(stored.unwrap_err(), "no dep-info"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_input_modified_during_the_compile_is_not_stored() {
    let unit = Crate::new("pub fn f() {}\n");
    let future = SystemTime::now() + Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(unit.root.join("src/lib.rs"))
        .unwrap()
        .set_modified(future)
        .unwrap();
    match unit.serve(&[]).unwrap() {
        Served::Miss {
            code: 0, stored, ..
        } => {
            assert_eq!(stored.unwrap_err(), "an input changed during the compile");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_output_that_is_a_directory_is_not_stored() {
    let unit = Crate::new("pub fn f() {}\n");
    fs::create_dir_all(unit.out().join("demo-1.dSYM")).unwrap();
    match unit.serve(&[]).unwrap() {
        Served::Miss {
            code: 0, stored, ..
        } => {
            assert_eq!(stored.unwrap_err(), "an output is not a regular file");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_hit_whose_objects_are_gone_runs_rustc_again() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    fs::remove_dir_all(unit.store.join("objects")).unwrap();
    unit.clear_outputs();
    assert_stored(&unit.serve(&[]).unwrap());
    assert!(unit.out().join("libdemo-1.rlib").is_file());
}

#[test]
fn a_missing_extern_is_an_error_before_rustc_runs() {
    let unit = Crate::new("pub fn f() {}\n");
    let missing = format!("dep={}", unit.root.join("libdep.rlib").display());
    let error = unit.serve(&["--extern", &missing]).unwrap_err();
    assert!(matches!(error, Error::Io { action: "read", .. }), "{error}");
}

#[test]
fn native_libraries_are_part_of_the_key() {
    let unit = Crate::new("pub fn f() {}\n");
    let native = unit.root.join("native");
    fs::create_dir_all(&native).unwrap();
    fs::write(native.join("libthing.a"), b"one").unwrap();
    let flag = format!("native={}", native.display());
    assert_stored(&unit.serve(&["-L", &flag]).unwrap());
    assert!(matches!(
        unit.serve(&["-L", &flag]).unwrap(),
        Served::Hit { .. }
    ));

    fs::write(native.join("libthing.a"), b"two").unwrap();
    assert_stored(&unit.serve(&["-L", &flag]).unwrap());

    let absent = format!("native={}", unit.root.join("absent").display());
    assert_stored(&unit.serve(&["-L", &absent]).unwrap());
}

#[cfg(unix)]
#[test]
fn a_restored_binary_is_executable_and_fresh() {
    use std::os::unix::fs::PermissionsExt;

    let unit = Crate::new("fn main() { println!(\"hi\"); }\n");
    let args = unit.args(&[]);
    let mut text: Vec<String> = args
        .iter()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect();
    text[4] = "bin".to_owned();
    text[5] = "--emit=dep-info,link".to_owned();
    let Classified::Cacheable(invocation) = Invocation::parse(text.clone()) else {
        panic!("cacheable");
    };
    let args: Vec<OsString> = text.into_iter().map(OsString::from).collect();
    assert_stored(
        &serve(
            &unit.store,
            None,
            &rustc(),
            &args,
            &invocation,
            LOCK_WAIT,
            &mut Trace::start(),
        )
        .unwrap(),
    );
    unit.clear_outputs();
    let before = SystemTime::now() - Duration::from_secs(1);
    assert!(matches!(
        serve(
            &unit.store,
            None,
            &rustc(),
            &args,
            &invocation,
            LOCK_WAIT,
            &mut Trace::start()
        )
        .unwrap(),
        Served::Hit { .. }
    ));
    let binary = unit.out().join("demo-1");
    let meta = fs::metadata(&binary).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o755);
    assert!(meta.modified().unwrap() > before);
    let output = std::process::Command::new(&binary).output().unwrap();
    assert_eq!(output.stdout, b"hi\n");
}

#[test]
fn outputs_rustc_reuses_from_its_incremental_cache_are_stored_too() {
    // Incremental rustc hardlinks unchanged artifacts (objects, and sometimes
    // the .rmeta) back into the out-dir with their old mtimes. A hit must
    // still restore every one of them, or a stale .rmeta from another branch
    // survives next to a restored .rlib and dependents link mismatched crates.
    let unit = Crate::new("pub fn a() -> u32 { 1 }\npub fn b() -> u32 { 2 }\n");
    let incremental = format!("incremental={}", unit.root.join("incremental").display());
    let flags = [
        "-C",
        incremental.as_str(),
        "-C",
        "debuginfo=2",
        "-C",
        "split-debuginfo=unpacked",
    ];
    assert_stored(&unit.serve(&flags).unwrap());
    std::thread::sleep(Duration::from_millis(1100));

    // A comment-only edit: rustc reuses most of the previous session.
    fs::write(
        unit.root.join("src/lib.rs"),
        "pub fn a() -> u32 { 1 }\n// edited\npub fn b() -> u32 { 2 }\n",
    )
    .unwrap();
    assert_stored(&unit.serve(&flags).unwrap());
    let built: Vec<String> = unit.outputs().into_iter().map(|(name, _)| name).collect();

    unit.clear_outputs();
    assert!(matches!(unit.serve(&flags).unwrap(), Served::Hit { .. }));
    let restored: Vec<String> = unit.outputs().into_iter().map(|(name, _)| name).collect();
    assert_eq!(restored, built);
}

#[test]
fn a_hit_removes_stale_outputs_of_the_same_unit() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    fs::write(
        unit.out().join("demo-1.stale.rcgu.o"),
        b"from another branch",
    )
    .unwrap();
    fs::write(unit.out().join("libdemo-1.rmeta"), b"from another branch").unwrap();
    assert!(matches!(unit.serve(&[]).unwrap(), Served::Hit { .. }));
    assert!(!unit.out().join("demo-1.stale.rcgu.o").exists());
    assert_ne!(
        fs::read(unit.out().join("libdemo-1.rmeta")).unwrap(),
        b"from another branch"
    );
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Sets the mtime of every file under `dir` to `when`.
fn backdate(dir: &Path, when: SystemTime) {
    for path in files_under(dir) {
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

#[test]
fn a_hit_records_its_last_use_on_the_action_and_pre_key() {
    let unit = Crate::new("pub fn answer() -> u32 { 42 }\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let long_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    let records = [unit.store.join("entries"), unit.store.join("prekeys")];
    for dir in &records {
        backdate(dir, long_ago);
    }

    unit.clear_outputs();
    assert!(matches!(unit.serve(&[]).unwrap(), Served::Hit { .. }));
    for dir in &records {
        for path in files_under(dir) {
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            assert!(
                modified > long_ago + Duration::from_secs(3600),
                "{} still says it was last used long ago",
                path.display()
            );
        }
    }
}

#[test]
fn a_miss_leaves_the_last_use_of_other_actions_alone() {
    let unit = Crate::new("pub fn answer() -> u32 { 42 }\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let long_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
    backdate(&unit.store.join("entries"), long_ago);
    let before = files_under(&unit.store.join("entries"));

    fs::write(
        unit.root.join("src/lib.rs"),
        "pub fn answer() -> u32 { 7 }\n",
    )
    .unwrap();
    assert_stored(&unit.serve(&[]).unwrap());
    for path in before {
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), long_ago);
    }
}

/// A remote tier backed by another machine's store directory.
struct StoreRemote {
    store: PathBuf,
    /// Leave this object out when fetching, as a remote with a hole would.
    withhold: Option<Digest>,
    /// Fail every call with this message.
    fail: Option<&'static str>,
}

impl StoreRemote {
    fn new(store: &Path) -> Self {
        Self {
            store: store.to_path_buf(),
            withhold: None,
            fail: None,
        }
    }
}

impl Remote for StoreRemote {
    fn input_sets(&self, pre: &Digest) -> std::result::Result<Option<String>, RemoteError> {
        if let Some(message) = self.fail {
            return Err(message.into());
        }
        Ok(record::read(
            &self.store.join("prekeys").join(pre.to_hex()),
        )?)
    }

    fn fetch_action(
        &self,
        key: &Digest,
        store: &Store,
    ) -> std::result::Result<Option<String>, RemoteError> {
        let path = self.store.join("entries/demo").join(key.to_hex());
        let Some(text) = record::read(&path)? else {
            return Ok(None);
        };
        let source = Store::open(&self.store)?;
        for digest in Action::decode(&text, &path)?.references() {
            if Some(digest) != self.withhold {
                store.insert_bytes(&source.read(&digest)?)?;
            }
        }
        Ok(Some(text))
    }
}

/// A unit compiled once, whose store is then moved aside to serve as the
/// remote of the now empty local store, as on a fresh machine.
fn remote_pair() -> (Crate, PathBuf) {
    let unit = Crate::new("pub fn answer() -> u32 { 42 }\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let remote = unit.root.join("remote-store");
    fs::rename(&unit.store, &remote).unwrap();
    unit.clear_outputs();
    (unit, remote)
}

#[test]
fn a_local_miss_is_served_from_the_remote_and_then_hits_locally() {
    let (second, remote) = remote_pair();
    let remote = StoreRemote::new(&remote);

    assert!(matches!(
        second.serve_with(Some(&remote), &[]).unwrap(),
        Served::RemoteHit { .. }
    ));
    let names: Vec<_> = second.outputs().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["demo-1.d", "libdemo-1.rlib", "libdemo-1.rmeta"]);

    second.clear_outputs();
    assert!(matches!(second.serve(&[]).unwrap(), Served::Hit { .. }));
}

#[test]
fn a_remote_action_missing_an_object_is_a_miss_with_the_reason() {
    let (second, remote) = remote_pair();
    let entry = files_under(&remote.join("entries")).pop().unwrap();
    let text = fs::read_to_string(&entry).unwrap();
    let action = Action::decode(&text, &entry).unwrap();
    let remote = StoreRemote {
        withhold: Some(action.outputs[1].digest),
        ..StoreRemote::new(&remote)
    };

    match second.serve_with(Some(&remote), &[]).unwrap() {
        Served::Miss {
            code: 0,
            stored: Ok(_),
            remote: Some(reason),
            ..
        } => assert!(reason.contains("does not provide object"), "{reason}"),
        other => panic!("expected a miss, got {other:?}"),
    }
}

#[test]
fn a_remote_action_naming_a_file_outside_the_output_directory_is_not_restored() {
    let (second, remote) = remote_pair();
    let entry = files_under(&remote.join("entries")).pop().unwrap();
    let text = fs::read_to_string(&entry).unwrap();
    fs::write(
        &entry,
        text.replace(" libdemo-1.rlib", " ../../escape.rlib"),
    )
    .unwrap();
    let remote = StoreRemote::new(&remote);

    assert!(matches!(
        second.serve_with(Some(&remote), &[]).unwrap(),
        Served::Miss { code: 0, .. }
    ));
    assert!(!second.root.join("target/escape.rlib").exists());
}

#[test]
fn a_failing_remote_only_makes_the_unit_compile() {
    let (second, remote) = remote_pair();
    let remote = StoreRemote {
        fail: Some("connection refused"),
        ..StoreRemote::new(&remote)
    };
    match second.serve_with(Some(&remote), &[]).unwrap() {
        Served::Miss {
            code: 0,
            stored: Ok(_),
            remote: Some(reason),
            ..
        } => assert_eq!(reason, "connection refused"),
        other => panic!("expected a miss, got {other:?}"),
    }
}

#[test]
fn a_remote_without_the_unit_is_a_plain_miss() {
    let empty = tempfile::tempdir().unwrap();
    let unit = Crate::new("pub fn answer() -> u32 { 42 }\n");
    let remote = StoreRemote::new(empty.path());
    match unit.serve_with(Some(&remote), &[]).unwrap() {
        Served::Miss {
            code: 0,
            stored: Ok(_),
            remote: None,
            ..
        } => {}
        other => panic!("expected a plain miss, got {other:?}"),
    }
}

#[test]
fn a_remote_input_set_whose_files_differ_here_is_not_used() {
    let (second, remote) = remote_pair();
    fs::write(
        second.root.join("src/lib.rs"),
        "pub fn answer() -> u32 { 7 }\n",
    )
    .unwrap();
    let remote = StoreRemote::new(&remote);
    assert!(matches!(
        second.serve_with(Some(&remote), &[]).unwrap(),
        Served::Miss { remote: None, .. }
    ));
}

/// The one action stored for `unit`: its path and text.
fn only_action(unit: &Crate) -> (PathBuf, String) {
    let dir = unit.store.join("entries/demo");
    let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let path = entries.into_iter().next().unwrap().unwrap().path();
    let text = fs::read_to_string(&path).unwrap();
    (path, text)
}

/// Another build's lock on `key`, as the wrapper takes it.
fn hold_lock(unit: &Crate, key: &str) -> fs::File {
    let dir = unit.store.join("locks");
    fs::create_dir_all(&dir).unwrap();
    let file = fs::File::create(dir.join(key)).unwrap();
    file.lock().unwrap();
    file
}

#[test]
fn a_compile_another_build_is_running_is_waited_for_then_restored() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    // Another build is compiling the same unit: its action is not written
    // yet, and it holds the lock on the full key.
    let (action, text) = only_action(&unit);
    fs::remove_file(&action).unwrap();
    unit.clear_outputs();
    let key = action.file_name().unwrap().to_str().unwrap().to_owned();
    let lock = hold_lock(&unit, &key);

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| unit.serve(&[]).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        fs::write(&action, &text).unwrap();
        drop(lock);
        match waiter.join().unwrap() {
            Served::Hit { waited: Some(_) } => {}
            other => panic!("expected a hit after waiting, got {other:?}"),
        }
    });
    assert_eq!(unit.outputs().len(), 3);
}

#[test]
fn a_compile_whose_twin_stored_nothing_runs_after_the_wait() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let (action, _) = only_action(&unit);
    fs::remove_file(&action).unwrap();
    let key = action.file_name().unwrap().to_str().unwrap().to_owned();
    let lock = hold_lock(&unit, &key);

    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| unit.serve(&[]).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        drop(lock);
        match waiter.join().unwrap() {
            Served::Miss {
                code: 0,
                stored: Ok(_),
                waited: Some(_),
                ..
            } => {}
            other => panic!("expected a stored miss after waiting, got {other:?}"),
        }
    });
}

#[test]
fn a_compile_with_other_sources_does_not_wait() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let (action, _) = only_action(&unit);
    let key = action.file_name().unwrap().to_str().unwrap().to_owned();
    let _lock = hold_lock(&unit, &key);
    fs::write(unit.root.join("src/lib.rs"), "pub fn g() {}\n").unwrap();

    match unit.serve(&[]).unwrap() {
        Served::Miss {
            code: 0, waited, ..
        } => assert_eq!(waited, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_held_lock_is_waited_for_only_until_the_limit() {
    let unit = Crate::new("pub fn f() {}\n");
    assert_stored(&unit.serve(&[]).unwrap());
    let (action, _) = only_action(&unit);
    fs::remove_file(&action).unwrap();
    let key = action.file_name().unwrap().to_str().unwrap().to_owned();
    let _lock = hold_lock(&unit, &key);

    match unit.serve_waiting(Duration::from_millis(50), &[]).unwrap() {
        Served::Miss {
            code: 0,
            stored: Ok(_),
            waited: None,
            ..
        } => {}
        other => panic!("expected a miss that gave up waiting, got {other:?}"),
    }
}
