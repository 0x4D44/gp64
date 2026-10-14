//! A lock-free single-producer, single-consumer mono sample ring. Samples are
//! stored as `f32` bits in atomics, so neither side ever blocks or locks and
//! no unsafe code is needed. The split handles are not `Clone`, which keeps
//! exactly one producer and one consumer.
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    Arc,
};

struct Shared {
    slots: Box<[AtomicU32]>,
    /// Total samples ever read; only the consumer stores it.
    head: AtomicUsize,
    /// Total samples ever written; only the producer stores it.
    tail: AtomicUsize,
    /// Samples refused because the ring was full.
    dropped: AtomicU64,
    /// Set by the consumer once playback has started.
    started: AtomicBool,
}
impl Shared {
    fn len(&self) -> usize {
        // Load head first: it only grows towards tail, so this cannot underflow.
        let head = self.head.load(Ordering::Acquire);
        self.tail.load(Ordering::Acquire).wrapping_sub(head)
    }
}

/// Creates a ring holding up to `capacity` samples.
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    assert!(capacity > 0);
    let shared = Arc::new(Shared {
        slots: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        dropped: AtomicU64::new(0),
        started: AtomicBool::new(false),
    });
    (
        Producer {
            shared: shared.clone(),
        },
        Consumer { shared },
    )
}

pub struct Producer {
    shared: Arc<Shared>,
}
impl Producer {
    /// Appends samples in order. When the ring is full the newest samples are
    /// dropped and counted; queued audio is never overwritten.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.shared;
        let capacity = shared.slots.len();
        let tail = shared.tail.load(Ordering::Relaxed);
        let head = shared.head.load(Ordering::Acquire);
        let free = capacity - tail.wrapping_sub(head);
        let count = samples.len().min(free);
        for (offset, &sample) in samples[..count].iter().enumerate() {
            shared.slots[tail.wrapping_add(offset) % capacity]
                .store(sample.to_bits(), Ordering::Relaxed);
        }
        shared
            .tail
            .store(tail.wrapping_add(count), Ordering::Release);
        let refused = samples.len() - count;
        if refused > 0 {
            shared.dropped.fetch_add(refused as u64, Ordering::Relaxed);
        }
        count
    }
    /// Samples queued for the consumer.
    pub fn len(&self) -> usize {
        self.shared.len()
    }
    /// Total samples ever accepted.
    pub fn pushed(&self) -> u64 {
        self.shared.tail.load(Ordering::Relaxed) as u64
    }
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.shared.slots.len()
    }
    /// Whether the consumer has begun playing queued audio.
    pub fn started(&self) -> bool {
        self.shared.started.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

pub struct Consumer {
    shared: Arc<Shared>,
}
impl Consumer {
    pub fn pop(&mut self) -> Option<f32> {
        let shared = &*self.shared;
        let head = shared.head.load(Ordering::Relaxed);
        if shared.tail.load(Ordering::Acquire) == head {
            return None;
        }
        let bits = shared.slots[head % shared.slots.len()].load(Ordering::Relaxed);
        shared.head.store(head.wrapping_add(1), Ordering::Release);
        Some(f32::from_bits(bits))
    }
    /// Discards up to `count` of the oldest queued samples.
    pub fn skip(&mut self, count: usize) -> usize {
        let shared = &*self.shared;
        let head = shared.head.load(Ordering::Relaxed);
        let count = count.min(shared.tail.load(Ordering::Acquire).wrapping_sub(head));
        shared
            .head
            .store(head.wrapping_add(count), Ordering::Release);
        count
    }
    pub fn len(&self) -> usize {
        self.shared.len()
    }
    pub fn capacity(&self) -> usize {
        self.shared.slots.len()
    }
    pub fn mark_started(&self) {
        self.shared.started.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_ring_drops_newest_and_keeps_queued_order() {
        let (mut producer, mut consumer) = ring(4);
        assert_eq!(producer.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(producer.push(&[4.0, 5.0, 6.0]), 1);
        assert_eq!(producer.dropped(), 2);
        assert_eq!(producer.len(), 4);
        assert_eq!(producer.pushed(), 4);
        assert_eq!(consumer.pop(), Some(1.0));
        assert_eq!(producer.push(&[7.0]), 1);
        assert_eq!(consumer.skip(1), 1);
        assert_eq!(
            [
                consumer.pop(),
                consumer.pop(),
                consumer.pop(),
                consumer.pop()
            ],
            [Some(3.0), Some(4.0), Some(7.0), None]
        );
        assert_eq!(consumer.skip(9), 0);
        assert!(!producer.started());
        consumer.mark_started();
        assert!(producer.started());
    }

    #[test]
    fn two_threads_lose_and_reorder_nothing_when_not_full() {
        const TOTAL: u32 = 2_000_000;
        let (mut producer, mut consumer) = ring(257);
        let writer = std::thread::spawn(move || {
            let mut next = 0u32;
            while next < TOTAL {
                let room = producer.capacity() - producer.len();
                let count = room.min(13).min((TOTAL - next) as usize);
                let chunk: Vec<f32> = (next..next + count as u32).map(|n| n as f32).collect();
                // Only ever offer what fits, so nothing may be refused.
                assert_eq!(producer.push(&chunk), count);
                next += count as u32;
                if count == 0 {
                    std::thread::yield_now();
                }
            }
            producer.dropped()
        });
        let mut expected = 0u32;
        while expected < TOTAL {
            match consumer.pop() {
                // f32 holds every integer below 2^24 exactly.
                Some(sample) => {
                    assert_eq!(sample, expected as f32);
                    expected += 1;
                }
                None => std::thread::yield_now(),
            }
        }
        assert_eq!(writer.join().unwrap(), 0);
        assert_eq!(consumer.pop(), None);
    }
}
