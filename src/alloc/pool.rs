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

    /// Pointer to the previous pool. Used for freeing full class pools
    prev_pool: *mut ClassPool,

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
        prev_pool: *mut ClassPool,
        chunk_size: usize
    ) -> Option<Self> {
        if !chunk_size.is_power_of_two() {
            return None;
        } else if chunk_size < MIN_POOL_CHUNK_SIZE {
            return None;
        }
        
        let mut this = Self {
            chunk_size,
            next_chunk: null_mut(),
            next_pool,
            prev_pool, 
            pool_list,
            available_chunks: 0
        };

        // Build up a list of available chunks
        for ind in 1..(BLOCK_SIZE/chunk_size) {
            unsafe { this.free_chunk(block_ptr.add(ind*chunk_size)) };
        }

        Some(this)
    }

    /// How many chunks a pool of the provided chunk size can contain 
    pub fn pool_chunk_capacity(chunk_size: usize) -> usize {
        (BLOCK_SIZE / chunk_size) - 1
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
        self.available_chunks == Self::pool_chunk_capacity(self.chunk_size) 
    }

    /// A pool can be freed if:
    /// 1. It's full free (no actively borrowed memory)
    /// 2. The total amount of free chunks within the list is greater than of 2 free pools (exlcuding ours that means 1 free pool worth of chunks)
    /// 
    /// The primary idea behind this is simple: too many free pools only take up blocks. We should at least
    /// keep 1 for quick allocation, but anything else isn't neccessary.
    pub fn can_be_freed(&self) -> bool {
        let list_chunks = unsafe { (*self.pool_list).total_available_chunks };

        self.is_fully_free() && 
            list_chunks >= Self::pool_chunk_capacity(self.chunk_size)*2
    }

    /// Free this chunk and add it to the chain
    /// 
    /// # Safety
    /// The passed chunk pointer is read as is it is, ONLY the pointer returned by the class pool can be passed back.
    unsafe fn free_chunk(&mut self, chunk: *mut u8) {
        unsafe { (chunk as *mut *mut u8).write(self.next_chunk) };
        self.next_chunk = chunk;

        // Make sure to update both our and pool's total available chunk count
        self.available_chunks += 1;
        unsafe { (*self.pool_list).total_available_chunks += 1 };
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

            // Update the total 
            self.available_chunks -= 1;
            unsafe { (*self.pool_list).total_available_chunks -= 1 };


            if !self.has_available_chunk() {
                unsafe { self.try_unlink(); }
            }

            Some(current_chunk)
        } else {
            None
        }
    }

    /// Unlink itself from the list, until we get back our chunks. Should ONLY happen when there are no more available chunks
    unsafe fn try_unlink(&mut self) {
        if self.has_available_chunk() {
            return;
        }

        // We'll imagine the following setup (ptr2 is our pool):
        // <-- ptr1 <--> ptr2 <--> ptr3 -->

        // First link our left pool to our right
        if !self.prev_pool.is_null() {
            unsafe { (*self.prev_pool).next_pool = self.next_pool };
            self.prev_pool = null_mut();
        }

        // Then link our right pool to our left
        if !self.next_pool.is_null() {
            unsafe { (*self.next_pool).prev_pool = self.prev_pool };
            self.next_pool = null_mut();
        }

        // TODO: Unlink from the list root

        // We now got the following
        // <-- ptr1 <--> ptr3 -->
    }
}

/// A class pool pointers used to reference pools
pub struct ClassPoolList {
    /// The size of the chunk for that list
    chunk_size: usize,

    /// The total sum of available chunks within this list
    total_available_chunks: usize,
    
    /// The pointer to the first pool
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
            root_pool: null_mut(),
            total_available_chunks: 0
        })
    }

    /// Add back an existing pool to the chain. This 
    fn add_pool(&'static mut self, pool: *mut ClassPool) {
        
    }

    /// Allocate a new pool to the chain. When there are no more available chunks
    fn add_new_pool(&'static mut self, alloc: &NanAlloc) -> Option<()> {
        // Find a free single block
        let pool_ptr = alloc.find_block(BLOCK_SIZE)? as *mut ClassPool;

        // If we had a pool before - we're going to shift it to the right
        if !self.root_pool.is_null() {
            unsafe { (*self.root_pool).prev_pool = pool_ptr };
        }

        // Create a write our new pool directly into the block
        unsafe {
            pool_ptr.write(ClassPool::new(
                pool_ptr as *mut u8, 
                self as *mut ClassPoolList,
                self.root_pool,
                null_mut(), 
                self.chunk_size
            )?);
        }

        // Add it to our chain
        self.root_pool = pool_ptr;

        Some(())
    } 
}