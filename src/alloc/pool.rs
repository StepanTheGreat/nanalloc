use core::ptr::null_mut;

use crate::{NanAlloc, alloc::BLOCK_SIZE};

/// The minimum size of a chunk within a pool. This requirement exists because
/// class pools live at the start of a block, hence they consume at least a single block. 
const MIN_POOL_CHUNK_SIZE: usize = 32;

/// A fixed-size pool used to redistribute large memory blocks into smaller chunks.
/// 
/// Under the hood it links other pools via pointers.
/// Each pool has a reference to the next pool and a pointer to the pointer to the root pool.
/// 
/// The most important properties:
/// 1. Pools are consumed from left to right
/// 2. The linked chain contains ONLY non-empty pools
/// 3. Each pool has a pointer to the next pool AND a pointer to the pointer to the root pool
/// 4. When a pool is out of chunks - it relinks the root to point to its next pool
///    - If that pool is null - allocate a new one
/// 5. The moment a pool gets back its chunk capacity and 
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

    /// Pointer to the pool list
    pool_list: *mut ClassPoolList,

    /// How many chunks we got
    available_chunks: usize
}

impl ClassPool {
    /// Create a new class pool from the provided block pointer and chunk size
    /// 
    /// This requires that the chunk size is power of two and >= [MIN_POOL_CHUNK_SIZE]
    unsafe fn new(
        block_ptr: *mut u8, 
        pool_list: *mut ClassPoolList,
        next_pool: *mut ClassPool, 
        chunk_size: usize
    ) -> Option<Self> {
        if !chunk_size.is_power_of_two() {
            return None;
        } else if chunk_size < MIN_POOL_CHUNK_SIZE {
            return None;
        }

        let available_chunks = (BLOCK_SIZE / chunk_size) - 1;
        
        let mut this = Self {
            chunk_size,
            next_chunk: null_mut(),
            next_pool,
            pool_list,
            available_chunks
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

    pub fn has_available_chunk(&self) -> bool {
        !self.next_chunk.is_null()
    }

    /// How many chunks available we got in total
    pub fn total_available_chunks(&self) -> usize {
        (BLOCK_SIZE / self.chunk_size) - 1
    }

    /// Is our entire pool fully free?
    pub fn is_fully_free(&self) -> bool {
        self.available_chunks == self.total_available_chunks() 
    }

    /// Free this chunk and add it to the chain
    /// 
    /// # Safety
    /// The passed chunk pointer is read as is it is, ONLY the pointer returned by the class pool can be passed back.
    unsafe fn free_chunk(&mut self, chunk: *mut u8) {
        unsafe { (chunk as *mut *mut u8).write(self.next_chunk) };
        self.next_chunk = chunk;
        self.available_chunks += 1;
    }

    /// Take the next chunk from the linked list if one is present
    /// 
    /// # Safety
    /// 
    unsafe fn take_chunk(&mut self) -> Option<*mut u8> {

        // ptr3 -> ptr2 -> ptr1 -> null
        // self.next_chunk = ptr2
        // current_chunk = ptr2
        // next_chunk = ptr1

        if self.has_available_chunk() {
            let current_chunk = self.next_chunk;
            let next_chunk = unsafe {(current_chunk as *mut *mut u8).read()};
            self.next_chunk = next_chunk;
            self.available_chunks -= 1;

            Some(current_chunk)
        } else {
            None
        }
    }
}

/// A class pool pointers used to reference pools
pub struct ClassPoolList {
    chunk_size: usize,
    root_pool: *mut ClassPool
}

impl ClassPoolList {
    pub fn new(chunk_size: usize) -> Option<Self> {
        if !chunk_size.is_power_of_two() {
            return None;
        } else if chunk_size < MIN_POOL_CHUNK_SIZE {
            return None;
        }

        Some(Self {
            chunk_size,
            root_pool: null_mut()
        })
    }

    fn add_pool(&'static mut self, pool: *mut ClassPool) {

    }

    /// Add a new pool to the chain
    fn add_new_pool(&'static mut self, alloc: &NanAlloc) -> Option<()> {
        // Find a free single block
        let pool_ptr = alloc.find_block(BLOCK_SIZE)? as *mut ClassPool;

        // Create a write our new pool directly into the block
        unsafe {
            pool_ptr.write(ClassPool::new(
                pool_ptr as *mut u8, 
                self as *mut ClassPoolList,
                self.root_pool, 
                self.chunk_size) 
            ?);
        }

        // Add it to our chain
        self.root_pool = pool_ptr;

        Some(())
    } 
}