use std::collections::VecDeque;

#[derive(Clone)]
pub(crate) struct RingBuffer<T> {
    inner: VecDeque<T>,
    capacity: usize,
}

impl<T> RingBuffer<T> {
    /// Creates an empty RingBuffer with a specific capacity.
    pub fn new(capacity: usize) -> Self {
        Self { inner: VecDeque::with_capacity(capacity), capacity }
    }

    /// Adds a new element to the buffer. If the buffer is full,
    /// the oldest element is removed to make space.
    pub fn push(&mut self, item: T) {
        if self.inner.len() == self.capacity {
            self.inner.pop_front(); // Remove the oldest element
        }
        self.inner.push_back(item); // Add the new element
    }

    pub fn pop_back(&mut self) -> Option<T> {
        self.inner.pop_back()
    }

    pub fn back(&self) -> Option<&T> {
        self.inner.back()
    }
}
