//! `tinymold stats`: what the wrapper did, from its log.
//!
//! With `TINYMOLD_LOG` set, the wrapper appends one line per invocation:
//! `<outcome> <crate> <milliseconds>ms <detail>`. The outcome is one of:
//!
//! - `hit`: restored from the local store;
//! - `remote`: fetched from the remote tier, then restored;
//! - `miss`: compiled;
//! - `pass`: run unchanged, not cacheable;
//! - `error`: a cache failure, after which rustc ran unchanged.
//!
//! [`Stats::parse`] counts them, and the hit rate is hits over cacheable
//! invocations. CI prints the [`Stats::markdown`] table into its job summary,
//! and can fail a job whose hit rate is too low.

use std::fmt::{self, Write as _};

/// One compiled unit, for the list of slowest misses.
#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    /// The crate that compiled.
    pub crate_name: String,
    /// How long the wrapper took, compile included, in milliseconds.
    pub millis: f64,
}

/// Counts and times from one wrapper log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// Local hits.
    pub hits: u64,
    /// Hits fetched from the remote tier.
    pub remote_hits: u64,
    /// Compiles.
    pub misses: u64,
    /// Compiles whose outputs could not be stored.
    pub unstored: u64,
    /// Invocations run unchanged because they are not cacheable.
    pub passes: u64,
    /// Cache failures, after which rustc ran unchanged.
    pub errors: u64,
    /// Total wrapper time on hits of either kind, in milliseconds.
    pub hit_millis: f64,
    /// Total wrapper time on misses, compile included, in milliseconds.
    pub miss_millis: f64,
    /// Lines that are not wrapper log lines.
    pub unparsed: u64,
    /// The slowest misses, slowest first; at most [`Stats::SLOWEST`].
    pub slowest: Vec<Miss>,
}

impl Stats {
    /// How many of the slowest misses are kept.
    pub const SLOWEST: usize = 5;

    /// Counts the lines of a wrapper log.
    #[must_use]
    pub fn parse(log: &str) -> Self {
        let mut stats = Self::default();
        let mut misses = Vec::new();
        for line in log.lines().filter(|line| !line.trim().is_empty()) {
            let mut parts = line.splitn(4, ' ');
            let (Some(outcome), Some(crate_name), Some(time)) =
                (parts.next(), parts.next(), parts.next())
            else {
                stats.unparsed += 1;
                continue;
            };
            let Some(millis) = time
                .strip_suffix("ms")
                .and_then(|number| number.parse::<f64>().ok())
            else {
                stats.unparsed += 1;
                continue;
            };
            let detail = parts.next().unwrap_or("");
            match outcome {
                "hit" => {
                    stats.hits += 1;
                    stats.hit_millis += millis;
                }
                "remote" => {
                    stats.remote_hits += 1;
                    stats.hit_millis += millis;
                }
                "miss" => {
                    stats.misses += 1;
                    stats.miss_millis += millis;
                    if detail.starts_with("not stored") {
                        stats.unstored += 1;
                    }
                    misses.push(Miss {
                        crate_name: crate_name.to_owned(),
                        millis,
                    });
                }
                "pass" => stats.passes += 1,
                "error" => stats.errors += 1,
                _ => stats.unparsed += 1,
            }
        }
        misses.sort_by(|a, b| b.millis.total_cmp(&a.millis));
        misses.truncate(Self::SLOWEST);
        stats.slowest = misses;
        stats
    }

    /// Cacheable invocations: hits of either kind, misses, and errors.
    #[must_use]
    pub fn cacheable(&self) -> u64 {
        self.hits + self.remote_hits + self.misses + self.errors
    }

    /// The percentage of cacheable invocations served from a cache, or
    /// `None` when there were none.
    #[must_use]
    pub fn hit_rate(&self) -> Option<f64> {
        let cacheable = self.cacheable();
        // Counts of wrapper invocations stay far below 2^52.
        #[allow(clippy::cast_precision_loss)]
        (cacheable > 0).then(|| (self.hits + self.remote_hits) as f64 * 100.0 / cacheable as f64)
    }

    /// A Markdown summary: a table of outcomes and the slowest misses.
    #[must_use]
    pub fn markdown(&self) -> String {
        let mut text = format!("### tinymold\n\n{}\n\n", self.headline());
        text.push_str("| Outcome | Count | Wrapper time |\n|---|---:|---:|\n");
        let rows = [
            ("Local hits", self.hits, None),
            ("Remote hits", self.remote_hits, None),
            ("Hits, total", self.hits + self.remote_hits, Some(self.hit_millis)),
            ("Misses (compiled)", self.misses, Some(self.miss_millis)),
            ("Not cacheable", self.passes, None),
            ("Cache errors", self.errors, None),
        ];
        for (name, count, millis) in rows {
            let time = millis.map_or_else(String::new, seconds);
            let _ = writeln!(text, "| {name} | {count} | {time} |");
        }
        if !self.slowest.is_empty() {
            text.push_str("\nSlowest misses: ");
            let names: Vec<String> = self
                .slowest
                .iter()
                .map(|miss| format!("`{}` {}", miss.crate_name, seconds(miss.millis)))
                .collect();
            text.push_str(&names.join(", "));
            text.push('\n');
        }
        text
    }

    fn headline(&self) -> String {
        match self.hit_rate() {
            Some(rate) => format!(
                "{rate:.1}% hit rate: {} of {} cacheable compiles served from cache",
                self.hits + self.remote_hits,
                self.cacheable()
            ),
            None => "no cacheable compiles".to_owned(),
        }
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.headline())?;
        writeln!(
            f,
            "hits {} (remote {}), misses {} ({} not stored), not cacheable {}, errors {}",
            self.hits + self.remote_hits,
            self.remote_hits,
            self.misses,
            self.unstored,
            self.passes,
            self.errors
        )?;
        write!(
            f,
            "time on hits {}, on misses {}",
            seconds(self.hit_millis),
            seconds(self.miss_millis)
        )
    }
}

fn seconds(millis: f64) -> String {
    format!("{:.2}s", millis / 1000.0)
}

#[cfg(test)]
mod test;
