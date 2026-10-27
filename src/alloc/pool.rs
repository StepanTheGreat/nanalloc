use core::ptr::null_mut;

use crate::alloc::BLOCK_SIZE;

/// The minimum size of a chunk within a pool. This requirement exists because
/// class pools live at the start of a block, hence they consume at least a single block. 
const MIN_POOL_CHUNK_SIZE: usize = 32;

/// A fixed-size pool used to redistribute large memory blocks into smaller chunks.
pub struct ClassPool {
    /// The size of a single pool chunk.
    chunk_size: usize,

    /// The next chunk to use. It's a linked list. Null means that this pool is fully taken.
    /// Example: ptr3 -> ptr2 -> ptr1 -> null
    /// 
    /// When we consume a chunk - we take the current root pointer, and set the current root pointer as
    /// the next chunk it's pointing at (unless it's null, in which case nothing happens)
    /// 
    /// When we free a chunk - we simply write our current root pointer to the newly freed chunk.
    next_chunk: *mut u8,

    /// Pointer to the next pool (if not null)
    next_pool: *mut ClassPool,

    /// Pointer to the previous pool (if not null)
    prev_pool: *mut ClassPool
}

impl ClassPool {
    /// Create a new class pool from the provided block pointer and chunk size
    /// 
    /// This requires that the chunk size is power of two and >= [MIN_POOL_CHUNK_SIZE]
    pub unsafe fn new(block_ptr: *mut u8, chunk_size: usize) -> Option<Self> {
        if !chunk_size.is_power_of_two() {
            return None;
        } else if chunk_size < MIN_POOL_CHUNK_SIZE {
            return None;
        }
        
        let mut this = Self {
            chunk_size,
            next_chunk: null_mut(),
            next_pool: null_mut(),
            prev_pool: null_mut(),
        };

        // Initialize everything
        for ind in 1..(BLOCK_SIZE/chunk_size) {
            unsafe { this.free_chunk(block_ptr.add(ind*chunk_size)) };
        }

        Some(this)
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Free this chunk and add it to the chain
    pub unsafe fn free_chunk(&mut self, chunk: *mut u8) {
        unsafe { (chunk as *mut *mut u8).write(self.next_chunk) };
        self.next_chunk = chunk;
    }

    /// Take the next chunk from the linked list if one is present
    pub unsafe fn take_chunk(&mut self) -> Option<*mut u8> {

        // ptr3 -> ptr2 -> ptr1 -> null
        // self.next_chunk = ptr2
        // current_chunk = ptr2
        // next_chunk = ptr1

        if self.next_chunk.is_null() {
            return None;
        } else {
            let current_chunk = self.next_chunk;
            let next_chunk = unsafe {(current_chunk as *mut *mut u8).read()};
            self.next_chunk = next_chunk;

            Some(current_chunk)
        }
    }
}