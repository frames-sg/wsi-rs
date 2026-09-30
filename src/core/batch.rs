use crate::error::WsiError;
use rayon::ThreadPool;
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

pub(crate) fn expect_exact_count<T>(
    values: Vec<T>,
    expected: usize,
    context: &'static str,
) -> Result<Vec<T>, WsiError> {
    if values.len() != expected {
        return Err(WsiError::BackendContract {
            context,
            expected,
            actual: values.len(),
        });
    }
    Ok(values)
}

pub(crate) fn exactly_one<T>(values: Vec<T>, context: &'static str) -> Result<T, WsiError> {
    let mut values = expect_exact_count(values, 1, context)?;
    Ok(values.pop().expect("length checked above"))
}

/// Threads running [`share_cpu_work`], callers and committed helpers alike.
static ACTIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);

fn available_cores() -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| std::thread::available_parallelism().map_or(1, NonZeroUsize::get))
}

#[cfg(test)]
thread_local! {
    static IDLE_CORES_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Cores that no [`share_cpu_work`] caller or helper is using right now.
pub(crate) fn idle_cores() -> usize {
    #[cfg(test)]
    if let Some(cores) = IDLE_CORES_OVERRIDE.with(std::cell::Cell::get) {
        return cores;
    }
    available_cores().saturating_sub(ACTIVE_WORKERS.load(Ordering::Relaxed))
}

/// Runs `f` with [`idle_cores`] fixed on this thread, so batch planning is
/// deterministic regardless of other tests' work.
#[cfg(test)]
pub(crate) fn with_idle_cores<T>(cores: usize, f: impl FnOnce() -> T) -> T {
    let previous = IDLE_CORES_OVERRIDE.with(|cell| cell.replace(Some(cores)));
    let result = f();
    IDLE_CORES_OVERRIDE.with(|cell| cell.set(previous));
    result
}

/// Counts one thread in [`ACTIVE_WORKERS`] until dropped.
struct ActiveWorker;

impl ActiveWorker {
    fn enter() -> Self {
        ACTIVE_WORKERS.fetch_add(1, Ordering::Relaxed);
        Self
    }

    /// Commits up to `wanted` idle cores to helpers that have not started.
    fn reserve_idle(wanted: usize) -> usize {
        let mut active = ACTIVE_WORKERS.load(Ordering::Relaxed);
        loop {
            let granted = available_cores().saturating_sub(active).min(wanted);
            if granted == 0 {
                return 0;
            }
            match ACTIVE_WORKERS.compare_exchange_weak(
                active,
                active + granted,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return granted,
                Err(current) => active = current,
            }
        }
    }
}

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        ACTIVE_WORKERS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs `jobs` on the calling thread in order while otherwise idle cores help.
///
/// Helpers go to the process worker pool only for cores that no other shared
/// work is using. Every thread claims the next unstarted job, so the caller
/// never waits for a helper that has not started: it waits only for jobs a
/// running helper already claimed. Results keep job order, and a panic in any
/// job resumes on the caller.
pub(crate) fn share_cpu_work<J, R>(jobs: Vec<J>, work: fn(&J) -> R) -> Vec<R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    share_cpu_work_on(
        crate::core::decode_runtime::process_jp2k_cpu_pool(),
        jobs,
        work,
    )
}

fn share_cpu_work_on<J, R>(pool: Option<&ThreadPool>, jobs: Vec<J>, work: fn(&J) -> R) -> Vec<R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    let _caller = ActiveWorker::enter();
    if jobs.len() <= 1 {
        return jobs.iter().map(work).collect();
    }
    let shared = Arc::new(SharedWork {
        next: AtomicUsize::new(0),
        results: Mutex::new((0..jobs.len()).map(|_| None).collect()),
        completed: Condvar::new(),
        jobs,
        work,
    });
    if let Some(pool) = pool {
        for _ in 0..ActiveWorker::reserve_idle(shared.jobs.len() - 1) {
            let shared = Arc::clone(&shared);
            pool.spawn(move || {
                // Adopt the core `reserve_idle` committed to this helper.
                let _helper = ActiveWorker;
                shared.run();
            });
        }
    }
    shared.run();
    shared.finish()
}

struct SharedWork<J, R> {
    jobs: Vec<J>,
    work: fn(&J) -> R,
    next: AtomicUsize,
    results: Mutex<Vec<Option<std::thread::Result<R>>>>,
    completed: Condvar,
}

impl<J, R> SharedWork<J, R> {
    fn run(&self) {
        loop {
            let index = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(job) = self.jobs.get(index) else {
                return;
            };
            let result = catch_unwind(AssertUnwindSafe(|| (self.work)(job)));
            self.results.lock().unwrap_or_else(|e| e.into_inner())[index] = Some(result);
            self.completed.notify_all();
        }
    }

    /// Waits for jobs that running helpers claimed, then returns every result.
    fn finish(&self) -> Vec<R> {
        let mut results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        while results.iter().any(Option::is_none) {
            results = self
                .completed
                .wait(results)
                .unwrap_or_else(|e| e.into_inner());
        }
        std::mem::take(&mut *results)
            .into_iter()
            .map(|result| match result.expect("every job completed") {
                Ok(value) => value,
                Err(panic) => resume_unwind(panic),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
