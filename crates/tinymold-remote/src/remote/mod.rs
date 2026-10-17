//! The remote tier as a synchronous cache: lookups for the wrapper, and
//! batch uploads for the end of a CI job.
//!
//! [`RemoteCache`] owns a current-thread tokio runtime and a lazily
//! connected [`Client`]. Nothing touches the network until the first call,
//! so a wrapper that only hits locally pays for neither a connection nor a
//! thread.
//!
//! An action is stored as an `ActionResult` (see `docs/specs/remote.md`):
//!
//! - one output file, [`naming::ACTION_FILE`], holds the tinymold action
//!   record;
//! - one output file per object the record names. Its path is the object's
//!   BLAKE3 digest, and its digest is the object's SHA-256.
//!
//! - with a signing key, one more output file, [`SIGNATURE_FILE`], holds the
//!   entry's signature (see `crate::signing`). A reader that trusts keys
//!   checks it before fetching anything else.
//!
//! Listing the objects as outputs makes servers that check an entry's
//! dependencies (bazel-remote does) vouch for every blob, and keep each one
//! as long as the entry lives. The fetching side re-derives every object's
//! BLAKE3 digest as it enters the local store, and refuses one whose content
//! does not match the name it was listed under.

mod refresh;
mod types;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime};

use futures_util::{StreamExt as _, future, stream};
use tinymold_cas::Store;
use tinymold_hash::Digest;
use tinymold_wrap::{RecordKind, RemoteError};

use crate::client::Client;
use crate::config::RemoteConfig;
use crate::naming::{self, ACTION_FILE, PREKEY_FILE};
use crate::proto::{self, ActionResult, OutputFile};
use crate::signing::{Kind, SIGNATURE_FILE};
use crate::{Error, Result, SigningKey, TrustedKeys};

pub use types::{Capabilities, PushReport};

/// How many digests one `FindMissingBlobs` call asks about.
const FIND_BATCH: usize = 1000;

/// How many calls `push` and a fetch keep in flight at once. Each is a round
/// trip; a CI runner an ocean away from its cache spends most of a call
/// waiting, and one at a time leaves the link nearly idle.
const IN_FLIGHT_CALLS: usize = 32;

/// How many blob bytes may be in flight at once. Each transfer holds its
/// blob in memory, and they share the link: this bounds the memory, and
/// every transfer's deadline allows for this many bytes beside it. A blob
/// larger than this goes alone.
const IN_FLIGHT_BYTES: u64 = if cfg!(test) { 64 << 10 } else { 32 << 20 };

/// Push runs once per job; its larger budget overlaps large Rust artifacts
/// without multiplying the memory budget of every concurrent wrapper.
const PUSH_IN_FLIGHT_BYTES: u64 = if cfg!(test) { 512 << 10 } else { 256 << 20 };

/// How long `push` waits before each retry of a call that timed out or
/// found the server unavailable. A server behind a saturated disk can stall
/// a write for seconds; giving up on the first stall throws away the whole
/// push.
const RETRY_DELAYS: [Duration; 3] = if cfg!(test) {
    [Duration::from_millis(10); 3]
} else {
    [
        Duration::from_secs(1),
        Duration::from_secs(4),
        Duration::from_secs(15),
    ]
};

/// A remote REAPI cache, called synchronously.
#[derive(Debug)]
pub struct RemoteCache {
    runtime: tokio::runtime::Runtime,
    client: Client,
    /// What the client was made from, for the anonymous probe in `check`.
    config: RemoteConfig,
}

/// Where the bytes of a blob to upload come from.
enum Source {
    /// A record's text.
    Bytes(Vec<u8>),
    /// An object in the local store.
    Object(Digest),
}

impl RemoteCache {
    /// A cache for the server `config` names. Does not connect yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runtime`] when the runtime cannot start,
    /// [`Error::Connect`] when the endpoint is unusable, and
    /// [`Error::InvalidAuth`] when the credential cannot be sent as a header.
    pub fn new(config: &RemoteConfig) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Error::Runtime)?;
        let client = {
            let _entered = runtime.enter();
            Client::new(config)?
        };
        Ok(Self {
            runtime,
            client,
            config: config.clone(),
        })
    }

    fn signing_key(&self) -> Option<&SigningKey> {
        self.config.signing_key.as_ref()
    }

    fn trusted_keys(&self) -> Option<&TrustedKeys> {
        self.config.trusted_keys.as_ref()
    }

    /// Asks the server what it supports, then uploads a small probe blob to
    /// learn whether `push` would be allowed, and another with no credential
    /// to learn whether anyone could write. This is `tinymold remote check`.
    ///
    /// A server that allows unauthenticated reads answers the first call
    /// whatever the credential, so only the probe tells a wrong credential
    /// apart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] or [`Error::Timeout`] when the server cannot
    /// be reached, and when a configured credential is refused for the probe.
    /// Without a credential, a refused probe only reports the cache read-only.
    /// Returns [`Error::OpenToAnonymousWrites`] when the server accepts a
    /// write with no credential and no trusted keys are configured, since
    /// anyone who reaches it could then plant entries this client would use.
    pub fn check(&self) -> Result<Capabilities> {
        let capabilities = self.runtime.block_on(self.client.capabilities())?;
        let cache = capabilities.cache_capabilities.unwrap_or_default();
        let probe = b"tinymold remote check\n";
        let authenticated = self.config.authorization.is_some();
        let writable = match self
            .runtime
            .block_on(self.client.write(&naming::blob(probe), probe))
        {
            Ok(()) => true,
            Err(error) if authenticated => return Err(error),
            Err(_) => false,
        };
        let anonymous_writes = if authenticated {
            let anonymous = RemoteConfig {
                authorization: None,
                ..self.config.clone()
            };
            let client = {
                let _entered = self.runtime.enter();
                Client::new(&anonymous)?
            };
            self.runtime
                .block_on(client.write(&naming::blob(probe), probe))
                .is_ok()
        } else {
            writable
        };
        if anonymous_writes && self.trusted_keys().is_none() {
            return Err(Error::OpenToAnonymousWrites);
        }
        let digest_functions = cache
            .digest_functions
            .iter()
            .filter_map(|function| match *function {
                proto::DIGEST_SHA256 => Some("sha256"),
                proto::DIGEST_BLAKE3 => Some("blake3"),
                _ => None,
            })
            .collect();
        Ok(Capabilities {
            writable,
            anonymous_writes,
            digest_functions,
            updates_enabled: cache
                .action_cache_update_capabilities
                .is_some_and(|update| update.update_enabled),
        })
    }

    /// Uploads every action and pre-key record in the store at `store_dir`
    /// used at or after `since` (all of them without it), with the objects
    /// the server lacks.
    ///
    /// Blobs go first and pre-key records last, so a reader that finds an
    /// input set can find its action, and one that finds an action can fetch
    /// every object it names.
    ///
    /// A call that times out, or that the server answers as unavailable or
    /// overloaded, is retried after 1 s, 4 s and 15 s. When one still
    /// fails, only the entries that need it are left out, and
    /// [`PushReport::failed`] counts them: the rest of the store is pushed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wrap`] or [`Error::Cas`] when the local store cannot
    /// be read, [`Error::Status`] on the first call the server refuses, such
    /// as a write without write credentials, and the last [`Error::Timeout`]
    /// or [`Error::Status`] when transient failures left no action or no
    /// pre-key record pushed, since a reader then finds nothing.
    pub fn push(&self, store_dir: &Path, since: Option<SystemTime>) -> Result<PushReport> {
        let store = Store::open(store_dir)?;
        let recent = |when: SystemTime| since.is_none_or(|since| when >= since);
        let mut report = PushReport::default();
        let mut blobs: HashMap<proto::Digest, Source> = HashMap::new();
        let mut actions = Vec::new();
        let mut prekeys = Vec::new();

        let records: Vec<_> = tinymold_wrap::records(store_dir)?
            .into_iter()
            .filter(|record| recent(record.last_used))
            .collect();
        let started = Instant::now();
        let refreshed = self.runtime.block_on(self.refresh_actions(&records))?;
        report.refresh_time = started.elapsed();
        report.refreshed = refreshed.len() as u64;
        let started = Instant::now();
        let mut object_digests = HashMap::new();
        for record in records {
            match record.kind {
                RecordKind::Action { key, .. } => {
                    if refreshed.contains(&key) {
                        continue;
                    }
                    let key = naming::action_key(&key);
                    match action_result(&store, &record.path, &mut blobs, &mut object_digests) {
                        Some(mut result) => {
                            self.sign(Kind::Action, &key, &mut result, &mut blobs);
                            actions.push((key, result));
                        }
                        None => report.skipped += 1,
                    }
                }
                RecordKind::PreKey { key } => {
                    let Ok(text) = fs::read(&record.path) else {
                        continue;
                    };
                    let digest = naming::blob(&text);
                    blobs.insert(digest.clone(), Source::Bytes(text));
                    let mut result = ActionResult {
                        output_files: vec![output(PREKEY_FILE, digest)],
                        exit_code: 0,
                    };
                    let key = naming::prekey_key(&key);
                    self.sign(Kind::Prekey, &key, &mut result, &mut blobs);
                    prekeys.push((key, result));
                }
                _ => {}
            }
        }

        report.prepare_time = started.elapsed();
        let started = Instant::now();
        let failure =
            self.runtime
                .block_on(self.send(&store, &blobs, actions, prekeys, &mut report))?;
        report.transfer_time = started.elapsed();
        // Pushing part of a store is worth something. A push that left no
        // action, or no pre-key record, cannot serve a single hit: a reader
        // finds actions only through pre-keys.
        match failure {
            Some(error) if report.actions + report.refreshed == 0 || report.prekeys == 0 => Err(error),
            _ => Ok(report),
        }
    }

    /// Uploads the `blobs` the server lacks, then the `actions` and
    /// `prekeys` whose blobs all went up, counting them in `report`. Returns
    /// the last transient failure, if any call still failed after its
    /// retries.
    async fn send(
        &self,
        store: &Store,
        blobs: &HashMap<proto::Digest, Source>,
        actions: Vec<(proto::Digest, ActionResult)>,
        prekeys: Vec<(proto::Digest, ActionResult)>,
        report: &mut PushReport,
    ) -> Result<Option<Error>> {
        // Blobs whose upload still failed after its retries, and why.
        let mut failed = HashSet::new();
        let mut last = None;
        let digests: Vec<proto::Digest> = blobs.keys().cloned().collect();
        let mut missing = Vec::new();
        for batch in digests.chunks(FIND_BATCH) {
            missing.extend(retrying(|| self.client.find_missing(batch.to_vec())).await?);
        }
        let missing = missing
            .into_iter()
            .map(|digest| (blob_size(&digest), digest))
            .collect();
        in_flight_with_budget(
            missing,
            PUSH_IN_FLIGHT_BYTES,
            |digest| async move {
                let data = match blobs.get(&digest) {
                    Some(Source::Bytes(bytes)) => bytes.clone(),
                    Some(Source::Object(object)) => store.read(object)?,
                    // The server asked about a blob nobody sent it.
                    None => return Ok(None),
                };
                let result =
                    retrying(|| self.client.write_sharing(&digest, &data, PUSH_IN_FLIGHT_BYTES)).await;
                Ok::<_, Error>(Some((digest, data.len(), result)))
            },
            |upload| {
                let Some((digest, size, result)) = upload? else {
                    return Ok(());
                };
                match result {
                    Ok(()) => {
                        report.blobs += 1;
                        report.bytes += size as u64;
                    }
                    Err(error) if transient(&error) => {
                        failed.insert(digest);
                        last = Some(error);
                    }
                    Err(error) => return Err(error),
                }
                Ok(())
            },
        )
        .await?;
        // An entry goes up only once every blob it names is there, and
        // every action before any pre-key record.
        for (entries, action) in [(actions, true), (prekeys, false)] {
            let mut ready = Vec::new();
            for (key, result) in entries {
                let complete = result
                    .output_files
                    .iter()
                    .filter_map(|file| file.digest.as_ref())
                    .all(|digest| !failed.contains(digest));
                if complete {
                    ready.push((key, result));
                } else {
                    report.failed += 1;
                }
            }
            let ready = ready.into_iter().map(|entry| (0, entry)).collect();
            in_flight(
                ready,
                |(key, result)| {
                    retrying(move || {
                        self.client
                            .update_action_result(key.clone(), result.clone())
                    })
                },
                |update| {
                    match update {
                        Ok(()) if action => report.actions += 1,
                        Ok(()) => report.prekeys += 1,
                        Err(error) if transient(&error) => {
                            report.failed += 1;
                            last = Some(error);
                        }
                        Err(error) => return Err(error),
                    }
                    Ok(())
                },
            )
            .await?;
        }
        Ok(last)
    }

    /// With a signing key, adds the signature of the entry `result` under
    /// `key` to it, and its blob to `blobs`. The entry's record is its first
    /// output file.
    fn sign(
        &self,
        kind: Kind,
        key: &proto::Digest,
        result: &mut ActionResult,
        blobs: &mut HashMap<proto::Digest, Source>,
    ) {
        let Some(signer) = self.signing_key() else {
            return;
        };
        let Some(record) = result
            .output_files
            .first()
            .and_then(|file| file.digest.clone())
        else {
            return;
        };
        let signature = signer.sign(kind, key, &record);
        let digest = naming::blob(&signature);
        blobs
            .entry(digest.clone())
            .or_insert(Source::Bytes(signature));
        result.output_files.push(output(SIGNATURE_FILE, digest));
    }

    /// With trusted keys, checks that the entry `result` under `key`, whose
    /// record is the blob `record`, carries a signature by one of them.
    async fn verify(
        &self,
        kind: Kind,
        key: &proto::Digest,
        record: &proto::Digest,
        result: &ActionResult,
    ) -> Result<()> {
        let Some(trusted) = self.trusted_keys() else {
            return Ok(());
        };
        let signature = result
            .output_files
            .iter()
            .find(|file| file.path == SIGNATURE_FILE)
            .and_then(|file| file.digest.clone())
            .ok_or_else(|| Error::Untrusted {
                key: key.hash.clone(),
                reason: "it is not signed",
            })?;
        let blob = self.client.read(&signature).await?;
        trusted.verify(kind, key, record, &blob)
    }

    fn input_sets_of(&self, pre: &Digest) -> Result<Option<String>> {
        self.runtime.block_on(async {
            let key = naming::prekey_key(pre);
            let Some(result) = self.client.get_action_result(key.clone()).await? else {
                return Ok(None);
            };
            let digest = find_file(&result, PREKEY_FILE, &key)?;
            // The record is only used once the signature checks out, but
            // reading both at once saves a round trip. A refused signature
            // drops the read at once, whatever the server is sending.
            let ((), bytes) = future::try_join(
                self.verify(Kind::Prekey, &key, &digest, &result),
                self.client.read(&digest),
            )
            .await?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| Error::Malformed {
                    key: key.hash,
                    reason: "its pre-key record is not text",
                })
        })
    }

    fn fetch_action_into(&self, key: &Digest, store: &Store) -> Result<Option<String>> {
        self.runtime.block_on(async {
            let key = naming::action_key(key);
            let Some(result) = self.client.get_action_result(key.clone()).await? else {
                return Ok(None);
            };
            let record = find_file(&result, ACTION_FILE, &key)?;
            // Before any object: an untrusted entry fetches nothing.
            self.verify(Kind::Action, &key, &record, &result).await?;
            let malformed = |reason| Error::Malformed {
                key: key.hash.clone(),
                reason,
            };
            let mut wanted = Vec::new();
            for file in &result.output_files {
                if file.path == ACTION_FILE || file.path == SIGNATURE_FILE {
                    continue;
                }
                let name = Digest::from_str(&file.path)
                    .map_err(|_| malformed("an output is not named by its digest"))?;
                if store.contains(&name) {
                    continue;
                }
                let digest = file
                    .digest
                    .clone()
                    .ok_or_else(|| malformed("an output has no digest"))?;
                wanted.push((name, digest));
            }
            // Every object, and the record, in flight together under one
            // limit: each read is a round trip. Each object goes into the
            // store as it arrives, so memory holds only what is in flight.
            let reads = std::iter::once((blob_size(&record), None))
                .chain(
                    wanted
                        .into_iter()
                        .map(|(name, digest)| (blob_size(&digest), Some((name, digest)))),
                )
                .collect();
            let mut text = None;
            in_flight(
                reads,
                |object| {
                    let record = &record;
                    async move {
                        let (name, digest) = match object {
                            Some((name, digest)) => (Some(name), digest),
                            None => (None, record.clone()),
                        };
                        let bytes = self.client.read_sharing(&digest, IN_FLIGHT_BYTES).await;
                        (name, bytes)
                    }
                },
                |(name, bytes)| match name {
                    None => {
                        text = Some(bytes?);
                        Ok(())
                    }
                    Some(name) if store.insert_bytes(&bytes?)? == name => Ok(()),
                    Some(_) => Err(malformed("an output does not match its name")),
                },
            )
            .await?;
            let text = text.ok_or_else(|| malformed("its action record was never read"))?;
            String::from_utf8(text)
                .map(Some)
                .map_err(|_| Error::Malformed {
                    key: key.hash,
                    reason: "its action record is not text",
                })
        })
    }
}

impl tinymold_wrap::Remote for RemoteCache {
    fn input_sets(&self, pre: &Digest) -> std::result::Result<Option<String>, RemoteError> {
        Ok(self.input_sets_of(pre)?)
    }

    fn fetch_action(
        &self,
        key: &Digest,
        store: &Store,
    ) -> std::result::Result<Option<String>, RemoteError> {
        Ok(self.fetch_action_into(key, store)?)
    }
}

/// The `ActionResult` for the action recorded at `path`, adding every blob it
/// needs to `blobs`; `None` when the record is unreadable or an object it
/// names is gone.
fn action_result(
    store: &Store,
    path: &Path,
    blobs: &mut HashMap<proto::Digest, Source>,
    object_digests: &mut HashMap<Digest, proto::Digest>,
) -> Option<ActionResult> {
    let text = fs::read(path).ok()?;
    let references = tinymold_wrap::action_references(path).ok()?;
    let record = naming::blob(&text);
    let mut files = vec![output(ACTION_FILE, record.clone())];
    let mut needed = vec![(record, Source::Bytes(text))];
    let mut seen = HashSet::new();
    for object in references {
        if !seen.insert(object) {
            continue;
        }
        let digest = match object_digests.get(&object) {
            Some(digest) if store.contains(&object) => digest.clone(),
            _ => {
                let bytes = store.read(&object).ok()?;
                let digest = naming::blob(&bytes);
                object_digests.insert(object, digest.clone());
                digest
            }
        };
        files.push(output(&object.to_hex(), digest.clone()));
        needed.push((digest, Source::Object(object)));
    }
    // Only a complete action's blobs are uploaded.
    for (digest, source) in needed {
        blobs.entry(digest).or_insert(source);
    }
    Some(ActionResult {
        output_files: files,
        exit_code: 0,
    })
}

fn output(path: &str, digest: proto::Digest) -> OutputFile {
    OutputFile {
        path: path.to_owned(),
        digest: Some(digest),
        is_executable: false,
    }
}

/// The size a digest declares, for the in-flight byte budget; 0 when it
/// declares none.
fn blob_size(digest: &proto::Digest) -> u64 {
    u64::try_from(digest.size_bytes).unwrap_or(0)
}

/// Runs `start` on every item, oldest first, keeping up to
/// [`IN_FLIGHT_CALLS`] of them and [`IN_FLIGHT_BYTES`] of their sizes in
/// flight; an item larger than the budget runs alone. Hands each output to
/// `done` as it finishes, and stops at the first error `done` returns.
async fn in_flight<I, T, Fut>(
    items: Vec<(u64, I)>,
    start: impl FnMut(I) -> Fut,
    done: impl FnMut(T) -> Result<()>,
) -> Result<()>
where
    Fut: Future<Output = T>,
{
    in_flight_with_budget(items, IN_FLIGHT_BYTES, start, done).await
}

/// The transfer scheduler with a caller-selected byte budget.
async fn in_flight_with_budget<I, T, Fut>(
    items: Vec<(u64, I)>,
    budget: u64,
    mut start: impl FnMut(I) -> Fut,
    mut done: impl FnMut(T) -> Result<()>,
) -> Result<()>
where
    Fut: Future<Output = T>,
{
    let mut queue = items.into_iter().peekable();
    let mut running = stream::FuturesUnordered::new();
    let mut bytes: u64 = 0;
    loop {
        while let Some(&(size, _)) = queue.peek() {
            let full =
                running.len() >= IN_FLIGHT_CALLS || bytes.saturating_add(size) > budget;
            if full && !running.is_empty() {
                break;
            }
            let Some((size, item)) = queue.next() else {
                break;
            };
            bytes = bytes.saturating_add(size);
            let work = start(item);
            running.push(async move { (size, work.await) });
        }
        let Some((size, output)) = running.next().await else {
            return Ok(());
        };
        bytes -= size;
        done(output)?;
    }
}

/// Runs `call`, and again after each of [`RETRY_DELAYS`] while it fails in a
/// way that may pass: a timeout, or a server that is unavailable or busy.
async fn retrying<T, F, Fut>(mut call: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut delays = RETRY_DELAYS.iter();
    loop {
        match call().await {
            Err(error) if transient(&error) => match delays.next() {
                Some(delay) => tokio::time::sleep(*delay).await,
                None => return Err(error),
            },
            done => return done,
        }
    }
}

/// Whether `error` may not happen again: a call that timed out, a server
/// that said it is unavailable, overloaded or aborted the call, or a proxy
/// in front of it that failed the call (see [`proxy_hiccup`]; the channel
/// reconnects). A refusal, such as a missing write credential, is not.
fn transient(error: &Error) -> bool {
    match error {
        Error::Timeout(_) => true,
        Error::Status { status, .. } => {
            matches!(
                status.code(),
                tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::ResourceExhausted
                    | tonic::Code::Aborted
            ) || (status.code() == tonic::Code::Internal && proxy_hiccup(status.message()))
        }
        _ => false,
    }
}

/// Whether an `INTERNAL` status's `message` is tonic's report of a proxy
/// in front of the server failing the call rather than the server refusing
/// it: a connection closed under the call (an HTTP/2 GOAWAY, which nginx
/// sends after its per-connection request limit), or a 502, 503 or 504 with
/// a body that is not gRPC (nginx while `tinymold serve` restarts).
fn proxy_hiccup(message: &str) -> bool {
    message.starts_with("h2 protocol error")
        || ["502", "503", "504"]
            .iter()
            .any(|code| message.contains(&format!("while receiving response with status: {code} ")))
}

/// The digest of the output file at `path` in `result`.
fn find_file(result: &ActionResult, path: &str, key: &proto::Digest) -> Result<proto::Digest> {
    result
        .output_files
        .iter()
        .find(|file| file.path == path)
        .and_then(|file| file.digest.clone())
        .ok_or_else(|| Error::Malformed {
            key: key.hash.clone(),
            reason: "it has no tinymold record",
        })
}

#[cfg(test)]
mod test;
