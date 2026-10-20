//! Unit tests for pushing to and fetching from a remote, against the
//! in-process fake server and hand-built local stores.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use tinymold_wrap::Remote as _;

use super::*;
use crate::fake::{STALL, Server};
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
    assert_eq!((again.actions, again.refreshed), (0, 1));
    assert_eq!(server.with(|state| state.writes), 4);
}

#[test]
fn saving_a_remote_hit_refreshes_it_without_reading_local_outputs() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    server.with(|state| state.updates.clear());
    // A remote hit already has complete outputs on the server. Saving it
    // must not depend on reading those potentially multi-gigabyte files.
    local.store.remove(&local.objects[1]).unwrap();
    let report = cache.push(&local.root, None).unwrap();
    assert_eq!(report.skipped, 0);
    assert_eq!(report.blobs, 0);
    assert_eq!(
        server.with(|state| state.updates.clone()),
        vec![naming::prekey_key(&local.pre).hash]
    );
}

#[test]
fn saving_repairs_an_existing_action_whose_remote_blob_was_evicted() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    let object = naming::blob(b"rlib bytes").hash;
    server.with(|state| {
        state.blobs.remove(&object);
    });
    let report = cache.push(&local.root, None).unwrap();
    assert_eq!(report.blobs, 1);
    assert_eq!(report.actions, 1);
    assert!(server.with(|state| state.blobs.contains_key(&object)));
}

impl Local {
    /// Adds `count` more actions, each with an object of its own and a
    /// pre-key record pointing at nothing in particular.
    fn with_actions(self, count: usize) -> Self {
        for n in 0..count {
            let object = self
                .store
                .insert_bytes(format!("object {n}").as_bytes())
                .unwrap();
            let empty = self.store.insert_bytes(b"").unwrap();
            let key = Digest::of_bytes(format!("key {n}").as_bytes());
            write(
                &self.root.join("entries/demo").join(key.to_hex()),
                &format!(
                    "tinymold-action 1\nstdout {empty}\nstderr {empty}\noutput {object} 0 0 lib{n}.rlib\n"
                ),
            );
            let pre = Digest::of_bytes(format!("pre {n}").as_bytes());
            write(
                &self.root.join("prekeys").join(pre.to_hex()),
                &format!("tinymold-prekey 1\nset {n}\n"),
            );
        }
        self
    }
}

#[test]
fn push_overlaps_its_calls_over_a_slow_link() {
    // Every call waits 50 ms, as across an ocean: one at a time, 40 actions
    // would cost seconds of round trips.
    let server = Server::start();
    server.with(|state| state.delay = Some(Duration::from_millis(50)));
    let local = Local::new().with_actions(40);
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!((report.actions, report.prekeys, report.failed), (41, 41, 0));
    let peak = |method: &str| server.with(|state| state.peak_in_flight[method]);
    assert!(peak("Write") > 1, "uploads ran one at a time");
    assert!(
        peak("UpdateActionResult") > 1,
        "entries went up one at a time"
    );
}

#[test]
fn a_fetch_reads_an_actions_objects_together() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    server.with(|state| state.delay = Some(Duration::from_millis(50)));
    let elsewhere = tempfile::tempdir().unwrap();
    let fresh = Store::open(elsewhere.path()).unwrap();
    let text = cache.fetch_action(&local.key, &fresh).unwrap().unwrap();
    assert_eq!(text, local.action_text());
    for object in &local.objects {
        assert!(fresh.contains(object));
    }
    assert!(
        server.with(|state| state.peak_in_flight["Read"]) > 1,
        "objects were read one at a time"
    );
}

/// Runs `sizes` through [`in_flight`] as jobs that each take 10 ms, and
/// returns the most bytes and the most jobs that were running at once.
fn peaks(sizes: &[u64]) -> (u64, usize) {
    peaks_with_budget(sizes, IN_FLIGHT_BYTES)
}

fn peaks_with_budget(sizes: &[u64], budget: u64) -> (u64, usize) {
    use std::cell::Cell;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (bytes, jobs) = (Cell::new(0), Cell::new(0));
    let (peak_bytes, peak_jobs) = (Cell::new(0), Cell::new(0));
    let items = sizes.iter().map(|&size| (size, size)).collect();
    runtime
        .block_on(in_flight_with_budget(
            items,
            budget,
            |size| {
                let (bytes, jobs) = (&bytes, &jobs);
                let (peak_bytes, peak_jobs) = (&peak_bytes, &peak_jobs);
                async move {
                    bytes.set(bytes.get() + size);
                    jobs.set(jobs.get() + 1);
                    peak_bytes.set(peak_bytes.get().max(bytes.get()));
                    peak_jobs.set(peak_jobs.get().max(jobs.get()));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    bytes.set(bytes.get() - size);
                    jobs.set(jobs.get() - 1);
                }
            },
            |()| Ok(()),
        ))
        .unwrap();
    (peak_bytes.get(), peak_jobs.get())
}

#[test]
fn in_flight_keeps_small_jobs_together_up_to_the_call_limit() {
    assert_eq!(peaks(&[1; 100]), (IN_FLIGHT_CALLS as u64, IN_FLIGHT_CALLS));
}

#[test]
fn in_flight_keeps_the_bytes_in_flight_within_the_budget() {
    let half = IN_FLIGHT_BYTES / 2 + 1;
    let (bytes, jobs) = peaks(&[half, half, half, 1, 1, half]);
    assert!(bytes <= IN_FLIGHT_BYTES, "{bytes} bytes in flight");
    assert_eq!(jobs, 3, "a big job, and the small ones beside it");
}

#[test]
fn in_flight_runs_a_job_over_the_budget_alone() {
    assert_eq!(peaks(&[IN_FLIGHT_BYTES * 3, 1]), (IN_FLIGHT_BYTES * 3, 1));
}

#[test]
fn in_flight_stops_at_the_first_failure() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut seen = 0;
    let result = runtime.block_on(in_flight(
        vec![(1, 1), (1, 2), (1, 3)],
        |n| async move { n },
        |n| {
            seen += 1;
            if n == 1 {
                Err(Error::Timeout("Write"))
            } else {
                Ok(())
            }
        },
    ));
    assert!(matches!(result, Err(Error::Timeout("Write"))));
    assert_eq!(seen, 1);
}

#[test]
fn a_blob_over_the_budget_still_goes_up_alone() {
    let server = Server::start();
    let local = Local::new();
    let object = local
        .store
        .insert_bytes(&vec![7; usize::try_from(PUSH_IN_FLIGHT_BYTES * 2).unwrap()])
        .unwrap();
    let empty = local.store.insert_bytes(b"").unwrap();
    let key = Digest::of_bytes(b"huge");
    write(
        &local.root.join("entries/demo").join(key.to_hex()),
        &format!("tinymold-action 1\nstdout {empty}\nstderr {empty}\noutput {object} 0 0 huge\n"),
    );
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!((report.actions, report.failed), (2, 0));
}

#[test]
fn a_fetch_keeps_its_record_within_the_call_limit() {
    // One action of 40 outputs: its record is read under the same limit as
    // the objects, not as one call beyond it.
    let server = Server::start();
    let local = Local::new();
    let empty = local.store.insert_bytes(b"").unwrap();
    let mut record = format!("tinymold-action 1\nstdout {empty}\nstderr {empty}\n");
    for n in 0..40 {
        let object = local
            .store
            .insert_bytes(format!("part {n}").as_bytes())
            .unwrap();
        writeln!(record, "output {object} 0 0 part{n}").unwrap();
    }
    let key = Digest::of_bytes(b"many parts");
    write(&local.root.join("entries/demo").join(key.to_hex()), &record);
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    server.with(|state| state.delay = Some(Duration::from_millis(50)));
    let elsewhere = tempfile::tempdir().unwrap();
    let fresh = Store::open(elsewhere.path()).unwrap();
    assert_eq!(
        cache.fetch_action(&key, &fresh).unwrap().as_deref(),
        Some(record.as_str())
    );
    let peak = server.with(|state| state.peak_in_flight["Read"]);
    assert_eq!(peak, IN_FLIGHT_CALLS as u64);
}

#[test]
fn every_action_goes_up_before_any_pre_key() {
    // A reader finds actions through pre-keys, so a pre-key must never land
    // before the actions it leads to.
    let server = Server::start();
    let local = Local::new().with_actions(20);
    remote(&server).push(&local.root, None).unwrap();
    let prekeys: HashSet<String> = (0..20)
        .map(|n| naming::prekey_key(&Digest::of_bytes(format!("pre {n}").as_bytes())).hash)
        .chain([naming::prekey_key(&local.pre).hash])
        .collect();
    let updates = server.with(|state| state.updates.clone());
    let first_prekey = updates
        .iter()
        .position(|key| prekeys.contains(key))
        .unwrap();
    assert!(
        updates[first_prekey..]
            .iter()
            .all(|key| prekeys.contains(key))
    );
    assert_eq!(updates.len(), 42);
}

#[test]
fn push_since_skips_records_not_used_since() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    let later = SystemTime::now() + Duration::from_secs(3600);
    let report = cache.push(&local.root, Some(later)).unwrap();
    assert_eq!(
        (
            report.actions,
            report.prekeys,
            report.refreshed,
            report.blobs
        ),
        (0, 0, 0, 0)
    );
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
    let elsewhere = tempfile::tempdir().unwrap();
    let fetched = Store::open(elsewhere.path()).unwrap();
    assert!(
        remote(&server)
            .fetch_action(&local.key, &fetched)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_connection_the_server_closed_is_retried() {
    // nginx ends an HTTP/2 connection after keepalive_requests calls with a
    // GOAWAY; tonic reports the calls in flight as internal h2 errors. The
    // channel reconnects, so a retry goes through.
    let status = |message: &str| Error::Status {
        call: "Write",
        status: Box::new(tonic::Status::internal(message.to_owned())),
    };
    assert!(transient(&status("h2 protocol error: http2 error")));
    assert!(!transient(&status("the store is broken")));
}

#[test]
fn a_proxy_whose_server_is_restarting_is_retried() {
    // nginx answers 502 while `tinymold serve` restarts (a deploy), with an
    // HTML body tonic cannot parse as gRPC; 503 and 504 are the same story.
    let status = |message: &str| Error::Status {
        call: "Write",
        status: Box::new(tonic::Status::internal(message.to_owned())),
    };
    for code in [
        "502 Bad Gateway",
        "503 Service Unavailable",
        "504 Gateway Timeout",
    ] {
        let message = format!(
            "protocol error: received message with invalid compression flag: 60 (valid flags are 0 and 1) while receiving response with status: {code}"
        );
        assert!(transient(&status(&message)), "{code}");
    }
    assert!(!transient(&status(
        "protocol error: received message with invalid compression flag: 60 (valid flags are 0 and 1) while receiving response with status: 400 Bad Request"
    )));
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
        &format!(
            "tinymold-action 1\nstdout {empty}\nstderr {empty}\noutput {stuck} 0 0 libstuck.rlib\n",
            empty = local.objects[0]
        ),
    );
    server.with(|state| {
        state.stalled_blobs.insert(naming::blob(b"stuck rlib").hash);
    });
    let report = impatient(&server).push(&local.root, None).unwrap();
    assert_eq!(
        (
            report.actions,
            report.prekeys,
            report.failed,
            report.skipped
        ),
        (1, 1, 1, 0)
    );
    assert!(
        report.to_string().contains("1 entries not pushed"),
        "{report}"
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let fetched = Store::open(elsewhere.path()).unwrap();
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
fn a_push_that_left_no_pre_key_is_an_error() {
    let server = Server::start();
    // The pre-key record's blob never goes up, so no reader could find the
    // action that did.
    server.with(|state| {
        state
            .stalled_blobs
            .insert(naming::blob(b"tinymold-prekey 1\nset 1\n").hash);
    });
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
fn a_signed_pre_key_reads_its_signature_and_record_together() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();
    server.with(|state| state.delay = Some(Duration::from_millis(50)));
    let reader = configured(&server, trusting(&signer));
    assert!(reader.input_sets(&local.pre).unwrap().is_some());
    assert_eq!(server.with(|state| state.peak_in_flight["Read"]), 2);
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
fn an_unsigned_pre_key_is_refused_without_waiting_for_its_record() {
    // The record never arrives: a reader that waited for it would hang for
    // the stall, or buffer whatever an untrusted server chose to send.
    let server = Server::start();
    let local = Local::new();
    remote(&server).push(&local.root, None).unwrap();
    let record = naming::blob(b"tinymold-prekey 1\nset 1\n");
    server.with(|state| state.stalled_reads.insert(record.hash));
    let reader = configured(&server, trusting(&SigningKey::generate()));
    let started = std::time::Instant::now();
    let result = reader.input_sets_of(&local.pre);
    assert!(matches!(result, Err(Error::Untrusted { .. })), "{result:?}");
    assert!(
        started.elapsed() < STALL / 2,
        "waited {:?}",
        started.elapsed()
    );
}

#[test]
fn each_object_is_stored_as_soon_as_it_arrives() {
    // One object stalls past the timeout; the one that did arrive is already
    // in the store rather than held in memory until every read is done.
    let server = Server::start();
    let local = Local::new();
    remote(&server).push(&local.root, None).unwrap();
    let rlib = naming::blob(b"rlib bytes");
    server.with(|state| state.stalled_reads.insert(rlib.hash));
    let elsewhere = tempfile::tempdir().unwrap();
    let fresh = Store::open(elsewhere.path()).unwrap();
    let result = impatient(&server).fetch_action_into(&local.key, &fresh);
    assert!(matches!(result, Err(Error::Timeout("Read"))), "{result:?}");
    let empty = tinymold_hash::Digest::of_bytes(b"");
    assert!(fresh.contains(&empty));
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
        (ACTION_FILE.to_owned(), record_for(b"x")),
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
        (ACTION_FILE.to_owned(), record_for(b"other")),
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
        vec![(ACTION_FILE.to_owned(), record_for(b"x"))],
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

#[test]
fn the_push_budget_overlaps_artifacts_larger_than_the_fetch_budget() {
    let size = IN_FLIGHT_BYTES * 2;
    assert_eq!(
        peaks_with_budget(&[size; 3], PUSH_IN_FLIGHT_BYTES),
        (size * 3, 3)
    );
}

#[test]
fn saving_republishes_an_action_after_the_signing_key_changes() {
    let server = Server::start();
    let local = Local::new();
    let first = SigningKey::generate();
    configured(&server, signing(&first))
        .push(&local.root, None)
        .unwrap();
    let second = SigningKey::generate();
    let report = configured(&server, signing(&second))
        .push(&local.root, None)
        .unwrap();
    assert_eq!((report.actions, report.refreshed), (1, 0));
    let fresh = tempfile::tempdir().unwrap();
    let reader = configured(&server, |config| {
        config.trusted_keys = Some(TrustedKeys::from_list(&second.public_hex()).unwrap());
    });
    assert!(
        reader
            .fetch_action(&local.key, &Store::open(fresh.path()).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn saving_repairs_a_remote_action_with_the_wrong_record() {
    let server = Server::start();
    let local = Local::new();
    let cache = remote(&server);
    cache.push(&local.root, None).unwrap();
    server.with(|state| {
        state
            .actions
            .get_mut(&naming::action_key(&local.key).hash)
            .unwrap()
            .output_files
            .retain(|file| file.path != ACTION_FILE);
    });
    let report = cache.push(&local.root, None).unwrap();
    assert_eq!((report.actions, report.refreshed), (1, 0));
}

#[test]
fn a_transient_refresh_failure_falls_back_to_uploading() {
    let server = Server::start();
    let local = Local::new();
    // Exhaust the optional refresh's retries, while writes remain healthy.
    server.with(|state| state.unavailable_gets = 4);
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!(
        (report.actions, report.prekeys, report.refreshed),
        (1, 1, 0)
    );
    assert_eq!(server.with(|state| state.unavailable_gets), 0);
}

#[test]
fn an_evicted_signature_during_refresh_does_not_abort_the_push() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();
    server.with(|state| {
        let result = &state.actions[&naming::action_key(&local.key).hash];
        let signature = result
            .output_files
            .iter()
            .find(|file| file.path == SIGNATURE_FILE)
            .unwrap()
            .digest
            .as_ref()
            .unwrap()
            .hash
            .clone();
        state.blobs.remove(&signature);
    });
    let report = configured(&server, trusting(&signer))
        .push(&local.root, None)
        .unwrap();
    assert_eq!(
        (report.actions, report.prekeys, report.refreshed),
        (1, 1, 0)
    );
}

#[test]
fn a_cache_outage_stops_queued_refreshes_instead_of_retrying_every_action() {
    let server = Server::start();
    let local = Local::new().with_actions(100);
    server.with(|state| state.unavailable_gets = u64::MAX);
    let report = remote(&server).push(&local.root, None).unwrap();
    assert_eq!(
        (report.actions, report.prekeys, report.refreshed),
        (101, 101, 0)
    );
    let attempts = server.with(|state| state.gets);
    assert!(
        attempts <= (IN_FLIGHT_CALLS * (RETRY_DELAYS.len() + 1)) as u64,
        "queued actions repeated a cache-wide outage: {attempts} calls"
    );
}

#[test]
fn an_oversized_signature_is_refused_before_downloading_it() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();
    server.with(|state| {
        let result = state
            .actions
            .get_mut(&naming::action_key(&local.key).hash)
            .unwrap();
        let signature = result
            .output_files
            .iter_mut()
            .find(|file| file.path == SIGNATURE_FILE)
            .unwrap();
        signature.digest.as_mut().unwrap().size_bytes = 1025;
    });
    let reads = server.with(|state| state.peak_in_flight.get("Read").copied());
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let result = configured(&server, trusting(&signer)).fetch_action_into(&local.key, &store);
    assert!(
        matches!(result, Err(Error::BlobSize { size: 1025, .. })),
        "{result:?}"
    );
    assert_eq!(
        server.with(|state| state.peak_in_flight.get("Read").copied()),
        reads
    );
}

#[test]
fn oversized_remote_records_are_refused_before_downloading_them() {
    let server = Server::start();
    let local = Local::new();
    remote(&server).push(&local.root, None).unwrap();
    server.with(|state| {
        for result in state.actions.values_mut() {
            for file in &mut result.output_files {
                if file.path == ACTION_FILE || file.path == PREKEY_FILE {
                    file.digest.as_mut().unwrap().size_bytes = (16 << 20) + 1;
                }
            }
        }
    });
    let reads = server.with(|state| state.peak_in_flight.get("Read").copied());
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let reader = remote(&server);
    assert!(matches!(
        reader.fetch_action_into(&local.key, &store),
        Err(Error::BlobSize { .. })
    ));
    assert!(matches!(
        reader.input_sets_of(&local.pre),
        Err(Error::BlobSize { .. })
    ));
    assert_eq!(
        server.with(|state| state.peak_in_flight.get("Read").copied()),
        reads
    );
}

#[test]
fn a_signed_record_does_not_authorize_an_unrelated_extra_blob() {
    let server = Server::start();
    let local = Local::new();
    let signer = SigningKey::generate();
    configured(&server, signing(&signer))
        .push(&local.root, None)
        .unwrap();
    let bytes = b"attacker controlled extra object";
    let extra = Digest::of_bytes(bytes);
    server.with(|state| {
        let digest = naming::blob(bytes);
        state.blobs.insert(digest.hash.clone(), bytes.to_vec());
        state
            .actions
            .get_mut(&naming::action_key(&local.key).hash)
            .unwrap()
            .output_files
            .push(output(&extra.to_hex(), digest));
    });
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    let result = configured(&server, trusting(&signer)).fetch_action_into(&local.key, &store);
    assert!(matches!(result, Err(Error::Malformed { .. })), "{result:?}");
    assert!(!store.contains(&extra));
    for object in &local.objects {
        assert!(
            !store.contains(object),
            "refuse the entry before fetching artifacts"
        );
    }
}

/// A valid action that references just the supplied diagnostic bytes.
fn record_for(bytes: &[u8]) -> Vec<u8> {
    let digest = Digest::of_bytes(bytes);
    format!("tinymold-action 1\nstdout {digest}\nstderr {digest}\n").into_bytes()
}

#[test]
fn metadata_size_limits_include_the_boundary_and_reject_negative_sizes() {
    for limit in [MAX_RECORD, MAX_SIGNATURE] {
        let mut digest = naming::blob(b"");
        digest.size_bytes = limit;
        assert!(metadata_size(&digest, limit).is_ok());
        for size in [-1, limit + 1] {
            digest.size_bytes = size;
            assert!(matches!(
                metadata_size(&digest, limit),
                Err(Error::BlobSize { .. })
            ));
        }
    }
}

#[test]
fn missing_and_duplicate_action_objects_are_refused_even_when_cached_locally() {
    for duplicate in [false, true] {
        let server = Server::start();
        let local = Local::new();
        remote(&server).push(&local.root, None).unwrap();
        server.with(|state| {
            let result = state
                .actions
                .get_mut(&naming::action_key(&local.key).hash)
                .unwrap();
            if duplicate {
                let file = result
                    .output_files
                    .iter()
                    .find(|file| file.path != ACTION_FILE)
                    .unwrap()
                    .clone();
                result.output_files.push(file);
            } else {
                result
                    .output_files
                    .retain(|file| file.path != local.objects[1].to_hex());
            }
        });
        let result = remote(&server).fetch_action_into(&local.key, &local.store);
        assert!(
            matches!(
                result,
                Err(Error::Malformed {
                    reason: "outputs do not match the action record",
                    ..
                })
            ),
            "{result:?}"
        );
    }
}

#[test]
fn a_malformed_record_is_refused_before_any_artifact_is_stored() {
    let server = Server::start();
    let key = Digest::of_bytes(b"malformed");
    let object = Digest::of_bytes(b"artifact");
    plant(
        &server,
        &key,
        vec![
            (ACTION_FILE.to_owned(), b"not an action".to_vec()),
            (object.to_hex(), b"artifact".to_vec()),
        ],
    );
    let empty = tempfile::tempdir().unwrap();
    let store = Store::open(empty.path()).unwrap();
    assert!(matches!(
        remote(&server).fetch_action_into(&key, &store),
        Err(Error::Wrap(_))
    ));
    assert!(!store.contains(&object));
}
