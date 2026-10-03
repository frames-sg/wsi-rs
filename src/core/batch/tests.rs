use super::*;

impl CoreLedger {
    /// A private ledger, so tests control idle cores without other tests.
    fn with_cores(cores: usize) -> &'static Self {
        let ledger: &'static Self = Box::leak(Box::new(Self::new()));
        ledger
            .cores
            .set(cores)
            .expect("new ledger has no core count");
        ledger
    }
}

#[test]
fn exactly_one_accepts_one_and_rejects_other_cardinalities() {
    assert_eq!(exactly_one(vec![7], "test").expect("one item"), 7);
    for values in [Vec::<u8>::new(), vec![1, 2]] {
        assert!(matches!(
            exactly_one(values, "test"),
            Err(WsiError::BackendContract { .. })
        ));
    }
}

fn square(value: &u64) -> u64 {
    value * value
}

#[test]
fn shared_cpu_work_returns_results_in_job_order() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let ledger = CoreLedger::with_cores(4);
    let jobs: Vec<u64> = (0..64).collect();
    let expected: Vec<u64> = jobs.iter().map(square).collect();
    assert_eq!(
        share_cpu_work_on(ledger, Some(&pool), jobs, square),
        expected
    );
    assert_eq!(share_cpu_work_on(ledger, None, vec![7], square), [49]);
    assert!(share_cpu_work_on(ledger, None, Vec::<u64>::new(), square).is_empty());
}

fn current_thread(_: &u64) -> std::thread::ThreadId {
    std::thread::current().id()
}

#[test]
fn shared_cpu_work_never_waits_for_helpers_that_have_not_started() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let (started, is_blocked) = std::sync::mpsc::channel();
    pool.spawn(move || {
        started.send(()).unwrap();
        blocked.recv().unwrap();
    });
    is_blocked.recv().unwrap();
    // Every helper queues behind the blocked worker; the caller runs all jobs.
    let threads = share_cpu_work_on(
        CoreLedger::with_cores(4),
        Some(&pool),
        (0..8).collect(),
        current_thread,
    );
    assert!(threads.iter().all(|id| *id == std::thread::current().id()));
    release.send(()).unwrap();
}

#[derive(Default)]
struct LateCoreProbe {
    caller: std::sync::OnceLock<std::thread::ThreadId>,
    started: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    helper_ran: std::sync::atomic::AtomicBool,
}

/// Job 0 holds the caller until the test frees a core. Later caller jobs pace
/// themselves until a helper runs one.
fn run_until_a_late_helper_joins(job: &(usize, Arc<LateCoreProbe>)) -> std::thread::ThreadId {
    use std::sync::atomic::Ordering::SeqCst;
    let (index, probe) = job;
    let thread = std::thread::current().id();
    if *index == 0 {
        probe
            .caller
            .set(thread)
            .expect("the caller runs job 0 first");
        probe.started.store(true, SeqCst);
        while !probe.released.load(SeqCst) {
            std::thread::yield_now();
        }
    } else if probe.caller.get() != Some(&thread) {
        probe.helper_ran.store(true, SeqCst);
    } else if !probe.helper_ran.load(SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    thread
}

#[test]
fn shared_cpu_work_recruits_cores_that_become_idle_mid_batch() {
    use std::sync::atomic::Ordering::SeqCst;
    let ledger = CoreLedger::with_cores(2);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    // Another read holds the second core when the batch starts.
    let other_read = ledger.enter();
    let probe = Arc::new(LateCoreProbe::default());
    let jobs = (0..2_000)
        .map(|index| (index, Arc::clone(&probe)))
        .collect();
    let pool = &pool;
    let threads = std::thread::scope(|scope| {
        let batch = scope.spawn(move || {
            share_cpu_work_on(ledger, Some(pool), jobs, run_until_a_late_helper_joins)
        });
        while !probe.started.load(SeqCst) {
            std::thread::yield_now();
        }
        drop(other_read);
        probe.released.store(true, SeqCst);
        batch.join().unwrap()
    });
    assert!(
        threads[1..].iter().any(|thread| *thread != threads[0]),
        "a core released mid-batch must join it"
    );
    // A helper releases its core just after it finds no job left.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while ledger.active.load(SeqCst) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "every helper releases its core"
        );
        std::thread::yield_now();
    }
}

fn panic_on_three(value: &u64) -> u64 {
    assert_ne!(*value, 3, "job three fails");
    *value
}

#[test]
fn shared_cpu_work_resumes_a_job_panic_on_the_caller() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let ledger = CoreLedger::with_cores(4);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        share_cpu_work_on(ledger, Some(&pool), (0..8).collect(), panic_on_three)
    }));
    assert!(result.is_err());
    // The pool and accounting remain usable after a propagated panic.
    assert_eq!(
        share_cpu_work_on(ledger, Some(&pool), vec![2, 3 + 1], square),
        [4, 16]
    );
}
