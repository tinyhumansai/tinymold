//! Unit tests for pushing to and fetching from a remote, against the
//! in-process fake server and hand-built local stores.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use tinymold_wrap::Remote as _;

use super::*;
use crate::fake::Server;
use crate::{SigningKey, TrustedKeys};

/// A local store holding one action of two objects and its pre-key record.
struct Local {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    key: Digest,
    pre: Digest,
    objects: Vec<Digest>,
}

impl Local {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let store = Store::open(&root).unwrap();
        let stdout = store.insert_bytes(b"").unwrap();
        let rlib = store.insert_bytes(b"rlib bytes").unwrap();
        let key = Digest::of_bytes(b"full key");
        let pre = Digest::of_bytes(b"pre key");
        write(
            &root.join("entries/demo").join(key.to_hex()),
            &format!(
                "tinymold-action 1\nstdout {stdout}\nstderr {stdout}\noutput {rlib} 0 0 libdemo.rlib\n"
            ),
        );
        write(
            &root.join("prekeys").join(pre.to_hex()),
            "tinymold-prekey 1\nset 1\n",
        );
        Self {
            _dir: dir,
            root,
            store,
            key,
            pre,
            objects: vec![stdout, rlib],
        }
    }

    fn action_text(&self) -> String {
        fs::read_to_string(self.root.join("entries/demo").join(self.key.to_hex())).unwrap()
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn remote(server: &Server) -> RemoteCache {
    RemoteCache::new(&RemoteConfig::new(&server.url()).unwrap()).unwrap()
}

#[test]
fn a_pushed_action_is_fetched_into_an_empty_store() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);

    let report = cache.push(&local.root, None).unwrap();
    assert_eq!(report.actions, 1);
    assert_eq!(report.prekeys, 1);
    assert_eq!(report.skipped, 0);
    // The action record, the pre-key record, and two objects.
    assert_eq!(report.blobs, 4);
    assert!(
        report
            .to_string()
            .starts_with("pushed 1 actions and 1 pre-keys")
    );

    let elsewhere = tempfile::tempdir().unwrap();
    let fresh = Store::open(elsewhere.path()).unwrap();
    assert_eq!(
        cache.input_sets(&local.pre).unwrap().as_deref(),
        Some("tinymold-prekey 1\nset 1\n")
    );
    let text = cache.fetch_action(&local.key, &fresh).unwrap().unwrap();
    assert_eq!(text, local.action_text());
    for object in &local.objects {
        assert!(fresh.contains(object));
    }
}

#[test]
fn pushing_again_uploads_no_blob_twice() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    let again = cache.push(&local.root, None).unwrap();
    assert_eq!(again.blobs, 0);
    assert_eq!(again.actions, 1);
    assert_eq!(server.with(|state| state.writes), 4);
}

#[test]
fn push_since_skips_records_not_used_since() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    let later = SystemTime::now() + Duration::from_secs(3600);
    let report = cache.push(&local.root, Some(later)).unwrap();
    assert_eq!(report, PushReport::default());
}

#[test]
fn an_action_whose_object_is_gone_is_skipped() {
    let server = Server::start();
    let local = Local::new();
    local.store.remove(&local.objects[1]).unwrap();
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!(report.skipped, 1);
    assert_eq!(report.actions, 0);
}

/// A client that gives up on a call after 300 ms, well before a stalled
/// upload on the fake server answers.
fn impatient(server: &Server) -> RemoteCache {
    configured(server, |config| config.timeout = Duration::from_millis(300))
}

#[test]
fn an_upload_that_times_out_once_is_retried() {
    let server = Server::start();
    server.with(|state| state.stall_writes = 1);
    let local = Local::new();
    let report = impatient(&server).push(&local.root, None).unwrap();
    assert_eq!((report.actions, report.prekeys, report.failed), (1, 1, 0));
    let fetched = Store::open(local._dir.path().join("fetched")).unwrap();
    assert!(remote(&server).fetch_action(&local.key, &fetched).unwrap().is_some());
}

#[test]
fn an_upload_that_keeps_failing_skips_only_the_entries_that_need_it() {
    let server = Server::start();
    let local = Local::new();
    // A second action whose only object the server will never accept.
    let stuck = local.store.insert_bytes(b"stuck rlib").unwrap();
    let stuck_key = Digest::of_bytes(b"stuck key");
    write(
        &local.root.join("entries/stuck").join(stuck_key.to_hex()),
        &format!("tinymold-action 1\noutput {stuck} 0 0 libstuck.rlib\n"),
    );
    server.with(|state| {
        state
            .stalled_blobs
            .insert(naming::blob(b"stuck rlib").hash);
    });
    let report = impatient(&server).push(&local.root, None).unwrap();
    assert_eq!((report.actions, report.prekeys, report.failed), (1, 1, 1));
    assert!(report.to_string().contains("1 entries not pushed"), "{report}");
    let fetched = Store::open(local._dir.path().join("fetched")).unwrap();
    let cache = remote(&server);
    assert!(cache.fetch_action(&local.key, &fetched).unwrap().is_some());
    assert_eq!(cache.fetch_action(&stuck_key, &fetched).unwrap(), None);
}

#[test]
fn a_push_whose_every_upload_fails_is_an_error() {
    let server = Server::start();
    server.with(|state| state.stall_writes = u64::MAX);
    let local = Local::new();
    let result = impatient(&server).push(&local.root, None);
    assert!(matches!(result, Err(Error::Timeout("Write"))), "{result:?}");
}

#[test]
fn a_corrupt_action_record_is_skipped() {
    let server = Server::start();
    let local = Local::new();
    write(
        &local.root.join("entries/demo").join(local.key.to_hex()),
        "not an action",
    );
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!(report.skipped, 1);
}

#[test]
fn unknown_keys_are_none() {
    let server = Server::start();
    let cache = remote(&server);
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let key = Digest::of_bytes(b"nothing");
    assert_eq!(cache.input_sets(&key).unwrap(), None);
    assert_eq!(cache.fetch_action(&key, &store).unwrap(), None);
}

#[test]
fn a_read_only_server_fails_the_push() {
    let server = Server::start();
    server.with(|state| state.read_only = true);
    let local = Local::new();
    let result = remote(&server).push(&local.root, None);
    assert!(matches!(result, Err(Error::Status { .. })), "{result:?}");
}

fn configured(server: &Server, change: impl FnOnce(&mut RemoteConfig)) -> RemoteCache {
    let mut config = RemoteConfig::new(&server.url()).unwrap();
    change(&mut config);
    RemoteCache::new(&config).unwrap()
}

/// A server that needs `Basic Y2k6cHc=` (`ci:pw`) for every call, as
/// bazel-remote does with an htpasswd file.
fn guarded() -> Server {
    let server = Server::start();
    server.with(|state| state.required_auth = Some("Basic Y2k6cHc=".to_owned()));
    server
}

fn with_credential(config: &mut RemoteConfig) {
    config.authorization = Some("Basic Y2k6cHc=".to_owned());
}

#[test]
fn check_reports_capabilities() {
    let server = guarded();
    let capabilities = configured(&server, with_credential).check().unwrap();
    assert_eq!(
        capabilities,
        Capabilities {
            digest_functions: vec!["sha256"],
            updates_enabled: true,
            writable: true,
            anonymous_writes: false,
        }
    );
    assert!(capabilities.sha256());
    assert_eq!(
        capabilities.to_string(),
        "digests sha256, action cache updates yes, writable yes, anonymous writes no"
    );
}

#[test]
fn check_refuses_a_server_open_to_anonymous_writes() {
    let server = Server::start();
    let result = remote(&server).check();
    assert!(
        matches!(result, Err(Error::OpenToAnonymousWrites)),
        "{result:?}"
    );
    // A credential does not make the server any less open.
    server.with(|state| state.required_auth = None);
    let result = configured(&server, with_credential).check();
    assert!(
        matches!(result, Err(Error::OpenToAnonymousWrites)),
        "{result:?}"
    );
}

#[test]
fn check_accepts_an_open_server_when_entries_must_be_signed() {
    let server = Server::start();
    let signer = SigningKey::generate();
    let capabilities = configured(&server, |config| {
        config.trusted_keys = Some(TrustedKeys::from_list(&signer.public_hex()).unwrap());
    })
    .check()
    .unwrap();
    assert!(capabilities.anonymous_writes);
    assert!(capabilities.writable);
}

fn signing(signer: &SigningKey) -> impl FnOnce(&mut RemoteConfig) {
    let signer = signer.clone();
    move |config| config.signing_key = Some(signer)
}

fn trusting(signer: &SigningKey) -> impl FnOnce(&mut RemoteConfig) {
    let public = signer.public_hex();
    move |config| config.trusted_keys = Some(TrustedKeys::from_list(&public).unwrap())
}

#[test]
fn a_signed_entry_is_fetched_by_a_reader_that_trusts_its_key() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();

    let reader = configured(&server, trusting(&signer));
    assert!(reader.input_sets(&local.pre).unwrap().is_some());
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let text = reader.fetch_action(&local.key, &store).unwrap();
    assert_eq!(text.as_deref(), Some(local.action_text().as_str()));
}

#[test]
fn an_unsigned_entry_is_refused_before_any_object_is_fetched() {
    let server = Server::start();
    let local = Local::new();
    remote(&server).push(&local.root, None).unwrap();

    let reader = configured(&server, trusting(&SigningKey::generate()));
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let result = reader.fetch_action_into(&local.key, &store);
    assert!(
        matches!(
            result,
            Err(Error::Untrusted {
                reason: "it is not signed",
                ..
            })
        ),
        "{result:?}"
    );
    for object in &local.objects {
        assert!(!store.contains(object));
    }
    let result = reader.input_sets_of(&local.pre);
    assert!(
        matches!(
            result,
            Err(Error::Untrusted {
                reason: "it is not signed",
                ..
            })
        ),
        "{result:?}"
    );
}

#[test]
fn an_entry_signed_by_an_untrusted_key_is_refused() {
    let server = Server::start();
    let local = Local::new();
    configured(&server, signing(&SigningKey::generate()))
        .push(&local.root, None)
        .unwrap();

    let reader = configured(&server, trusting(&SigningKey::generate()));
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let result = reader.fetch_action_into(&local.key, &store);
    assert!(matches!(result, Err(Error::Untrusted { .. })), "{result:?}");
    assert!(!store.contains(&local.objects[1]));
}

#[test]
fn a_signed_entry_whose_record_is_swapped_is_refused() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();

    // An attacker with write access points the entry at a record of their own,
    // keeping the original signature.
    let forged = b"tinymold-action 1\nstdout 00\n".to_vec();
    let digest = naming::blob(&forged);
    server.with(|state| {
        state.blobs.insert(digest.hash.clone(), forged);
        let result = state
            .actions
            .get_mut(&naming::action_key(&local.key).hash)
            .unwrap();
        for file in &mut result.output_files {
            if file.path == ACTION_FILE {
                file.digest = Some(digest.clone());
            }
        }
    });

    let reader = configured(&server, trusting(&signer));
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let result = reader.fetch_action_into(&local.key, &store);
    assert!(
        matches!(
            result,
            Err(Error::Untrusted {
                reason: "its signature does not match it",
                ..
            })
        ),
        "{result:?}"
    );
}

#[test]
fn a_reader_without_trusted_keys_still_uses_signed_entries() {
    let server = Server::start();
    let local = Local::new();
    configured(&server, signing(&SigningKey::generate()))
        .push(&local.root, None)
        .unwrap();
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    assert!(
        remote(&server)
            .fetch_action(&local.key, &store)
            .unwrap()
            .is_some()
    );
}

#[test]
fn signed_pushes_are_idempotent() {
    let server = Server::start();
    let local = Local::new();
    let writer = configured(&server, signing(&SigningKey::generate()));
    writer.push(&local.root, None).unwrap();
    let again = writer.push(&local.root, None).unwrap();
    assert_eq!(again.blobs, 0);
}

#[test]
fn check_tells_a_refused_credential_from_an_anonymous_read_only_client() {
    let server = Server::start();
    server.with(|state| state.read_only = true);
    let anonymous = remote(&server).check().unwrap();
    assert!(!anonymous.writable);

    let mut config = RemoteConfig::new(&server.url()).unwrap();
    config.authorization = Some("Basic d3Jvbmc=".to_owned());
    let refused = RemoteCache::new(&config).unwrap().check();
    assert!(
        matches!(refused, Err(Error::Status { call: "Write", .. })),
        "{refused:?}"
    );
}

/// Puts `result` under the action key of `key` on `server`, with every
/// output's blob present.
fn plant(server: &Server, key: &Digest, files: Vec<(String, Vec<u8>)>) {
    server.with(|state| {
        let mut output_files = Vec::new();
        for (path, bytes) in files {
            let digest = naming::blob(&bytes);
            state.blobs.insert(digest.hash.clone(), bytes);
            output_files.push(output(&path, digest));
        }
        state.actions.insert(
            naming::action_key(key).hash,
            ActionResult {
                output_files,
                exit_code: 0,
            },
        );
    });
}

fn fetch_error(files: Vec<(String, Vec<u8>)>) -> Error {
    let server = Server::start();
    let key = Digest::of_bytes(b"planted");
    plant(&server, &key, files);
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    remote(&server)
        .fetch_action_into(&key, &store)
        .expect_err("a malformed entry must not be fetched")
}

#[test]
fn entries_tinymold_did_not_write_are_refused() {
    let no_record = fetch_error(vec![]);
    assert!(matches!(
        no_record,
        Error::Malformed {
            reason: "it has no tinymold record",
            ..
        }
    ));

    let unnamed = fetch_error(vec![
        (ACTION_FILE.to_owned(), b"record".to_vec()),
        ("libdemo.rlib".to_owned(), b"x".to_vec()),
    ]);
    assert!(matches!(
        unnamed,
        Error::Malformed {
            reason: "an output is not named by its digest",
            ..
        }
    ));

    let misnamed = fetch_error(vec![
        (ACTION_FILE.to_owned(), b"record".to_vec()),
        (Digest::of_bytes(b"other").to_hex(), b"x".to_vec()),
    ]);
    assert!(matches!(
        misnamed,
        Error::Malformed {
            reason: "an output does not match its name",
            ..
        }
    ));

    let binary = fetch_error(vec![(ACTION_FILE.to_owned(), vec![0xff, 0xfe])]);
    assert!(matches!(
        binary,
        Error::Malformed {
            reason: "its action record is not text",
            ..
        }
    ));
}

#[test]
fn an_output_without_a_digest_is_refused() {
    let server = Server::start();
    let key = Digest::of_bytes(b"planted");
    plant(
        &server,
        &key,
        vec![(ACTION_FILE.to_owned(), b"record".to_vec())],
    );
    server.with(|state| {
        let result = state
            .actions
            .get_mut(&naming::action_key(&key).hash)
            .unwrap();
        result.output_files.push(OutputFile {
            path: Digest::of_bytes(b"x").to_hex(),
            digest: None,
            is_executable: false,
        });
    });
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let error = remote(&server).fetch_action_into(&key, &store).unwrap_err();
    assert!(matches!(
        error,
        Error::Malformed {
            reason: "an output has no digest",
            ..
        }
    ));
}

#[test]
fn a_pre_key_record_that_is_not_text_is_refused() {
    let server = Server::start();
    let pre = Digest::of_bytes(b"pre");
    let bytes = vec![0xff];
    let digest = naming::blob(&bytes);
    server.with(|state| {
        state.blobs.insert(digest.hash.clone(), bytes);
        state.actions.insert(
            naming::prekey_key(&pre).hash,
            ActionResult {
                output_files: vec![output(PREKEY_FILE, digest)],
                exit_code: 0,
            },
        );
    });
    let error = remote(&server).input_sets_of(&pre).unwrap_err();
    assert!(matches!(
        error,
        Error::Malformed {
            reason: "its pre-key record is not text",
            ..
        }
    ));
}

#[test]
fn objects_the_store_already_holds_are_not_fetched_again() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    // Remove the blobs: a fetch that asked for them would fail.
    let record = naming::blob(local.action_text().as_bytes());
    server.with(|state| state.blobs.retain(|hash, _| *hash == record.hash));
    let text = cache.fetch_action(&local.key, &local.store).unwrap();
    assert_eq!(text.unwrap(), local.action_text());
}
