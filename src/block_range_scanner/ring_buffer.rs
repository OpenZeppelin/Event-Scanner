use std::collections::VecDeque;

#[derive(Clone)]
pub struct RingBuffer<T> {
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

    /// Removes and returns the oldest element from the buffer, or None if it's empty.
    pub fn pop(&mut self) -> Option<T> {
        self.inner.pop_front()
    }

    pub fn back(&self) -> Option<&T> {
        self.inner.back()
    }

    /// Returns the current number of elements in the buffer.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns the maximum capacity of the buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}
