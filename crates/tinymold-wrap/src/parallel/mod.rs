//! Running per-item work on scoped worker threads.

use crate::Error;

/// How many threads [`map`] spreads its items over.
pub(crate) fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
}

/// Applies `work` to every item, on up to [`workers`] scoped threads.
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
    let mut results = Vec::with_capacity(items.len());
    std::thread::scope(|scope| -> std::result::Result<(), E> {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|batch| scope.spawn(move || batch.iter().map(work).collect::<std::result::Result<Vec<R>, E>>()))
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(batch) => results.extend(batch?),
                Err(_) => return Err(Error::WorkerPanicked { task }.into()),
            }
        }
        Ok(())
    })?;
    Ok(results)
}

#[cfg(test)]
mod test;
