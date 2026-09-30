use super::*;

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
    let jobs: Vec<u64> = (0..64).collect();
    let expected: Vec<u64> = jobs.iter().map(square).collect();
    assert_eq!(share_cpu_work_on(Some(&pool), jobs, square), expected);
    assert_eq!(share_cpu_work_on(None, vec![7], square), [49]);
    assert!(share_cpu_work_on(None, Vec::<u64>::new(), square).is_empty());
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
    let threads = share_cpu_work_on(Some(&pool), (0..8).collect(), current_thread);
    assert!(threads.iter().all(|id| *id == std::thread::current().id()));
    release.send(()).unwrap();
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        share_cpu_work_on(Some(&pool), (0..8).collect(), panic_on_three)
    }));
    assert!(result.is_err());
    // The pool and accounting remain usable after a propagated panic.
    assert_eq!(
        share_cpu_work_on(Some(&pool), vec![2, 3 + 1], square),
        [4, 16]
    );
}
