//! Running rustc: unchanged, or with its output streamed and kept.
//!
//! Cargo pipelines builds on rustc's stdout: a dependent starts as soon as the
//! JSON `artifact` notification for an upstream `.rmeta` arrives, long before
//! the upstream compile finishes. On a miss the wrapper must therefore pass
//! rustc's output through as it is written, not when rustc exits, and keep a
//! copy for the cache at the same time. Each stream gets a thread that writes
//! every chunk through and appends it to a buffer, and both note, in the
//! order they write them through, which stream each chunk went to.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use crate::{Error, Result};

/// What a captured run produced.
#[derive(Debug)]
pub(crate) struct Captured {
    /// The exit code; 1 when the process died to a signal.
    pub(crate) code: i32,
    /// Everything written to stdout.
    pub(crate) stdout: Vec<u8>,
    /// Everything written to stderr.
    pub(crate) stderr: Vec<u8>,
    /// How the two streams interleaved: the lengths of alternating runs of
    /// output, starting with stdout. `[1, 2, 3]` is one byte of stdout,
    /// then two of stderr, then three of stdout; `[0, 3]` is three bytes of
    /// stderr only.
    pub(crate) segments: Vec<u64>,
}

/// One of the two output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// Appends `len` bytes of `stream` to `segments`, laid out as
/// [`Captured::segments`] describes.
fn note_segment(segments: &mut Vec<u64>, stream: Stream, len: u64) {
    if segments.is_empty() && stream == Stream::Stderr {
        segments.push(0);
    }
    // Runs at even indexes are stdout's, at odd ones stderr's.
    let parity = usize::from(stream == Stream::Stderr);
    let runs = segments.len();
    match segments.last_mut() {
        Some(last) if (runs - 1) % 2 == parity => *last += len,
        _ => segments.push(len),
    }
}

/// Runs `program` with `args`, inheriting stdio, and returns its exit code.
pub(crate) fn pass_through(program: &Path, args: &[OsString]) -> Result<i32> {
    let mut command = Command::new(program);
    command.args(args);
    pass_through_command(command, program)
}

/// Runs `command`, which runs `program`, inheriting stdio, and returns its
/// exit code.
pub(crate) fn pass_through_command(mut command: Command, program: &Path) -> Result<i32> {
    let status = command.status().map_err(spawn_error(program))?;
    Ok(status.code().unwrap_or(1))
}

/// Runs `program` with `args`, streaming its stdout and stderr through to
/// ours while keeping a copy of each.
pub(crate) fn run_captured(program: &Path, args: &[OsString]) -> Result<Captured> {
    let mut command = Command::new(program);
    command.args(args);
    capture(command, program)
}

/// Runs `command`, which runs `program`, streaming its stdout and stderr
/// through to ours while keeping a copy of each. Its stdin is whatever
/// `command` sets, or ours.
pub(crate) fn capture(mut command: Command, program: &Path) -> Result<Captured> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error(program))?;
    let segments = Arc::new(Mutex::new(Vec::new()));
    let stdout = child
        .stdout
        .take()
        .map(|pipe| tee(pipe, io::stdout, Stream::Stdout, Arc::clone(&segments)));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| tee(pipe, io::stderr, Stream::Stderr, Arc::clone(&segments)));
    let status = child.wait().map_err(spawn_error(program))?;
    let collect = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
        handle
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);
    let segments = std::mem::take(&mut *segments.lock().unwrap_or_else(PoisonError::into_inner));
    Ok(Captured {
        code: status.code().unwrap_or(1),
        stdout,
        stderr,
        segments,
    })
}

/// Copies `source`, the output `stream`, to the writer `sink` makes, chunk
/// by chunk, flushing each chunk and noting it in `segments`, and returns
/// everything copied. A chunk is written through while `segments` is held,
/// so the noted order is the order the chunks went out in.
fn tee<R, W>(
    mut source: R,
    sink: fn() -> W,
    stream: Stream,
    segments: Arc<Mutex<Vec<u64>>>,
) -> thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
    W: Write + 'static,
{
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0_u8; 8192];
        while let Ok(read) = source.read(&mut buffer) {
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            kept.extend_from_slice(chunk);
            let mut segments = segments.lock().unwrap_or_else(PoisonError::into_inner);
            note_segment(&mut segments, stream, read as u64);
            let mut out = sink();
            // A closed stdout must not stop the compile; the bytes are still
            // kept, and rustc's own exit status decides the outcome.
            let _ = out.write_all(chunk).and_then(|()| out.flush());
        }
        kept
    })
}

fn spawn_error(program: &Path) -> impl FnOnce(io::Error) -> Error {
    let program = program.to_path_buf();
    move |source| Error::Spawn { program, source }
}

#[cfg(test)]
mod test;
