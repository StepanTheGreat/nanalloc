use core::ptr::null_mut;

use crate::{NanAlloc, alloc::{BLOCK_SIZE, get_block_ptr, is_block_aligned}};

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
    /// Allocate a new class pool from the provided pool list. Can fail if the allocator fails to find a block.
    unsafe fn alloc(alloc: &NanAlloc, pool_list: *mut ClassPoolList) -> Option<*mut Self> {
        let block_ptr = alloc.find_block(BLOCK_SIZE)?;

        let pool_ptr = block_ptr as *mut ClassPool;

        let chunk_size = unsafe { (*pool_list).chunk_size };

        // Create our pool and write itself into our newly allocated block
        unsafe { pool_ptr.write(Self {
            next_chunk: null_mut(),
            next_pool: null_mut(),
            prev_pool: null_mut(), 
            pool_list,
            chunk_size,
            available_chunks: 0,
        }) };

        // We'll be referring to it by pointer
        let this = pool_ptr;

        // Build up a list of available chunks for it (starting from 1, because the first one contains our metadata)
        for ind in 1..(BLOCK_SIZE/chunk_size) {
            unsafe { (*this).free_chunk(alloc, block_ptr.add(ind*chunk_size)) };
        }

        // The former step will automatically link our pool 

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

    /// Free this chunk and add it to the pool's chain. Internally this will also check if the entire list's capacity
    /// is not too large to keep this pool. If this pool is fully free (no references to its chunks) and the list has enough chunks from other
    /// pools - this one and its block will get freed.
    /// 
    /// # Safety
    /// The passed chunk pointer is read as is it is, ONLY the pointer returned by the class pool can be passed back.
    unsafe fn free_chunk(&mut self, alloc: &NanAlloc, chunk: *mut u8) {
        unsafe { (chunk as *mut *mut u8).write(self.next_chunk) };
        self.next_chunk = chunk;

        // If we didn't have any chunks - then our pool was unlinked
        if !self.has_available_chunk() {
            unsafe { self.relink() };
        }

        // Make sure to update both our and pool's total available chunk count
        self.available_chunks += 1;
        unsafe { (*self.pool_list).total_available_chunks += 1 };

        // In case this pool becomes useless weight - unlink it
        if self.can_be_freed() {
            unsafe { self.unlink_free(alloc) };
        }
    }

    /// Take the next chunk from the linked list if one is present. If there are no more 
    /// chunks - will automatically unlink itself (not free, only temporarily unlink).
    /// 
    /// # Safety
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

            // If we're out of chunks - unlink
            if !self.has_available_chunk() {
                unsafe { self.unlink(); }
            }

            Some(current_chunk)
        } else {
            None
        }
    }

    /// Unlink itself from the list, until we get back our chunks. Should ONLY happen when there are no more available chunks.
    /// 
    /// # Safety
    /// Should ONLY Be called when the pool was linked and it lost its final chunk 
    unsafe fn unlink(&mut self) {
        // We'll imagine the following setup (ptr2 is our pool):
        // <-- ptr1 <--> ptr2 <--> ptr3 -->

        // If our pool directly points to us - point it to our next pool
        {
            if (unsafe { (*self.pool_list).root_pool }) == self as *mut Self {
                unsafe { (*self.pool_list).root_pool = self.next_pool };
            } 
        }

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

        // We now got the following
        // <-- ptr1 <--> ptr3 -->
    }

    /// Link this pool back to the list
    /// 
    /// # Safety
    /// Should ONLY be called when the pool was unlinked and it got a chunk. 
    unsafe fn relink(&mut self) {
        self.prev_pool = null_mut();
        self.next_pool = unsafe { (*self.pool_list).root_pool };
        unsafe { (*self.pool_list).root_pool = self as *mut Self };
    }

    /// Unlink and free this pool.
    /// 
    /// # Safety 
    /// Similar to [unlink]
    unsafe fn unlink_free(&mut self, alloc: &NanAlloc) {
        unsafe { self.unlink() };

        let block_ptr = self as *mut Self as *mut u8;
        let _ = alloc.free_block(block_ptr, BLOCK_SIZE);
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

    /// Take a chunk if possible (can fail if the allocator cannot provide any more blocks)
    pub unsafe fn take_chunk(this: *mut Self, alloc: &NanAlloc) -> Option<*mut u8> {
        
        unsafe {
            // If no pool is available - create a new one
            if (*this).root_pool.is_null() {
                (*this).root_pool = ClassPool::alloc(alloc, this)?;
            }
    
            (*(*this).root_pool).take_chunk()
        }
    }

    /// Free this chunk from the provided block.
    /// 
    /// # Safety
    /// Only non-block aligned pointers can be freed as chunks. The reason: pool chunks are never block aliged, which is a good 
    /// way to tell if a pointer is a pool or a block pointer.
    pub unsafe fn free_chunk(alloc: &NanAlloc, chunk: *mut u8) {
        if !is_block_aligned(chunk) {
            return;
        }

        unsafe {
            // Get our pool pointer
            let pool_ptr = get_block_ptr(chunk) as *mut ClassPool;

            // Free out chunk
            (*pool_ptr).free_chunk(alloc, chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{NanAlloc, alloc::pool::{ClassPool, ClassPoolList, MIN_POOL_CHUNK_SIZE}};

    #[test]
    fn test_class_pool() {

        let alloc = NanAlloc::new();

        let mut pool_list64 = ClassPoolList::new(64).unwrap();
        let mut pool_list128 = ClassPoolList::new(128).unwrap();

        // We cannot use 32 byte class pools in tests (because our word size can be larger than 32 bits), but this MUST work in wasm (because
        // its address space is 32 bit wide).
        ClassPoolList::new(32).unwrap();

        // Only power of two sizes are allowed
        assert!(ClassPoolList::new(63).is_none());

        // We must respect the minimum pool chunk size
        assert!(ClassPoolList::new(MIN_POOL_CHUNK_SIZE-1).is_none());


    }
}