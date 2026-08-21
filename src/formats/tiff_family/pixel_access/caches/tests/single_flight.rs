use super::*;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread;
use std::time::Duration;

fn tile(value: u8) -> Arc<CpuTile> {
    Arc::new(CpuTile {
        width: 1,
        height: 1,
        channels: 3,
        color_space: ColorSpace::Rgb,
        layout: CpuTileLayout::Interleaved,
        data: CpuTileData::u8(vec![value; 3]),
    })
}

#[test]
fn concurrent_misses_decode_once_and_share_the_arc() {
    let cache = Arc::new(SingleFlightTileCache::<u32>::new(1024));
    let decodes = Arc::new(AtomicUsize::new(0));
    let workers = (0..8)
        .map(|_| {
            let cache = cache.clone();
            let decodes = decodes.clone();
            thread::spawn(move || {
                cache
                    .get_or_try_insert_with(7, || {
                        decodes.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(20));
                        Ok(tile(7))
                    })
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();

    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(decodes.load(Ordering::SeqCst), 1);
    assert!(results
        .iter()
        .skip(1)
        .all(|result| Arc::ptr_eq(&results[0], result)));
}

#[test]
fn failed_decode_is_shared_with_waiters_but_not_cached() {
    let cache = Arc::new(SingleFlightTileCache::<u32>::new(1024));
    let decodes = Arc::new(AtomicUsize::new(0));
    let workers = (0..4)
        .map(|_| {
            let cache = cache.clone();
            let decodes = decodes.clone();
            thread::spawn(move || {
                cache.get_or_try_insert_with(9, || {
                    decodes.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    Err("decode failed".to_string())
                })
            })
        })
        .collect::<Vec<_>>();

    for worker in workers {
        assert_eq!(worker.join().unwrap().unwrap_err(), "decode failed");
    }
    assert_eq!(decodes.load(Ordering::SeqCst), 1);

    let recovered = cache
        .get_or_try_insert_with(9, || {
            decodes.fetch_add(1, Ordering::SeqCst);
            Ok(tile(9))
        })
        .unwrap();
    assert_eq!(recovered.as_u8(), Some([9, 9, 9].as_slice()));
    assert_eq!(decodes.load(Ordering::SeqCst), 2);
}

#[test]
fn panic_does_not_leave_the_key_permanently_in_flight() {
    let cache = SingleFlightTileCache::<u32>::new(1024);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = cache.get_or_try_insert_with(11, || -> Result<Arc<CpuTile>, String> {
            panic!("decoder panic")
        });
    }));
    assert!(panicked.is_err());

    assert!(cache.get_or_try_insert_with(11, || Ok(tile(11))).is_ok());
}

#[derive(Clone)]
struct SignalingKey {
    value: u32,
    first_hash: Arc<std::sync::atomic::AtomicBool>,
    hashed: Option<SyncSender<()>>,
}

impl SignalingKey {
    fn plain(value: u32) -> Self {
        Self {
            value,
            first_hash: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hashed: None,
        }
    }
}

impl PartialEq for SignalingKey {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for SignalingKey {}

impl Hash for SignalingKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
        if self.first_hash.swap(false, Ordering::SeqCst) {
            self.hashed
                .as_ref()
                .expect("signaling key has sender")
                .send(())
                .expect("hash observer remains live");
        }
    }
}

#[test]
fn cache_is_rechecked_after_an_initial_miss_waits_for_the_flight_lock() {
    let cache = Arc::new(SingleFlightTileCache::<SignalingKey>::new(1024));
    // Keep the underlying map non-empty so its missing-key lookup hashes the
    // signaling key before this test releases the flight lock.
    cache.put(SignalingKey::plain(999), tile(1));
    let flight_guard = cache
        .flights
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (hashed_tx, hashed_rx) = sync_channel(0);
    let key = SignalingKey {
        value: 17,
        first_hash: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        hashed: Some(hashed_tx),
    };
    let late_loads = Arc::new(AtomicUsize::new(0));
    let worker = {
        let cache = cache.clone();
        let late_loads = late_loads.clone();
        thread::spawn(move || {
            cache
                .get_or_try_insert_with(key, || {
                    late_loads.fetch_add(1, Ordering::SeqCst);
                    Ok(tile(99))
                })
                .expect("cache lookup")
        })
    };

    hashed_rx.recv().expect("initial cache lookup reached");
    let cached = tile(17);
    cache.put(SignalingKey::plain(17), cached.clone());
    drop(flight_guard);

    let result = worker.join().expect("worker");
    assert_eq!(late_loads.load(Ordering::SeqCst), 0);
    assert!(Arc::ptr_eq(&result, &cached));
}
