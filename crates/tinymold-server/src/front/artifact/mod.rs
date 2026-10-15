//! Single-artifact entries: one opaque blob under a client's key.
//!
//! Turborepo, Nx and Gradle all store a task's outputs as one archive under
//! a key the client computed. This module is what they share: finding the
//! stored blob for a key, and streaming an upload into the store under a
//! key. The protocol details (paths, headers, status codes) stay in each
//! front-end.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use tinymold_store::{Entry, Key};

use crate::front::{blocking, now};
use crate::route::State;
use crate::transfer::{self, Opened, UploadError};

/// A stored artifact, opened for a response, and the metadata kept with it.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) opened: Opened,
    pub(crate) meta: BTreeMap<String, String>,
}

/// The artifact stored under `key`, if any, looked up and opened in one
/// trip to the blocking pool.
pub(crate) async fn find(state: &Arc<State>, key: Key) -> Option<Found> {
    let store = state.store.clone();
    blocking(move || {
        let entry = store.get(&key).ok()??;
        let blob = *entry.blobs.first()?;
        Some(Found {
            opened: transfer::open(&store.cas().object_path(&blob))?,
            meta: entry.meta,
        })
    })
    .await
    .flatten()
}

/// Whether an artifact is stored under `key`.
pub(crate) async fn exists(state: &Arc<State>, key: Key) -> bool {
    let store = state.store.clone();
    blocking(move || store.contains(&key))
        .await
        .unwrap_or(false)
}

/// Streams `body` into the store as the artifact under `key`, with `meta`,
/// replacing any artifact already there. Returns the outcome and how many
/// bytes arrived.
pub(crate) async fn put<B>(
    state: &Arc<State>,
    body: B,
    key: Key,
    meta: BTreeMap<String, String>,
) -> (Result<(), UploadError<tinymold_store::Error>>, u64)
where
    B: hyper::body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let store = state.store.clone();
    transfer::upload(body, state.max_body, move |reader| {
        let ingested = store.ingest(reader, None)?;
        let mut entry = Entry::new(vec![ingested.digest]);
        entry.meta = meta;
        entry.written = now();
        store.put(&key, &entry)
    })
    .await
}
