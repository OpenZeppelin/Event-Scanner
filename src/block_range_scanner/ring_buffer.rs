use std::collections::VecDeque;

use alloy::{eips::BlockNumHash, primitives::BlockNumber};

/// Configuration for how many past block hashes to retain for reorg detection.
///
/// This type is re-exported as `PastBlocksStorageCapacity` from the crate root.
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

#[derive(Clone, Debug)]
pub(crate) struct RingBuffer {
    inner: VecDeque<BlockNumHash>,
    capacity: RingBufferCapacity,
}

impl RingBuffer {
    /// Creates an empty [`RingBuffer`] with a specific capacity.
    pub fn new(capacity: RingBufferCapacity) -> Self {
        if let RingBufferCapacity::Limited(limit) = capacity {
            Self { inner: VecDeque::with_capacity(limit), capacity }
        } else {
            Self { inner: VecDeque::new(), capacity }
        }
    }

    /// Adds a new `BlockNumHash` to the buffer.
    ///
    /// If limited capacity and the buffer is full, the oldest element is removed to make space.
    pub fn push(&mut self, item: BlockNumHash) {
        match self.capacity {
            RingBufferCapacity::Infinite => {
                self.inner.push_back(item);
            }
            RingBufferCapacity::Limited(0) => {
                // Do nothing, reorg handling disabled
            }
            RingBufferCapacity::Limited(limit) => {
                if self.inner.len() == limit {
                    self.inner.pop_front(); // Remove the oldest element
                }
                self.inner.push_back(item);
            }
        }
    }

    /// Appends multiple `BlockNumHash` entries to the buffer.
    pub fn append(&mut self, items: impl IntoIterator<Item = BlockNumHash>) {
        for item in items {
            self.push(item);
        }
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns a reference to the oldest element in the buffer.
    pub fn front(&self) -> Option<&BlockNumHash> {
        self.inner.front()
    }

    /// Returns a reference to the newest element in the buffer.
    pub fn back(&self) -> Option<&BlockNumHash> {
        self.inner.back()
    }

    /// Finds a block by its number.
    ///
    /// Since blocks are stored in sorted order, uses binary search for O(log n) lookup.
    pub fn get(&self, number: BlockNumber) -> Option<&BlockNumHash> {
        let idx = self.inner.partition_point(|entry| entry.number < number);
        self.inner.get(idx).filter(|entry| entry.number == number)
    }

    /// Truncates the buffer, keeping only blocks up to and including `keep_number`.
    ///
    /// Removes all blocks with number > `keep_number`.
    /// Uses binary search since the buffer is sorted by block number.
    pub fn truncate_after(&mut self, keep_number: BlockNumber) {
        let idx = self.inner.partition_point(|entry| entry.number <= keep_number);
        self.inner.truncate(idx);
    }

    /// Clears all elements currently stored in the buffer.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::B256;

    use super::*;

    fn block(number: u64, byte: u8) -> BlockNumHash {
        BlockNumHash::new(number, B256::repeat_byte(byte))
    }

    #[test]
    fn zero_capacity_should_ignore_elements() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(0));
        buf.push(block(1, 1));
        assert!(buf.is_empty());
    }

    #[test]
    fn push_and_back_returns_newest() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        buf.push(block(100, 1));
        buf.push(block(101, 2));
        assert_eq!(buf.back().unwrap().number, 101);
    }

    #[test]
    fn front_returns_oldest() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        buf.push(block(100, 1));
        buf.push(block(101, 2));
        assert_eq!(buf.front().unwrap().number, 100);
    }

    #[test]
    fn get_finds_block_by_number() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        let h1 = B256::repeat_byte(1);
        let h2 = B256::repeat_byte(2);
        let h3 = B256::repeat_byte(3);

        buf.push(BlockNumHash::new(100, h1));
        buf.push(BlockNumHash::new(101, h2));
        buf.push(BlockNumHash::new(102, h3));

        assert_eq!(buf.get(100).map(|b| b.hash), Some(h1));
        assert_eq!(buf.get(101).map(|b| b.hash), Some(h2));
        assert_eq!(buf.get(102).map(|b| b.hash), Some(h3));
        assert!(buf.get(99).is_none());
        assert!(buf.get(103).is_none());
    }

    #[test]
    fn truncate_after_keeps_blocks_up_to_number() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        buf.push(block(100, 1));
        buf.push(block(101, 2));
        buf.push(block(102, 3));
        buf.push(block(103, 4));

        buf.truncate_after(101);

        assert!(buf.get(100).is_some());
        assert!(buf.get(101).is_some());
        assert!(buf.get(102).is_none());
        assert!(buf.get(103).is_none());
    }

    #[test]
    fn append_adds_multiple_blocks() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        buf.push(block(100, 1));
        buf.append([block(101, 2), block(102, 3)]);

        assert_eq!(buf.back().unwrap().number, 102);
        assert!(buf.get(101).is_some());
    }

    #[test]
    fn is_empty_works_correctly() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(10));
        assert!(buf.is_empty());
        buf.push(block(100, 1));
        assert!(!buf.is_empty());
        buf.clear();
        assert!(buf.is_empty());
    }

    #[test]
    fn limited_capacity_evicts_oldest() {
        let mut buf = RingBuffer::new(RingBufferCapacity::Limited(3));
        buf.push(block(100, 1));
        buf.push(block(101, 2));
        buf.push(block(102, 3));
        buf.push(block(103, 4));

        assert!(buf.get(100).is_none()); // evicted
        assert_eq!(buf.front().unwrap().number, 101);
        assert_eq!(buf.back().unwrap().number, 103);
    }
}
