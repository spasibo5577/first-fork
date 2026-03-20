//! Generic bounded ring buffer for event histories.
//!
//! One implementation for recovery events, backup results, disk samples,
//! remediation log, and restart tracking.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Bounded FIFO buffer that evicts the oldest entry when full.
///
/// Not thread-safe — owned by the control loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RingBuf<T> {
    #[serde(flatten)]
    items: VecDeque<T>,
    #[serde(skip)]
    max_size: usize,
}

impl<T> RingBuf<T> {
    /// Creates an empty ring buffer with the given maximum capacity.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        assert!(max_size > 0, "RingBuf max_size must be > 0");
        Self {
            items: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Pushes an item, evicting the oldest if at capacity.
    pub fn push(&mut self, item: T) {
        if self.items.len() == self.max_size {
            self.items.pop_front();
        }
        self.items.push_back(item);
    }

    /// Iterator over all items, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Copies all items to a `Vec`, oldest first.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.items.iter().cloned().collect()
    }
}

/// Collection utility methods — standard API, used in tests,
/// consumed by Phase 4 runtime (history endpoints, diagnostics).
#[allow(dead_code)]
impl<T> RingBuf<T> {
    /// Number of items currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Maximum capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.max_size
    }

    /// Last N items as references, most recent last.
    #[must_use]
    pub fn last_n(&self, n: usize) -> Vec<&T> {
        let skip = self.items.len().saturating_sub(n);
        self.items.iter().skip(skip).collect()
    }

    /// Clears all items.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Sets max size after deserialization. Truncates if needed.
    pub fn set_max_size(&mut self, max_size: usize) {
        self.max_size = max_size;
        while self.items.len() > max_size {
            self.items.pop_front();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn push_within_capacity() {
        let mut buf = RingBuf::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        assert_eq!(buf.to_vec(), vec![1, 2, 3]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn push_evicts_oldest() {
        let mut buf = RingBuf::new(3);
        buf.push(1);
        buf.push(2);
        buf.push(3);
        buf.push(4);
        assert_eq!(buf.to_vec(), vec![2, 3, 4]);
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn push_evicts_multiple() {
        let mut buf = RingBuf::new(2);
        for i in 0..10 {
            buf.push(i);
        }
        assert_eq!(buf.to_vec(), vec![8, 9]);
    }

    #[test]
    fn last_n_returns_tail() {
        let mut buf = RingBuf::new(5);
        for i in 0..5 {
            buf.push(i);
        }
        let tail: Vec<&i32> = buf.last_n(3);
        assert_eq!(tail, vec![&2, &3, &4]);
    }

    #[test]
    fn last_n_larger_than_len() {
        let mut buf = RingBuf::new(5);
        buf.push(1);
        buf.push(2);
        let tail: Vec<&i32> = buf.last_n(10);
        assert_eq!(tail, vec![&1, &2]);
    }

    #[test]
    fn empty_buffer() {
        let buf: RingBuf<i32> = RingBuf::new(5);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.to_vec(), Vec::<i32>::new());
    }

    #[test]
    fn clear_works() {
        let mut buf = RingBuf::new(5);
        buf.push(1);
        buf.push(2);
        buf.clear();
        assert!(buf.is_empty());
    }

    #[test]
    fn set_max_size_truncates() {
        let mut buf = RingBuf::new(5);
        for i in 0..5 {
            buf.push(i);
        }
        buf.set_max_size(3);
        assert_eq!(buf.to_vec(), vec![2, 3, 4]);
        assert_eq!(buf.capacity(), 3);
    }

    #[test]
    #[should_panic(expected = "max_size must be > 0")]
    fn zero_capacity_panics() {
        let _buf: RingBuf<i32> = RingBuf::new(0);
    }

    #[test]
    fn serde_roundtrip() {
        let mut buf = RingBuf::new(3);
        buf.push(10);
        buf.push(20);
        buf.push(30);

        let json = serde_json::to_string(&buf).expect("serialize");
        let mut loaded: RingBuf<i32> = serde_json::from_str(&json).expect("deserialize");
        loaded.set_max_size(3);

        assert_eq!(loaded.to_vec(), vec![10, 20, 30]);
    }
}