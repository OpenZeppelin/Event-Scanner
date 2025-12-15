use alloy::primitives::BlockNumber;
use std::ops::RangeInclusive;
use tracing::debug;

/// Direction of block range iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Iterate from lower block numbers to higher (oldest to newest).
    Forward,
    /// Iterate from higher block numbers to lower (newest to oldest).
    Reverse,
}

/// An iterator that yields block ranges in batches of a configurable size.
#[derive(Debug, Clone)]
pub struct BatchIterator {
    /// Current position in the iteration.
    current: BlockNumber,
    /// The boundary we're iterating toward.
    end: BlockNumber,
    /// Maximum blocks per batch.
    max_block_range: u64,
    /// Direction of iteration.
    direction: Direction,
    /// Whether iteration has completed.
    exhausted: bool,
    /// Number of batches yielded so far.
    batch_count: u64,
}

impl BatchIterator {
    /// Creates a forward iterator (oldest to newest).
    ///
    /// Yields ranges from `start` toward `end`, inclusive.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_range` is 0.
    #[must_use]
    pub fn forward(start: BlockNumber, end: BlockNumber, max_block_range: u64) -> Self {
        assert!(max_block_range >= 1, "max_block_range must be at least 1");
        Self {
            current: start,
            end,
            max_block_range,
            direction: Direction::Forward,
            exhausted: start > end,
            batch_count: 0,
        }
    }

    /// Creates a reverse iterator (newest to oldest).
    ///
    /// Yields ranges from `start` (higher) toward `end` (lower), inclusive.
    /// Each yielded range is still formatted as `low..=high`.
    ///
    /// # Panics
    ///
    /// Panics if `max_block_range` is 0.
    #[must_use]
    pub fn reverse(start: BlockNumber, end: BlockNumber, max_block_range: u64) -> Self {
        assert!(max_block_range >= 1, "max_block_range must be at least 1");
        Self {
            current: start,
            end,
            max_block_range,
            direction: Direction::Reverse,
            exhausted: start < end,
            batch_count: 0,
        }
    }

    /// Returns the number of batches yielded so far.
    #[must_use]
    #[allow(dead_code)]
    pub fn batch_count(&self) -> u64 {
        self.batch_count
    }

    /// Resets the iterator to continue from a new position.
    ///
    /// Useful after detecting a reorg to rescan from a common ancestor.
    pub fn reset_to(&mut self, block: BlockNumber) {
        self.current = block;
        self.exhausted = match self.direction {
            Direction::Forward => self.current > self.end,
            Direction::Reverse => self.current < self.end,
        };
    }

    /// Returns the current position.
    #[must_use]
    #[allow(dead_code)]
    pub fn current(&self) -> BlockNumber {
        self.current
    }

    /// Returns the end boundary.
    #[must_use]
    #[allow(dead_code)]
    pub fn end(&self) -> BlockNumber {
        self.end
    }

    /// Returns the direction of iteration.
    #[must_use]
    #[allow(dead_code)]
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Returns whether iteration has completed.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}

impl Iterator for BatchIterator {
    type Item = RangeInclusive<BlockNumber>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted {
            return None;
        }

        self.batch_count += 1;
        if self.batch_count % 10 == 0 {
            debug!(batch_count = self.batch_count, "Processed batches");
        }

        match self.direction {
            Direction::Forward => {
                let batch_start = self.current;
                let batch_end = batch_start.saturating_add(self.max_block_range - 1).min(self.end);

                if batch_end >= self.end {
                    self.exhausted = true;
                } else {
                    self.current = batch_end + 1;
                }

                Some(batch_start..=batch_end)
            }
            Direction::Reverse => {
                let batch_high = self.current;
                let batch_low = batch_high.saturating_sub(self.max_block_range - 1).max(self.end);

                if batch_low <= self.end {
                    self.exhausted = true;
                } else {
                    self.current = batch_low - 1;
                }

                // Always return range as low..=high for consistency
                Some(batch_low..=batch_high)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_basic() {
        let mut iter = BatchIterator::forward(100, 250, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), Some(200..=249));
        assert_eq!(iter.next(), Some(250..=250));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_basic() {
        let mut iter = BatchIterator::reverse(250, 100, 50);
        assert_eq!(iter.next(), Some(201..=250));
        assert_eq!(iter.next(), Some(151..=200));
        assert_eq!(iter.next(), Some(101..=150));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_single_batch() {
        let mut iter = BatchIterator::forward(100, 120, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_single_batch() {
        let mut iter = BatchIterator::reverse(120, 100, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_exact_boundary() {
        let mut iter = BatchIterator::forward(100, 199, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_exact_boundary() {
        let mut iter = BatchIterator::reverse(199, 100, 50);
        assert_eq!(iter.next(), Some(150..=199));
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_empty_range() {
        let mut iter = BatchIterator::forward(200, 100, 50);
        assert_eq!(iter.next(), None);
        assert!(iter.is_exhausted());
    }

    #[test]
    fn reverse_empty_range() {
        let mut iter = BatchIterator::reverse(100, 200, 50);
        assert_eq!(iter.next(), None);
        assert!(iter.is_exhausted());
    }

    #[test]
    fn forward_single_block() {
        let mut iter = BatchIterator::forward(100, 100, 50);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_single_block() {
        let mut iter = BatchIterator::reverse(100, 100, 50);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn forward_max_block_range_one() {
        let mut iter = BatchIterator::forward(100, 103, 1);
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), Some(101..=101));
        assert_eq!(iter.next(), Some(102..=102));
        assert_eq!(iter.next(), Some(103..=103));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_max_block_range_one() {
        let mut iter = BatchIterator::reverse(103, 100, 1);
        assert_eq!(iter.next(), Some(103..=103));
        assert_eq!(iter.next(), Some(102..=102));
        assert_eq!(iter.next(), Some(101..=101));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_rewinds_forward_iteration() {
        let mut iter = BatchIterator::forward(100, 300, 50);
        assert_eq!(iter.next(), Some(100..=149));
        assert_eq!(iter.next(), Some(150..=199));

        iter.reset_to(175);

        assert_eq!(iter.next(), Some(175..=224));
        assert_eq!(iter.next(), Some(225..=274));
        assert_eq!(iter.next(), Some(275..=300));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_rewinds_reverse_iteration() {
        let mut iter = BatchIterator::reverse(300, 100, 50);
        assert_eq!(iter.next(), Some(251..=300));
        assert_eq!(iter.next(), Some(201..=250));

        // Reset to re-scan from a higher block (simulating reorg detection)
        iter.reset_to(280);

        assert_eq!(iter.next(), Some(231..=280));
        assert_eq!(iter.next(), Some(181..=230));
        assert_eq!(iter.next(), Some(131..=180));
        assert_eq!(iter.next(), Some(100..=130));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_after_exhausted() {
        let mut iter = BatchIterator::forward(100, 120, 50);
        assert_eq!(iter.next(), Some(100..=120));
        assert_eq!(iter.next(), None);
        assert!(iter.is_exhausted());

        iter.reset_to(110);

        assert!(!iter.is_exhausted());
        assert_eq!(iter.next(), Some(110..=120));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_beyond_end_exhausts_forward() {
        let mut iter = BatchIterator::forward(100, 200, 50);
        assert_eq!(iter.next(), Some(100..=149));

        iter.reset_to(250);

        assert!(iter.is_exhausted());
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reset_to_beyond_end_exhausts_reverse() {
        let mut iter = BatchIterator::reverse(200, 100, 50);
        assert_eq!(iter.next(), Some(151..=200));

        iter.reset_to(50);

        assert!(iter.is_exhausted());
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn current_returns_position() {
        let mut iter = BatchIterator::forward(100, 300, 50);
        assert_eq!(iter.current(), 100);

        iter.next();
        assert_eq!(iter.current(), 150);

        iter.next();
        assert_eq!(iter.current(), 200);
    }

    #[test]
    fn end_returns_boundary() {
        let iter = BatchIterator::forward(100, 300, 50);
        assert_eq!(iter.end(), 300);
    }

    #[test]
    fn direction_returns_correct_value() {
        let forward = BatchIterator::forward(100, 200, 50);
        assert_eq!(forward.direction(), Direction::Forward);

        let reverse = BatchIterator::reverse(200, 100, 50);
        assert_eq!(reverse.direction(), Direction::Reverse);
    }

    #[test]
    fn forward_starting_from_zero() {
        let mut iter = BatchIterator::forward(0, 100, 50);
        assert_eq!(iter.next(), Some(0..=49));
        assert_eq!(iter.next(), Some(50..=99));
        assert_eq!(iter.next(), Some(100..=100));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn reverse_ending_at_zero() {
        let mut iter = BatchIterator::reverse(100, 0, 50);
        assert_eq!(iter.next(), Some(51..=100));
        assert_eq!(iter.next(), Some(1..=50));
        assert_eq!(iter.next(), Some(0..=0));
        assert_eq!(iter.next(), None);
    }

    #[test]
    #[should_panic(expected = "max_block_range must be at least 1")]
    fn forward_zero_max_block_range_panics() {
        let _ = BatchIterator::forward(100, 200, 0);
    }

    #[test]
    #[should_panic(expected = "max_block_range must be at least 1")]
    fn reverse_zero_max_block_range_panics() {
        let _ = BatchIterator::reverse(200, 100, 0);
    }

    #[test]
    fn batch_count_increments() {
        let mut iter = BatchIterator::forward(100, 300, 50);
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
        let mut iter = BatchIterator::forward(100, 300, 50);
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
