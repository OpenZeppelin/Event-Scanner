use std::collections::VecDeque;

use alloy::primitives::BlockNumber;

#[derive(Copy, Clone, Debug)]
pub enum RingBufferCapacity {
    /// Keep at most `n` items.
    ///
    /// A value of `0` disables storing past block hashes and effectively disables reorg
    /// detection.
    Limited(usize),
    /// Keep an unbounded number of items.
    ///
    /// WARNING: This can lead to unbounded memory growth over long-running processes.
    /// Avoid using this in production deployments without an external bound.
    Infinite,
}

macro_rules! impl_from_unsigned {
    ($target:ty; $($source:ty),+ $(,)?) => {
        $(
            impl From<$source> for $target {
                fn from(value: $source) -> Self {
                    RingBufferCapacity::Limited(value as usize)
                }
            }
        )+
    };
}

impl_from_unsigned!(RingBufferCapacity; u8, u16, u32, usize);

/// Information about a block stored in the ring buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockInfo<H> {
    pub number: BlockNumber,
    pub hash: H,
}

#[derive(Clone, Debug)]
pub(crate) struct RingBuffer<T> {
    inner: VecDeque<T>,
    capacity: RingBufferCapacity,
}

impl<T> RingBuffer<T> {
    /// Creates an empty [`RingBuffer`] with a specific capacity.
    pub fn new(capacity: RingBufferCapacity) -> Self {
        if let RingBufferCapacity::Limited(limit) = capacity {
            Self { inner: VecDeque::with_capacity(limit), capacity }
        } else {
            Self { inner: VecDeque::new(), capacity }
        }
    }

    /// Adds a new element to the buffer.
    ///
    /// If limited capacity and the buffer is full, the oldest element is removed to make space.
    pub fn push(&mut self, item: T) {
        match self.capacity {
            RingBufferCapacity::Infinite => {
                self.inner.push_back(item); // Add the new element
            }
            RingBufferCapacity::Limited(0) => {
                // Do nothing, reorg handling disabled
            }
            RingBufferCapacity::Limited(limit) => {
                if self.inner.len() == limit {
                    self.inner.pop_front(); // Remove the oldest element
                }
                self.inner.push_back(item); // Add the new element
            }
        }
    }

    /// Removes and returns the newest element from the buffer.
    pub fn pop_back(&mut self) -> Option<T> {
        self.inner.pop_back()
    }

    /// Returns a reference to the newest element in the buffer.
    pub fn back(&self) -> Option<&T> {
        self.inner.back()
    }

    /// Clears all elements currently stored in the buffer.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<H> RingBuffer<BlockInfo<H>> {
    /// Finds a block by its number.
    ///
    /// Performs a reverse linear search since we typically look for recent blocks.
    pub fn find_by_number(&self, number: BlockNumber) -> Option<&BlockInfo<H>> {
        self.inner.iter().rev().find(|info| info.number == number)
    }

    /// Truncates the buffer to remove all blocks after (and including) the given block number.
    /// This is used when a reorg is detected to remove the reorged blocks.
    pub fn truncate_from(&mut self, from_number: BlockNumber) {
        self.inner.retain(|info| info.number < from_number);
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::B256;

    use super::*;

    #[test]
    fn zero_capacity_should_ignore_elements() {
        let mut buf = RingBuffer::<u32>::new(RingBufferCapacity::Limited(0));
        buf.push(1);
        assert!(buf.inner.is_empty());
    }

    #[test]
    fn find_by_number_returns_correct_block() {
        let mut buf = RingBuffer::<BlockInfo<B256>>::new(RingBufferCapacity::Limited(10));

        let hash1 = B256::repeat_byte(1);
        let hash2 = B256::repeat_byte(2);
        let hash3 = B256::repeat_byte(3);

        buf.push(BlockInfo { number: 100, hash: hash1 });
        buf.push(BlockInfo { number: 101, hash: hash2 });
        buf.push(BlockInfo { number: 102, hash: hash3 });

        assert_eq!(buf.find_by_number(100).map(|b| b.hash), Some(hash1));
        assert_eq!(buf.find_by_number(101).map(|b| b.hash), Some(hash2));
        assert_eq!(buf.find_by_number(102).map(|b| b.hash), Some(hash3));
        assert_eq!(buf.find_by_number(99), None);
        assert_eq!(buf.find_by_number(103), None);
    }

    #[test]
    fn truncate_from_removes_blocks_from_given_number() {
        let mut buf = RingBuffer::<BlockInfo<B256>>::new(RingBufferCapacity::Limited(10));

        buf.push(BlockInfo { number: 100, hash: B256::repeat_byte(1) });
        buf.push(BlockInfo { number: 101, hash: B256::repeat_byte(2) });
        buf.push(BlockInfo { number: 102, hash: B256::repeat_byte(3) });
        buf.push(BlockInfo { number: 103, hash: B256::repeat_byte(4) });

        buf.truncate_from(102);

        assert!(buf.find_by_number(100).is_some());
        assert!(buf.find_by_number(101).is_some());
        assert!(buf.find_by_number(102).is_none());
        assert!(buf.find_by_number(103).is_none());
    }

    #[test]
    fn is_empty_works_correctly() {
        let mut buf = RingBuffer::<BlockInfo<B256>>::new(RingBufferCapacity::Limited(10));
        assert!(buf.is_empty());

        buf.push(BlockInfo { number: 100, hash: B256::repeat_byte(1) });
        assert!(!buf.is_empty());

        buf.clear();
        assert!(buf.is_empty());
    }
}
