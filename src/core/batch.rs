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

pub(crate) fn cpu_core_count() -> usize {
    PROCESS_CORES.cores()
}

/// Cores that no [`share_cpu_work`] caller or helper is using right now.
/// Other work on the shared codec pool, such as CPU decode or region
/// composition, is not counted, so the pool may still be busy.
pub(crate) fn idle_cores() -> usize {
    if let Some(cores) = crate::core::test_hooks::idle_cores_override() {
        return cores;
    }
    PROCESS_CORES.idle()
}

/// Counts one thread in its [`CoreLedger`] until dropped.
struct ActiveWorker(&'static CoreLedger);

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs CPU codec work on the process-wide WSI pool.
///
/// An invoking Rayon worker is reused instead of entering another registry;
/// calls from ordinary threads share the one process-wide pool.
pub(crate) fn install_codec_cpu<R: Send>(operation: impl FnOnce() -> R + Send) -> R {
    if rayon::current_thread_index().is_some() {
        run_codec_work_on_worker(operation)
    } else if let Some(pool) = process_codec_cpu_pool() {
        crate::core::execution_telemetry::record(
            crate::core::execution_telemetry::Event::CpuPoolDispatches,
            1,
        );
        pool.install(operation)
    } else {
        operation()
    }
}

/// Worker count available to CPU codec work started from the calling thread.
pub(crate) fn cpu_worker_count() -> usize {
    if rayon::current_thread_index().is_some() {
        rayon::current_num_threads()
    } else {
        process_codec_cpu_pool().map_or(1, ThreadPool::current_num_threads)
    }
}

/// Run CPU codec work invoked on a Rayon worker.
///
/// The work stays on the invoking worker unless that worker belongs to another
/// pool and holds slide admission. There, idle workers of that pool can steal
/// the work's nested codec jobs, start unrelated reads while waiting on their
/// own nested jobs, and block on the same admission with the stolen job still
/// on their stack: a cycle no budget release can break. Such work moves to the
/// WSI pool, whose workers never wait for admission. A scoped thread hands it
/// over, because `install` from a foreign worker steals from the caller's pool
/// while it waits, and a joined thread does not.
pub(crate) fn run_codec_work_on_worker<R: Send>(operation: impl FnOnce() -> R + Send) -> R {
    match process_codec_cpu_pool() {
        Some(pool)
            if pool.current_thread_index().is_none()
                && crate::core::limits::current_thread_holds_admission() =>
        {
            crate::core::execution_telemetry::record(
                crate::core::execution_telemetry::Event::CpuPoolDispatches,
                1,
            );
            std::thread::scope(|scope| {
                scope
                    .spawn(|| pool.install(operation))
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
        }
        _ => operation(),
    }
}

pub(crate) fn process_codec_cpu_pool() -> Option<&'static ThreadPool> {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        // Rayon's default sizing honors RAYON_NUM_THREADS; leaving it unset
        // also avoids a second wsi-rs-specific concurrency control.
        rayon::ThreadPoolBuilder::new()
            .thread_name(|index| format!("wsi-rs-jp2k-cpu-{index}"))
            .build()
            .map(Some)
            .unwrap_or_else(|error| {
                tracing::error!(
                    %error,
                    "failed to initialize process-wide JP2K CPU pool; decoding inline"
                );
                None
            })
    })
    .as_ref()
}

/// Runs `jobs` on the calling thread in order while otherwise idle cores help.
///
/// Helpers go to the process worker pool only for cores that no other shared
/// work is using. The caller checks for idle cores before each of its jobs, so
/// cores that other reads release mid-batch join it too. Every thread claims
/// the next unstarted job, so the caller never waits for a helper that has not
/// started: it waits only for jobs a running helper already claimed. Results
/// keep job order, and a panic in any job resumes on the caller. Helpers retire
/// between jobs when new callers occupy their previously idle cores.
pub(crate) fn share_cpu_work<J, R>(jobs: Vec<J>, work: fn(&J) -> R) -> Vec<R>
where
    J: Send + Sync + 'static,
    R: Send + 'static,
{
    let pool = if jobs.len() > 1 {
        process_codec_cpu_pool()
    } else {
        None
    };
    share_cpu_work_on(&PROCESS_CORES, pool, jobs, work)
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
        jobs: jobs.into_iter().map(|job| Mutex::new(Some(job))).collect(),
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
    /// Each job moves out of its slot when claimed, so helpers that start
    /// after every job is claimed hold no job data.
    jobs: Box<[Mutex<Option<J>>]>,
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
                while ledger.active.load(Ordering::Relaxed) <= ledger.cores() {
                    if !shared.run_next() {
                        break;
                    }
                }
                shared.helpers.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }
}

impl<J, R> SharedWork<J, R> {
    /// Runs the next unstarted job, or returns `false` once all are claimed.
    fn run_next(&self) -> bool {
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        let Some(slot) = self.jobs.get(index) else {
            return false;
        };
        let job = slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("each job index is claimed once");
        let result = catch_unwind(AssertUnwindSafe(|| (self.work)(&job)));
        drop(job);
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
