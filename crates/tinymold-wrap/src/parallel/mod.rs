//! Running per-item work on scoped worker threads.
//!
//! Both sides of the unit cache handle every output of a compile, and an
//! incremental compile with split debuginfo has hundreds. [`map`] splits such
//! a list into one contiguous batch per worker, runs each batch on a scoped
//! thread, and gives back the results in item order. A worker that panics
//! becomes an [`Error::WorkerPanicked`], never a panic of the caller: the
//! wrapper must always be able to fall back to running rustc.

use std::thread::ScopedJoinHandle;

use crate::Error;

/// How many threads [`map`] spreads its items over: the machine's
/// parallelism, capped at 8, beyond which the filesystem, not the CPU, is
/// the bottleneck.
pub(crate) fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// Applies `work` to every item, on up to [`workers`] scoped threads, and
/// returns the results in item order.
///
/// Every worker is joined before this returns, whatever the others did.
///
/// # Errors
///
/// The error of the first failing item, by position, or
/// [`Error::WorkerPanicked`] naming `task` when a worker panicked first.
pub(crate) fn map<T, R, E>(
    items: &[T],
    task: &'static str,
    work: impl Fn(&T) -> std::result::Result<R, E> + Sync,
) -> std::result::Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: Send + From<Error>,
{
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let chunk = items.len().div_ceil(workers());
    let work = &work;
    let batches: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<ScopedJoinHandle<'_, std::result::Result<Vec<R>, E>>> = items
            .chunks(chunk)
            .map(|batch| scope.spawn(move || batch.iter().map(work).collect()))
            .collect();
        // Join them all before looking at any: a scope that ends with an
        // unjoined panicked thread panics itself.
        handles.into_iter().map(ScopedJoinHandle::join).collect()
    });
    let mut results = Vec::with_capacity(items.len());
    for batch in batches {
        match batch {
            Ok(batch) => results.extend(batch?),
            Err(_) => return Err(Error::WorkerPanicked { task }.into()),
        }
    }
    Ok(results)
}

#[cfg(test)]
mod test;
