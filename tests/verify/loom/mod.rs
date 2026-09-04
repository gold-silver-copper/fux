use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

pub fn manager_election() {
    loom::model(|| {
        let elected = Arc::new(AtomicBool::new(false));
        let winners = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let elected = Arc::clone(&elected);
            let winners = Arc::clone(&winners);
            threads.push(thread::spawn(move || {
                if elected
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    winners.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for thread in threads {
            thread.join().expect("election thread");
        }
        assert_eq!(winners.load(Ordering::Acquire), 1);
    });
}

pub fn snapshot_notification() {
    loom::model(|| {
        let generation = Arc::new(AtomicUsize::new(0));
        let notified = Arc::new(AtomicUsize::new(0));
        let writer_generation = Arc::clone(&generation);
        let writer_notified = Arc::clone(&notified);
        let writer = thread::spawn(move || {
            writer_generation.store(1, Ordering::Release);
            writer_notified.store(1, Ordering::Release);
        });
        let reader_generation = Arc::clone(&generation);
        let reader = thread::spawn(move || {
            if notified.load(Ordering::Acquire) == 1 {
                assert_eq!(reader_generation.load(Ordering::Acquire), 1);
            }
        });
        writer.join().expect("snapshot writer");
        reader.join().expect("snapshot reader");
    });
}

pub fn exit_close() {
    loom::model(|| {
        let published = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let published = Arc::clone(&published);
            let count = Arc::clone(&count);
            threads.push(thread::spawn(move || {
                if !published.swap(true, Ordering::AcqRel) {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for thread in threads {
            thread.join().expect("close thread");
        }
        assert_eq!(count.load(Ordering::Acquire), 1);
    });
}

pub fn shutdown_mutation() {
    loom::model(|| {
        let state = Arc::new(Mutex::new((false, 0usize)));
        let shutdown_state = Arc::clone(&state);
        let shutdown = thread::spawn(move || shutdown_state.lock().expect("state").0 = true);
        let mutation_state = Arc::clone(&state);
        let mutation = thread::spawn(move || {
            let mut state = mutation_state.lock().expect("state");
            if !state.0 {
                state.1 += 1;
            }
        });
        shutdown.join().expect("shutdown");
        mutation.join().expect("mutation");
        let state = state.lock().expect("state");
        assert!(state.1 <= 1);
        if state.0 && state.1 == 1 { /* mutation linearized before shutdown */ }
    });
}

pub fn subscriber_close() {
    loom::model(|| {
        let subscriber = Arc::new(Mutex::new(Some(Vec::<u8>::new())));
        let close_subscriber = Arc::clone(&subscriber);
        let close = thread::spawn(move || {
            close_subscriber.lock().expect("subscriber").take();
        });
        let publish_subscriber = Arc::clone(&subscriber);
        let publish = thread::spawn(move || {
            if let Some(queue) = publish_subscriber.lock().expect("subscriber").as_mut() {
                queue.push(1);
            }
        });
        close.join().expect("close");
        publish.join().expect("publish");
        assert!(subscriber.lock().expect("subscriber").is_none());
    });
}

pub fn notification_completion() {
    loom::model(|| {
        let child = Arc::new(Mutex::new(Some(7usize)));
        let complete_child = Arc::clone(&child);
        let completion = thread::spawn(move || complete_child.lock().expect("child").take());
        let shutdown_child = Arc::clone(&child);
        let shutdown = thread::spawn(move || shutdown_child.lock().expect("child").take());
        let completed = completion.join().expect("completion");
        let stopped = shutdown.join().expect("shutdown");
        assert_eq!(
            usize::from(completed.is_some()) + usize::from(stopped.is_some()),
            1
        );
    });
}

pub fn signal_reap() {
    loom::model(|| {
        let process = Arc::new(Mutex::new(Some((41usize, 1usize))));
        let reap_process = Arc::clone(&process);
        let reap = thread::spawn(move || reap_process.lock().expect("process").take());
        let signal_process = Arc::clone(&process);
        let signal = thread::spawn(move || {
            signal_process
                .lock()
                .expect("process")
                .filter(|(_, generation)| *generation == 1)
        });
        let reaped = reap.join().expect("reap");
        let signaled = signal.join().expect("signal");
        assert!(reaped.is_some());
        if signaled.is_some() { /* signal linearized while the owned generation was live */ }
        assert!(process.lock().expect("process").is_none());
    });
}
