pub struct FixedStack<T: Copy, const S: usize,> {
    values: [T; S],
    len: usize
}

impl<T: Copy, const S: usize> FixedStack<T, S> {
    pub fn new(init: T) -> Self {
        Self {
            values: [init; S],
            len: 0
        }
    }

    /// Push a value onto a stack (if possible)
    pub fn push(&mut self, value: T) -> Option<T> {
        if self.len >= S {
            return Some(value);
        }

        self.values[self.len] = value;
        self.len += 1;

        None
    }

    /// Pop a value from a stack (if any exists)
    pub fn pop(&mut self) -> Option<T> {
        if self.len > 0 {
            self.len -= 1;
            
            Some(self.values[self.len])
        } else {
            None
        }
    }

    /// Get the last element on the stack
    pub fn head(&self) -> Option<T> {
        if self.len > 0 {
            Some(self.values[self.len-1])
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}