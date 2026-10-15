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

mod types;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

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

        for record in tinymold_wrap::records(store_dir)? {
            if !recent(record.last_used) {
                continue;
            }
            match record.kind {
                RecordKind::Action { key, .. } => {
                    let key = naming::action_key(&key);
                    match action_result(&store, &record.path, &mut blobs) {
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

        let failure = self.runtime.block_on(async {
            // Blobs whose upload still failed after its retries, and why.
            let mut failed = HashSet::new();
            let mut last = None;
            let digests: Vec<proto::Digest> = blobs.keys().cloned().collect();
            for batch in digests.chunks(FIND_BATCH) {
                let missing = retrying(|| self.client.find_missing(batch.to_vec())).await?;
                for missing in missing {
                    let data = match blobs.get(&missing) {
                        Some(Source::Bytes(bytes)) => bytes.clone(),
                        Some(Source::Object(object)) => store.read(object)?,
                        // The server asked about a blob nobody sent it.
                        None => continue,
                    };
                    match retrying(|| self.client.write(&missing, &data)).await {
                        Ok(()) => {
                            report.blobs += 1;
                            report.bytes += data.len() as u64;
                        }
                        Err(error) if transient(&error) => {
                            failed.insert(missing);
                            last = Some(error);
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            // An entry goes up only once every blob it names is there.
            let entries = actions
                .into_iter()
                .map(|entry| (entry, true))
                .chain(prekeys.into_iter().map(|entry| (entry, false)));
            for ((key, result), action) in entries {
                let complete = result
                    .output_files
                    .iter()
                    .filter_map(|file| file.digest.as_ref())
                    .all(|digest| !failed.contains(digest));
                if !complete {
                    report.failed += 1;
                    continue;
                }
                match retrying(|| {
                    self.client
                        .update_action_result(key.clone(), result.clone())
                })
                .await
                {
                    Ok(()) if action => report.actions += 1,
                    Ok(()) => report.prekeys += 1,
                    Err(error) if transient(&error) => {
                        report.failed += 1;
                        last = Some(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok::<_, Error>(last)
        })?;
        // Pushing part of a store is worth something. A push that left no
        // action, or no pre-key record, cannot serve a single hit: a reader
        // finds actions only through pre-keys.
        match failure {
            Some(error) if report.actions == 0 || report.prekeys == 0 => Err(error),
            _ => Ok(report),
        }
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
            self.verify(Kind::Prekey, &key, &digest, &result).await?;
            let bytes = self.client.read(&digest).await?;
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
            for file in &result.output_files {
                if file.path == ACTION_FILE || file.path == SIGNATURE_FILE {
                    continue;
                }
                let malformed = |reason| Error::Malformed {
                    key: key.hash.clone(),
                    reason,
                };
                let name = Digest::from_str(&file.path)
                    .map_err(|_| malformed("an output is not named by its digest"))?;
                if store.contains(&name) {
                    continue;
                }
                let digest = file
                    .digest
                    .clone()
                    .ok_or_else(|| malformed("an output has no digest"))?;
                let bytes = self.client.read(&digest).await?;
                if store.insert_bytes(&bytes)? != name {
                    return Err(malformed("an output does not match its name"));
                }
            }
            let text = self.client.read(&record).await?;
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
        let bytes = store.read(&object).ok()?;
        let digest = naming::blob(&bytes);
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

/// Whether `error` may not happen again: a call that timed out, or a server
/// that said it is unavailable, overloaded or aborted the call. A refusal,
/// such as a missing write credential, is not.
fn transient(error: &Error) -> bool {
    match error {
        Error::Timeout(_) => true,
        Error::Status { status, .. } => matches!(
            status.code(),
            tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::ResourceExhausted
                | tonic::Code::Aborted
        ),
        _ => false,
    }
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
