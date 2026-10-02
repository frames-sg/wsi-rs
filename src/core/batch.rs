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

/// Counts the threads running [`share_cpu_work`], callers and committed
/// helpers alike, against the cores they may occupy.
struct CoreLedger {
    cores: OnceLock<usize>,
    active: AtomicUsize,
}

impl CoreLedger {
    const fn new() -> Self {
        Self {
            cores: OnceLock::new(),
            active: AtomicUsize::new(0),
        }
    }

    /// A private ledger, so tests control idle cores without other tests.
    #[cfg(test)]
    fn with_cores(cores: usize) -> &'static Self {
        let ledger: &'static Self = Box::leak(Box::new(Self::new()));
        ledger
            .cores
            .set(cores)
            .expect("new ledger has no core count");
        ledger
    }

    fn cores(&self) -> usize {
        *self
            .cores
            .get_or_init(|| std::thread::available_parallelism().map_or(1, NonZeroUsize::get))
    }

    fn idle(&self) -> usize {
        self.cores()
            .saturating_sub(self.active.load(Ordering::Relaxed))
    }

    fn enter(&'static self) -> ActiveWorker {
        self.active.fetch_add(1, Ordering::Relaxed);
        ActiveWorker(self)
    }

    /// Commits up to `wanted` idle cores to helpers that have not started.
    fn reserve_idle(&self, wanted: usize) -> usize {
        let mut active = self.active.load(Ordering::Relaxed);
        loop {
            let granted = self.cores().saturating_sub(active).min(wanted);
            if granted == 0 {
                return 0;
            }
            match self.active.compare_exchange_weak(
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

static PROCESS_CORES: CoreLedger = CoreLedger::new();

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
    PROCESS_CORES.idle()
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

/// Counts one thread in its [`CoreLedger`] until dropped.
struct ActiveWorker(&'static CoreLedger);

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs `jobs` on the calling thread in order while otherwise idle cores help.
///
/// Helpers go to the process worker pool only for cores that no other shared
/// work is using. The caller checks for idle cores before each of its jobs, so
/// cores that other reads release mid-batch join it too. Every thread claims
/// the next unstarted job, so the caller never waits for a helper that has not
/// started: it waits only for jobs a running helper already claimed. Results
/// keep job order, and a panic in any job resumes on the caller.
pub(crate) fn share_cpu_work<J, R>(jobs: Vec<J>, work: fn(&J) -> R) -> Vec<R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    share_cpu_work_on(
        &PROCESS_CORES,
        crate::core::decode_runtime::process_jp2k_cpu_pool(),
        jobs,
        work,
    )
}

fn share_cpu_work_on<J, R>(
    ledger: &'static CoreLedger,
    pool: Option<&ThreadPool>,
    jobs: Vec<J>,
    work: fn(&J) -> R,
) -> Vec<R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    let _caller = ledger.enter();
    if jobs.len() <= 1 {
        return jobs.iter().map(work).collect();
    }
    let shared = Arc::new(SharedWork {
        next: AtomicUsize::new(0),
        helpers: AtomicUsize::new(0),
        results: Mutex::new((0..jobs.len()).map(|_| None).collect()),
        completed: Condvar::new(),
        jobs,
        work,
    });
    loop {
        if let Some(pool) = pool {
            shared.recruit(ledger, pool);
        }
        if !shared.run_next() {
            break;
        }
    }
    shared.finish()
}

struct SharedWork<J, R> {
    jobs: Vec<J>,
    work: fn(&J) -> R,
    next: AtomicUsize,
    /// Helpers committed to this work that have not finished.
    helpers: AtomicUsize,
    results: Mutex<Vec<Option<std::thread::Result<R>>>>,
    completed: Condvar,
}

impl<J, R> SharedWork<J, R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    /// Spawns a helper for each idle core while unstarted jobs, beyond the one
    /// the caller claims next, outnumber the helpers already committed.
    fn recruit(self: &Arc<Self>, ledger: &'static CoreLedger, pool: &ThreadPool) {
        let unstarted = self
            .jobs
            .len()
            .saturating_sub(self.next.load(Ordering::Relaxed));
        let wanted = unstarted
            .saturating_sub(1)
            .saturating_sub(self.helpers.load(Ordering::Relaxed));
        if wanted == 0 {
            return;
        }
        let granted = ledger.reserve_idle(wanted);
        self.helpers.fetch_add(granted, Ordering::Relaxed);
        for _ in 0..granted {
            let shared = Arc::clone(self);
            pool.spawn(move || {
                // Adopt the core `reserve_idle` committed to this helper.
                let _helper = ActiveWorker(ledger);
                while shared.run_next() {}
                shared.helpers.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }
}

impl<J, R> SharedWork<J, R> {
    /// Runs the next unstarted job, or returns `false` once all are claimed.
    fn run_next(&self) -> bool {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        let Some(job) = self.jobs.get(index) else {
            return false;
        };
        let result = catch_unwind(AssertUnwindSafe(|| (self.work)(job)));
        self.results.lock().unwrap_or_else(|e| e.into_inner())[index] = Some(result);
        self.completed.notify_all();
        true
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
