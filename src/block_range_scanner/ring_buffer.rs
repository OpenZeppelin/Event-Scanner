use std::collections::VecDeque;

#[derive(Copy, Clone, Debug)]
pub enum RingBufferCapacity {
    Limited(usize),
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

#[derive(Clone)]
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

    pub fn pop_back(&mut self) -> Option<T> {
        self.inner.pop_back()
    }

    pub fn back(&self) -> Option<&T> {
        self.inner.back()
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_capacity_should_ignore_elements() {
        let mut buf = RingBuffer::<u32>::new(RingBufferCapacity::Limited(0));
        buf.push(1);
        assert!(buf.inner.is_empty());
    }
}
