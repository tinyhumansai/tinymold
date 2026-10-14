//! Running rustc: unchanged, or with its output streamed and kept.
//!
//! Cargo pipelines builds on rustc's stdout: a dependent starts as soon as the
//! JSON `artifact` notification for an upstream `.rmeta` arrives, long before
//! the upstream compile finishes. On a miss the wrapper must therefore pass
//! rustc's output through as it is written, not when rustc exits, and keep a
//! copy for the cache at the same time. Each stream gets a thread that writes
//! every chunk through and appends it to a buffer.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
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
/// through to ours while keeping a copy of each.
pub(crate) fn capture(mut command: Command, program: &Path) -> Result<Captured> {
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error(program))?;
    let stdout = child.stdout.take().map(|pipe| tee(pipe, io::stdout));
    let stderr = child.stderr.take().map(|pipe| tee(pipe, io::stderr));
    let status = child.wait().map_err(spawn_error(program))?;
    let collect = |handle: Option<thread::JoinHandle<Vec<u8>>>| {
        handle
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    };
    Ok(Captured {
        code: status.code().unwrap_or(1),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Copies `source` to the writer `sink` makes, chunk by chunk, flushing each
/// chunk, and returns everything copied.
fn tee<R, W>(mut source: R, sink: fn() -> W) -> thread::JoinHandle<Vec<u8>>
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
