use alloy::primitives::BlockNumber;
use std::{marker::PhantomData, ops::RangeInclusive};
use tracing::debug;

pub struct Forward;
pub struct Reverse;

/// An iterator that yields block ranges of a configurable size.
#[derive(Debug, Clone)]
pub struct RangeIterator<D> {
    current: BlockNumber,
    end: BlockNumber,
    range_size: u64,
    batch_count: u64,
    total_batches: u64,
    _direction: PhantomData<D>,
}

impl RangeIterator<Forward> {
    /// Creates a forward iterator (oldest to newest).
    ///
    /// Yields ranges from `start` toward `end`, inclusive.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_range` is 0.
    #[must_use]
    pub const fn forward(start: BlockNumber, end: BlockNumber, max_block_range: u64) -> Self {
        assert!(max_block_range >= 1, "max_block_range must be at least 1");
        let total_batches = if start > end { 0 } else { (end - start) / max_block_range + 1 };
        Self {
            current: start,
            end,
            range_size: max_block_range,
            batch_count: 0,
            total_batches,
            _direction: PhantomData,
        }
    }

    /// Resets the iterator to continue from a new position.
    ///
    /// Useful after detecting a reorg to rescan from a common ancestor.
    pub fn reset_to(&mut self, block: BlockNumber) {
        self.current = block;
        self.total_batches = if block > self.end {
            self.batch_count
        } else {
            self.batch_count + (self.end - block) / self.range_size + 1
        };
    }
}

impl RangeIterator<Reverse> {
    /// Creates a reverse iterator (newest to oldest).
    ///
    /// Yields ranges from `start` (higher) toward `end` (lower), inclusive.
    /// Each yielded range is still formatted as `low..=high`.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_range` is 0.
    #[must_use]
    pub const fn reverse(start: BlockNumber, end: BlockNumber, max_block_range: u64) -> Self {
        assert!(max_block_range >= 1, "max_block_range must be at least 1");
        let total_batches = if start < end { 0 } else { (start - end) / max_block_range + 1 };
        Self {
            current: start,
            end,
            range_size: max_block_range,
            batch_count: 0,
            total_batches,
            _direction: PhantomData,
        }
    }
}

impl<D> RangeIterator<D> {
    /// Returns the number of batches yielded so far.
    #[must_use]
    pub fn batch_count(&self) -> u64 {
        self.batch_count
    }
}

impl Iterator for RangeIterator<Forward> {
    type Item = RangeInclusive<BlockNumber>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.batch_count >= self.total_batches {
            return None;
        }

        self.batch_count += 1;
        if self.batch_count % 10 == 0 {
            debug!(batch_count = self.batch_count, "Processed batches");
        }

        let batch_start = self.current;
        let batch_end = batch_start.saturating_add(self.range_size - 1).min(self.end);
        self.current = batch_end + 1;

        Some(batch_start..=batch_end)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining =
            usize::try_from(self.total_batches - self.batch_count).unwrap_or(usize::MAX);
        (remaining, None)
    }
}

impl Iterator for RangeIterator<Reverse> {
    type Item = RangeInclusive<BlockNumber>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.batch_count >= self.total_batches {
            return None;
        }

        self.batch_count += 1;
        if self.batch_count % 10 == 0 {
            debug!(batch_count = self.batch_count, "Processed batches");
        }

        let batch_high = self.current;
        let batch_low = batch_high.saturating_sub(self.range_size - 1).max(self.end);
        self.current = batch_low.saturating_sub(1);

        Some(batch_low..=batch_high)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match usize::try_from(self.total_batches - self.batch_count) {
            Ok(remaining) => (remaining, Some(remaining)),
            Err(_) => (usize::MAX, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_basic() {
        let mut iter = RangeIterator::forward(100, 250, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), Some(200..=249));
        assert_eq!(iter.next(), Some(250..=250));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_basic() {
        let mut iter = RangeIterator::reverse(250, 100, 50);
        assert_eq!(iter.next(), Some(201..=250));
        assert_eq!(iter.next(), Some(151..=200));
        assert_eq!(iter.next(), Some(101..=150));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_single_batch() {
        let mut iter = RangeIterator::forward(100, 120, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_single_batch() {
        let mut iter = RangeIterator::reverse(120, 100, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_exact_boundary() {
        let mut iter = RangeIterator::forward(100, 199, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_exact_boundary() {
        let mut iter = RangeIterator::reverse(199, 100, 50);
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_empty_range() {
        let mut iter = RangeIterator::forward(200, 100, 50);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_empty_range() {
        let mut iter = RangeIterator::reverse(100, 200, 50);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_single_block() {
        let mut iter = RangeIterator::forward(100, 100, 50);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_single_block() {
        let mut iter = RangeIterator::reverse(100, 100, 50);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_max_block_range_one() {
        let mut iter = RangeIterator::forward(100, 103, 1);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), Some(101..=101));
        assert_eq!(iter.next(), Some(102..=102));
        assert_eq!(iter.next(), Some(103..=103));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_max_block_range_one() {
        let mut iter = RangeIterator::reverse(103, 100, 1);
        assert_eq!(iter.next(), Some(103..=103));
        assert_eq!(iter.next(), Some(102..=102));
        assert_eq!(iter.next(), Some(101..=101));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_rewinds_forward_iteration() {
        let mut iter = RangeIterator::forward(100, 300, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));

        iter.reset_to(175);

        assert_eq!(iter.next(), Some(175..=224));
        assert_eq!(iter.next(), Some(225..=274));
        assert_eq!(iter.next(), Some(275..=300));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_after_exhausted() {
        let mut iter = RangeIterator::forward(100, 120, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);

        iter.reset_to(110);

        assert_eq!(iter.next(), Some(110..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_beyond_end_exhausts_forward() {
        let mut iter = RangeIterator::forward(100, 200, 50);
        assert_eq!(iter.next(), Some(100..=149));

        iter.reset_to(250);

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_starting_from_zero() {
        let mut iter = RangeIterator::forward(0, 100, 50);
        assert_eq!(iter.next(), Some(0..=49));
        assert_eq!(iter.next(), Some(50..=99));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_ending_at_zero() {
        let mut iter = RangeIterator::reverse(100, 0, 50);
        assert_eq!(iter.next(), Some(51..=100));
        assert_eq!(iter.next(), Some(1..=50));
        assert_eq!(iter.next(), Some(0..=0));
        assert_eq!(iter.next(), None);
    }

    #[test]
    #[should_panic(expected = "max_block_range must be at least 1")]
    fn forward_zero_max_block_range_panics() {
        let _ = RangeIterator::forward(100, 200, 0);
    }

    #[test]
    #[should_panic(expected = "max_block_range must be at least 1")]
    fn reverse_zero_max_block_range_panics() {
        let _ = RangeIterator::reverse(200, 100, 0);
    }

    #[test]
    fn batch_count_increments() {
        let mut iter = RangeIterator::forward(100, 300, 50);
        assert_eq!(iter.batch_count(), 0);

        iter.next();
        assert_eq!(iter.batch_count(), 1);

        iter.next();
        assert_eq!(iter.batch_count(), 2);

        iter.next();
        assert_eq!(iter.batch_count(), 3);
    }

    #[test]
    fn batch_count_persists_after_reset() {
        let mut iter = RangeIterator::forward(100, 300, 50);
        iter.next();
        iter.next();
        assert_eq!(iter.batch_count(), 2);

        iter.reset_to(150);

        // batch_count is not reset
        assert_eq!(iter.batch_count(), 2);

        iter.next();
        assert_eq!(iter.batch_count(), 3);
    }
}
