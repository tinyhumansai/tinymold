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
use std::time::SystemTime;

use tinymold_cas::Store;
use tinymold_hash::Digest;
use tinymold_wrap::{RecordKind, RemoteError};

use crate::client::Client;
use crate::config::RemoteConfig;
use crate::naming::{self, ACTION_FILE, PREKEY_FILE};
use crate::proto::{self, ActionResult, OutputFile};
use crate::{Error, Result};

pub use types::{Capabilities, PushReport};

/// How many digests one `FindMissingBlobs` call asks about.
const FIND_BATCH: usize = 1000;

/// A remote REAPI cache, called synchronously.
#[derive(Debug)]
pub struct RemoteCache {
    runtime: tokio::runtime::Runtime,
    client: Client,
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
        Ok(Self { runtime, client })
    }

    /// Asks the server what it supports. A wrong credential or an unreachable
    /// server fails here, which makes this the `tinymold remote check` call.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Status`] or [`Error::Timeout`] when the call fails.
    pub fn check(&self) -> Result<Capabilities> {
        let capabilities = self.runtime.block_on(self.client.capabilities())?;
        let cache = capabilities.cache_capabilities.unwrap_or_default();
        Ok(Capabilities {
            sha256: cache.digest_functions.contains(&proto::DIGEST_SHA256),
            blake3: cache.digest_functions.contains(&proto::DIGEST_BLAKE3),
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
    /// # Errors
    ///
    /// Returns [`Error::Wrap`] or [`Error::Cas`] when the local store cannot
    /// be read, and [`Error::Status`] or [`Error::Timeout`] on the first call
    /// the server refuses, such as a write without write credentials.
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
                    match action_result(&store, &record.path, &mut blobs) {
                        Some(result) => actions.push((naming::action_key(&key), result)),
                        None => report.skipped += 1,
                    }
                }
                RecordKind::PreKey { key } => {
                    let Ok(text) = fs::read(&record.path) else {
                        continue;
                    };
                    let digest = naming::blob(&text);
                    blobs.insert(digest.clone(), Source::Bytes(text));
                    let result = ActionResult {
                        output_files: vec![output(PREKEY_FILE, digest)],
                        exit_code: 0,
                    };
                    prekeys.push((naming::prekey_key(&key), result));
                }
                _ => {}
            }
        }

        self.runtime.block_on(async {
            let digests: Vec<proto::Digest> = blobs.keys().cloned().collect();
            for batch in digests.chunks(FIND_BATCH) {
                for missing in self.client.find_missing(batch.to_vec()).await? {
                    let data = match blobs.get(&missing) {
                        Some(Source::Bytes(bytes)) => bytes.clone(),
                        Some(Source::Object(object)) => store.read(object)?,
                        // The server asked about a blob nobody sent it.
                        None => continue,
                    };
                    report.blobs += 1;
                    report.bytes += data.len() as u64;
                    self.client.write(&missing, data).await?;
                }
            }
            for (key, result) in actions {
                self.client.update_action_result(key, result).await?;
                report.actions += 1;
            }
            for (key, result) in prekeys {
                self.client.update_action_result(key, result).await?;
                report.prekeys += 1;
            }
            Ok::<(), Error>(())
        })?;
        Ok(report)
    }

    fn input_sets_of(&self, pre: &Digest) -> Result<Option<String>> {
        self.runtime.block_on(async {
            let key = naming::prekey_key(pre);
            let Some(result) = self.client.get_action_result(key.clone()).await? else {
                return Ok(None);
            };
            let digest = find_file(&result, PREKEY_FILE, &key)?;
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
            for file in &result.output_files {
                if file.path == ACTION_FILE {
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
    blobs.insert(record, Source::Bytes(text));
    let mut seen = HashSet::new();
    for object in references {
        if !seen.insert(object) {
            continue;
        }
        let bytes = store.read(&object).ok()?;
        let digest = naming::blob(&bytes);
        files.push(output(&object.to_hex(), digest.clone()));
        blobs.entry(digest).or_insert(Source::Object(object));
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
