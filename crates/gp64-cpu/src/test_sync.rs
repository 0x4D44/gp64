use std::sync::{Mutex, MutexGuard};

static SERIAL_TEST_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn serial_guard() -> MutexGuard<'static, ()> {
    SERIAL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::serial_guard;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Barrier,
    };
    use std::thread;
    use std::time::Duration;

    #[test]
    fn serial_guard_excludes_concurrent_tests() {
        const WORKERS: usize = 6;

        let start = Arc::new(Barrier::new(WORKERS + 1));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut workers = Vec::with_capacity(WORKERS);
        let mut release_txs = Vec::with_capacity(WORKERS);

        for _ in 0..WORKERS {
            let (release_tx, release_rx) = mpsc::channel();
            release_txs.push(release_tx);

            let start = Arc::clone(&start);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            let entered_tx = entered_tx.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                let _guard = serial_guard();
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(current, Ordering::SeqCst);
                entered_tx.send(()).expect("test receiver remains open");
                release_rx.recv().expect("test releases every worker");
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        start.wait();
        let first = entered_rx.recv_timeout(Duration::from_secs(10));
        let second = if first.is_ok() {
            entered_rx.recv_timeout(Duration::from_millis(100))
        } else {
            Err(mpsc::RecvTimeoutError::Disconnected)
        };

        for release_tx in release_txs {
            let _ = release_tx.send(());
        }
        let workers_joined = workers.into_iter().all(|worker| worker.join().is_ok());

        assert!(first.is_ok(), "no worker acquired the serial guard");
        assert!(
            matches!(second, Err(mpsc::RecvTimeoutError::Timeout)),
            "more than one worker entered the guarded section before release"
        );
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert!(workers_joined, "all test workers should finish");
    }
}
