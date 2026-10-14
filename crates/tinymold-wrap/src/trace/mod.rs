//! Timing every step of one wrapper invocation, for profiling a build.
//!
//! With [`Config::trace`](crate::Config::trace) set, the wrapper appends one
//! line per step to that file once the invocation is done. Each line is a
//! Chrome trace "complete" event (`"ph":"X"`), so `bench/profile_build.py` can merge
//! the lines of every wrapper process with cargo's own unit timeline into one
//! trace that Perfetto or `chrome://tracing` opens:
//!
//! ```text
//! {"name":"pre_key","cat":"wrapper","ph":"X","ts":1760443200000000,"dur":812,"pid":4242,"tid":0,"args":{"crate":"syn","outcome":"miss"}}
//! ```
//!
//! `ts` is microseconds since the Unix epoch and `dur` is microseconds, so
//! events from concurrent wrappers line up on one clock. The steps are
//! `classify`, `pre_key`, `lookup`, `restore`, `rustc`, `ingest`, and
//! `pass_through`, plus one `invocation` event spanning them all.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The steps of one invocation recorded so far.
#[derive(Debug)]
pub(crate) struct Trace {
    epoch_us: u128,
    origin: Instant,
    spans: Vec<Span>,
}

/// One timed step, as microsecond offsets from the trace's origin.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    name: &'static str,
    start_us: u128,
    dur_us: u128,
}

impl Trace {
    /// A trace whose clock starts now.
    pub(crate) fn start() -> Self {
        let epoch_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_micros());
        Self {
            epoch_us,
            origin: Instant::now(),
            spans: Vec::new(),
        }
    }

    /// Runs `step` and records how long it took under `name`.
    pub(crate) fn span<T>(&mut self, name: &'static str, step: impl FnOnce() -> T) -> T {
        let started = self.origin.elapsed();
        let value = step();
        let ended = self.origin.elapsed();
        self.spans.push(Span {
            name,
            start_us: started.as_micros(),
            dur_us: (ended - started).as_micros(),
        });
        value
    }

    /// The trace lines for every recorded step plus the whole invocation,
    /// labelled with the crate and how the invocation was served.
    fn lines(&self, pid: u32, crate_name: &str, outcome: &str) -> String {
        let total = Span {
            name: "invocation",
            start_us: 0,
            dur_us: self.origin.elapsed().as_micros(),
        };
        let args = format!(
            r#"{{"crate":"{}","outcome":"{}"}}"#,
            escape(crate_name),
            escape(outcome)
        );
        std::iter::once(&total)
            .chain(&self.spans)
            .map(|span| {
                format!(
                    r#"{{"name":"{}","cat":"wrapper","ph":"X","ts":{},"dur":{},"pid":{pid},"tid":0,"args":{args}}}"#,
                    span.name,
                    self.epoch_us + span.start_us,
                    span.dur_us,
                ) + "\n"
            })
            .collect()
    }

    /// Appends this invocation's lines to `path`. Profiling never fails a
    /// build, so a file that cannot be written is skipped.
    pub(crate) fn write(&self, path: &Path, crate_name: &str, outcome: &str) {
        let lines = self.lines(std::process::id(), crate_name, outcome);
        // One write keeps concurrent wrappers' lines whole.
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(lines.as_bytes());
        }
    }
}

/// `text` escaped for a JSON string.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            c if u32::from(c) < 0x20 => escaped.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod test;
