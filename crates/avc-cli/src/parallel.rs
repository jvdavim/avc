//! Running the same job over many files at once.
//!
//! Hashing is what `add`, `status`, and `verify` spend their time on, and it is
//! CPU-bound: a modern disk delivers bytes faster than one core can digest
//! them. A directory of a thousand files hashed one at a time therefore leaves
//! most of the machine idle.
//!
//! Deliberately small. There is no thread pool, no work stealing, and no
//! dependency — a scoped spawn per worker and an atomic cursor into the input
//! is the whole design, because the unit of work here is "hash a file", which
//! is coarse enough that nothing finer would pay for itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::Failure;

/// Upper bound on workers.
///
/// Past a handful of concurrent readers a spinning disk spends its time
/// seeking, and the ceiling costs nothing on a machine that could have used
/// more: hashing saturates well before it.
const MAX_WORKERS: usize = 8;

/// Apply `job` to every item, in parallel, returning results in input order.
///
/// The first error wins and the rest are discarded — a run that cannot hash one
/// file is going to stop anyway, and reporting the first failure encountered is
/// what a sequential loop would have done.
pub(crate) fn map<T, R, F>(items: &[T], job: F) -> Result<Vec<R>, Failure>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Result<R, Failure> + Sync,
{
    // One item is the common case for `avc add <file>`, and spawning a thread
    // to do nothing in parallel is pure overhead.
    if items.len() < 2 {
        return items.iter().map(&job).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(MAX_WORKERS)
        .min(items.len());
    if workers < 2 {
        return items.iter().map(&job).collect();
    }

    let cursor = AtomicUsize::new(0);
    // Results are placed by index rather than pushed, so the output order is
    // the input order however the work happens to interleave.
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let failure: Mutex<Option<Failure>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                // Stop handing out work once something has failed; the error
                // is reported, not the remaining files.
                if failure.lock().expect("worker panicked").is_some() {
                    return;
                }
                let index = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(index) else {
                    return;
                };
                match job(item) {
                    Ok(value) => {
                        results.lock().expect("worker panicked")[index] = Some(value);
                    }
                    Err(error) => {
                        let mut first = failure.lock().expect("worker panicked");
                        if first.is_none() {
                            *first = Some(error);
                        }
                        return;
                    }
                }
            });
        }
    });

    if let Some(error) = failure.into_inner().expect("worker panicked") {
        return Err(error);
    }
    Ok(results
        .into_inner()
        .expect("worker panicked")
        .into_iter()
        .map(|value| value.expect("every index is filled when no worker failed"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_in_input_order() {
        let items: Vec<usize> = (0..1000).collect();
        let doubled = map(&items, |value| Ok(value * 2)).unwrap();
        assert_eq!(doubled, items.iter().map(|value| value * 2).collect::<Vec<_>>());
    }

    #[test]
    fn one_item_and_none_are_both_fine() {
        assert_eq!(map(&[7_usize], |value| Ok(*value)).unwrap(), vec![7]);
        assert!(map(&[] as &[usize], |value| Ok(*value)).unwrap().is_empty());
    }

    #[test]
    fn a_failure_is_reported_rather_than_swallowed() {
        let items: Vec<usize> = (0..500).collect();
        let error = map(&items, |value| {
            if *value == 400 {
                Err(Failure::from("cannot hash 400"))
            } else {
                Ok(*value)
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "cannot hash 400");
    }
}
