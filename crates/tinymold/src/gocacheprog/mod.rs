//! `tinymold gocacheprog`: Go's build cache, kept in tinymold's store.
//!
//! Go 1.24 and later run the program named by `GOCACHEPROG` as a child and
//! ask it, over stdin and stdout, for the outputs of build actions
//! (`docs/specs/cache-server.md`, client-side adapters):
//!
//! ```sh
//! GOCACHEPROG="tinymold gocacheprog" go build ./...
//! ```
//!
//! Entries live in the cache server's store, in the `native` front-end's
//! `go` tenant, so they share its objects, its collector and, in CI, its
//! GitHub cache entry. No server needs to be running: the store is opened
//! directly, and is safe to share with a running `tinymold serve`.
//!
//! - `get` answers a hit with the stored object's own path as `DiskPath`.
//!   Objects are immutable and read-only, so go can read it in place and
//!   nothing is copied.
//! - `put` streams the base64 body into the store, keeps go's `OutputID`
//!   with the entry, and answers with the object's path.
//! - `close` answers and ends the session; so does the end of input.
//!
//! A failure that concerns one request (a store error, a body of the wrong
//! size) is answered with `Err`, and go builds that action itself. Input
//! that does not follow the protocol ends the session with an error.

mod message;

use std::io::{self, BufRead, Read, Write};

use tinymold_store::{Entry, FrontEnd, Namespace, Store};

use crate::{Error, Result};
use message::{BodyLine, Request, json_bytes, json_string, parse_request};

/// The tenant Go's entries are kept under.
const TENANT: &str = "go";

/// The metadata key that holds go's `OutputID`, hex-encoded.
const OUTPUT_ID: &str = "go-output-id";

/// Serves the `GOCACHEPROG` protocol on `input` and `output` from `store`,
/// until `close` or the end of input.
///
/// # Errors
///
/// Returns [`Error::CacheProg`] when the input does not follow the
/// protocol, [`Error::Input`] when it cannot be read, and [`Error::Output`]
/// when a response cannot be written.
pub fn serve(store: &Store, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let namespace = Namespace::new(FrontEnd::Native, TENANT)?;
    respond(
        &mut output,
        r#"{"ID":0,"KnownCommands":["get","put","close"]}"#,
    )?;

    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line).map_err(Error::Input)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let request = parse_request(&line).map_err(Error::CacheProg)?;
        let response = match request.command.as_str() {
            "get" => get(store, &namespace, &request),
            "put" => put(store, &namespace, &request, &mut input)?,
            "close" => {
                respond(&mut output, &format!(r#"{{"ID":{}}}"#, request.id))?;
                return Ok(());
            }
            _ => failure(request.id, "unknown command"),
        };
        respond(&mut output, &response)?;
    }
}

fn get(store: &Store, namespace: &Namespace, request: &Request) -> String {
    let found = (|| {
        let key = namespace.key(&request.action_id).ok()?;
        let entry = store.get(&key).ok()??;
        let blob = *entry.blobs.first()?;
        let path = store.cas().object_path(&blob);
        let size = std::fs::metadata(&path).ok()?.len();
        let output_id = hex_decode(entry.meta.get(OUTPUT_ID)?)?;
        Some((path, size, output_id))
    })();
    match found {
        Some((path, size, output_id)) => format!(
            r#"{{"ID":{},"OutputID":{},"Size":{size},"DiskPath":{}}}"#,
            request.id,
            json_bytes(&output_id),
            json_string(&path.to_string_lossy()),
        ),
        None => format!(r#"{{"ID":{},"Miss":true}}"#, request.id),
    }
}

/// Stores a `put`'s body. Errors only when the body line cannot be read
/// to its end, since the session could not continue in step after that.
fn put(
    store: &Store,
    namespace: &Namespace,
    request: &Request,
    input: &mut impl BufRead,
) -> Result<String> {
    let (stored, size) = if request.body_size == 0 {
        (store.ingest(io::empty(), None), 0)
    } else {
        let mut body = BodyLine::new(input);
        let mut counted = Counted {
            inner: body.decoded(),
            count: 0,
        };
        let stored = store.ingest(&mut counted, None);
        let size = counted.count;
        drop(counted);
        // Whatever the store did, the rest of the line must be read before
        // the next request.
        body.finish().map_err(Error::Input)?;
        (stored, size)
    };

    let ingested = match stored {
        Ok(ingested) => ingested,
        Err(error) => return Ok(failure(request.id, &format!("store failed: {error}"))),
    };
    if size != request.body_size {
        return Ok(failure(
            request.id,
            "the body's size does not match BodySize",
        ));
    }
    let Ok(key) = namespace.key(&request.action_id) else {
        return Ok(failure(request.id, "ActionID too long"));
    };
    let mut entry = Entry::new(vec![ingested.digest]);
    entry
        .meta
        .insert(OUTPUT_ID.to_owned(), hex_encode(&request.output_id));
    if let Err(error) = store.put(&key, &entry) {
        return Ok(failure(request.id, &format!("store failed: {error}")));
    }
    let path = store.cas().object_path(&ingested.digest);
    Ok(format!(
        r#"{{"ID":{},"DiskPath":{}}}"#,
        request.id,
        json_string(&path.to_string_lossy()),
    ))
}

fn failure(id: i64, message: &str) -> String {
    format!(r#"{{"ID":{id},"Err":{}}}"#, json_string(message))
}

fn respond(output: &mut impl Write, line: &str) -> Result<()> {
    writeln!(output, "{line}")
        .and_then(|()| output.flush())
        .map_err(Error::Output)
}

/// Counts the bytes read through it.
struct Counted<R> {
    inner: R,
    count: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.count += read as u64;
        Ok(read)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod test;
