use core::{cell::Cell, ptr};

use crate::{
    NanAlloc,
    alloc::{BLOCK_SIZE, get_block_ptr, is_block_aligned},
};

/// The minimum size of a chunk within a pool. This requirement exists because
/// class pools live at the start of a block, hence they consume at least a single block.
const MIN_POOL_CHUNK_SIZE: usize = 32;

const MAX_POOL_CHUNK_SIZE: usize = BLOCK_SIZE / 4;

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
    /// The pointer to the allocated block itself
    block_ptr: *mut u8,

    /// The next chunk to use. It's a linked list. Null means that this pool is fully taken.
    /// Example: ptr3 -> ptr2 -> ptr1 -> null
    ///
    /// When we consume a chunk - we take the current root pointer, and set the current root pointer as
    /// the next chunk it's pointing at (unless it's null, in which case nothing happens)
    ///
    /// When we free a chunk - we simply write our current root pointer to the newly freed chunk.
    next_chunk: Cell<*mut u8>,

    /// Pointer to the next pool (if not null)
    next_pool: Cell<*const ClassPool>,

    /// Pointer to the previous pool. Used for freeing full class pools
    prev_pool: Cell<*const ClassPool>,

    /// Pointer to the pool list
    pool_list: &'static ClassPoolList,

    /// How many chunks we got
    available_chunks: Cell<usize>,
}

impl ClassPool {
    /// Allocate a new class pool from the provided pool list. Can fail if the allocator fails to find a block.
    fn alloc(alloc: &NanAlloc, pool_list: &'static ClassPoolList) -> Option<*const Self> {
        let block_ptr = alloc.find_block(BLOCK_SIZE)?;

        let pool_ptr = block_ptr as *mut ClassPool;

        let chunk_size = pool_list.chunk_size;

        // Create our pool and write itself into our newly allocated block
        unsafe {
            pool_ptr.write(Self {
                next_chunk: Cell::new(ptr::null_mut()),
                next_pool: Cell::new(ptr::null_mut()),
                prev_pool: Cell::new(ptr::null_mut()),
                block_ptr,
                pool_list,
                available_chunks: Cell::new(0),
            })
        };

        // We'll be referring to it by pointer
        let this = pool_ptr;

        // Build up a list of available chunks for it (starting from 1, because the first one contains our metadata)
        for ind in 1..(BLOCK_SIZE / chunk_size) {
            unsafe { (*this).free_chunk(alloc, block_ptr.add(ind * chunk_size)) };
        }

        // The former step will automatically link our pool

        Some(this)
    }

    /// How many chunks this pool can contain in total
    pub fn pool_chunk_capacity(&self) -> usize {
        (BLOCK_SIZE / self.chunk_size()) - 1
    }

    pub fn chunk_size(&self) -> usize {
        self.pool_list.chunk_size
    }

    pub fn has_available_chunk(&self) -> bool {
        !self.next_chunk.get().is_null()
    }

    fn increment_available_chunks(&self, by: usize) {
        self.available_chunks.set(self.available_chunks.get() + by);
        self.pool_list.increment_total_chunks(by);
    }

    fn decrement_available_chunks(&self, by: usize) {
        self.available_chunks.set(self.available_chunks.get() - by);
        self.pool_list.decrement_total_chunks(by);
    }

    /// Is our entire pool fully free?
    pub fn is_fully_free(&self) -> bool {
        self.available_chunks.get() == self.pool_chunk_capacity()
    }

    /// A pool can be freed if:
    /// 1. It's full free (no actively borrowed memory)
    /// 2. The total amount of free chunks within the list is greater than of 2 free pools (exlcuding ours that means 1 free pool worth of chunks)
    ///
    /// The primary idea behind this is simple: too many free pools only take up blocks. We should at least
    /// keep 1 for quick allocation, but anything else isn't neccessary.
    pub fn can_be_freed(&self) -> bool {
        let list_chunks = self.pool_list.total_available_chunks.get();

        self.is_fully_free() && (list_chunks >= (self.pool_chunk_capacity() * 2))
    }

    /// Free this chunk and add it to the pool's chain. Internally this will also check if the entire list's capacity
    /// is not too large to keep this pool. If this pool is fully free (no references to its chunks) and the list has enough chunks from other
    /// pools - this one and its block will get freed.
    ///
    /// # Safety
    /// The passed chunk pointer is read as is it is, ONLY the pointer returned by the class pool can be passed back.
    unsafe fn free_chunk(&self, alloc: &NanAlloc, chunk: *mut u8) {
        let was_pool_unlinked = !self.has_available_chunk();

        unsafe { (chunk as *mut *mut u8).write(self.next_chunk.get()) };
        self.next_chunk.set(chunk);

        // If we didn't have any chunks - then our pool was unlinked
        if was_pool_unlinked {
            unsafe { self.relink() };
        }

        // Make sure to update both our and pool's total available chunk count
        self.increment_available_chunks(1);

        // In case this pool becomes useless weight - unlink it
        if self.can_be_freed() {
            unsafe { self.unlink_free(alloc) };
        }
    }

    /// Take the next chunk from the linked list if one is present. If there are no more
    /// chunks - will automatically unlink itself (not free, only temporarily unlink).
    ///
    /// # Safety
    unsafe fn take_chunk(&self) -> Option<*mut u8> {
        // ptr3 -> ptr2 -> ptr1 -> null
        // self.next_chunk = ptr2
        // current_chunk = ptr2
        // next_chunk = ptr1

        if !self.has_available_chunk() {
            return None;
        }

        // The chunk we're about to return
        let current_chunk = self.next_chunk.get();

        // The chunk we're going to read next
        let next_chunk = unsafe { (current_chunk as *mut *mut u8).read() };
        self.next_chunk.set(next_chunk);

        // Update the total
        self.decrement_available_chunks(1);

        // If we're out of chunks - unlink
        if !self.has_available_chunk() {
            unsafe {
                self.unlink();
            }
        }

        Some(current_chunk)
    }

    /// Unlink itself from the list, until we get back our chunks. Should ONLY happen when there are no more available chunks.
    ///
    /// # Safety
    /// Should ONLY Be called when the pool was linked and it lost its final chunk
    unsafe fn unlink(&self) {
        // We'll imagine the following setup (ptr2 is our pool):
        // <-- ptr1 <--> ptr2 <--> ptr3 -->

        // If our pool directly points to us - point it to our next pool
        {
            if ptr::eq(self.pool_list.root_pool.get(), self) {
                self.pool_list.root_pool.set(self.next_pool.get());
            }
        }

        // First link our left pool to our right
        if !self.prev_pool.get().is_null() {
            unsafe { (*self.prev_pool.get()).next_pool.set(self.next_pool.get()) };
        }

        // Then link our right pool to our left
        if !self.next_pool.get().is_null() {
            unsafe { (*self.next_pool.get()).prev_pool.set(self.prev_pool.get()) };
        }

        self.prev_pool.set(ptr::null_mut());
        self.next_pool.set(ptr::null_mut());

        // We now got the following
        // <-- ptr1 <--> ptr3 -->
    }

    /// Link this pool back to the list
    ///
    /// # Safety
    /// Should ONLY be called when the pool was unlinked and it got a chunk.
    unsafe fn relink(&self) {
        self.prev_pool.set(ptr::null_mut());
        self.next_pool.set(self.pool_list.root_pool.get());

        // If it had a pool - link it back to us
        if !self.next_pool.get().is_null() {
            unsafe { (*self.next_pool.get()).prev_pool.set(self) };
        }

        // Set our pool as root
        self.pool_list.root_pool.set(self);
    }

    /// Unlink and free this pool.
    ///
    /// # Safety
    /// Similar to [unlink], only if the pool can be freed
    unsafe fn unlink_free(&self, alloc: &NanAlloc) {
        unsafe { self.unlink() };

        // Decrement our list's total available chunks
        self.decrement_available_chunks(self.available_chunks.get());

        // Free out block
        let _ = alloc.free_block(self.block_ptr, BLOCK_SIZE);
    }
}

/// A class pool pointers used to reference pools
pub struct ClassPoolList {
    /// The size of the chunk for that list
    chunk_size: usize,

    /// The total sum of available chunks within this list
    total_available_chunks: Cell<usize>,

    /// The pointer to the first pool
    root_pool: Cell<*const ClassPool>,
}

impl ClassPoolList {
    /// Create a new class pool from the provided chunk size (in bytes). Do note that it will still create power of two
    /// chunk sizes, the minimum being [MIN_POOL_CHUNK_SIZE] and maximum [MAX_POOL_CHUNK_SIZE].
    ///
    /// This only exists for safety, since minimum and maximum class size directly affects memory layout.
    pub const fn new(chunk_size: usize) -> Self {
        let chunk_size = chunk_size.next_power_of_two();
        let chunk_size = if chunk_size >= MIN_POOL_CHUNK_SIZE {
            chunk_size
        } else {
            MIN_POOL_CHUNK_SIZE
        };
        let chunk_size = if chunk_size <= MAX_POOL_CHUNK_SIZE {
            chunk_size
        } else {
            MAX_POOL_CHUNK_SIZE
        };

        Self {
            chunk_size,
            root_pool: Cell::new(ptr::null_mut()),
            total_available_chunks: Cell::new(0),
        }
    }

    fn increment_total_chunks(&self, by: usize) {
        self.total_available_chunks
            .set(self.total_available_chunks.get() + by);
    }

    fn decrement_total_chunks(&self, by: usize) {
        self.total_available_chunks
            .set(self.total_available_chunks.get() - by);
    }

    /// Take a chunk if possible (can fail if the allocator cannot provide any more blocks)
    pub fn take_chunk(&'static self, alloc: &NanAlloc) -> Option<*mut u8> {
        unsafe {
            // If no pool is available - create a new one
            if self.root_pool.get().is_null() {
                self.root_pool.set(ClassPool::alloc(alloc, self)?);
            }

            let root_pool = self.root_pool.get();

            (*root_pool).take_chunk()
        }
    }

    /// Free this chunk from the provided block.
    ///
    /// # Safety
    /// Only non-block aligned pointers can be freed as chunks. The reason: pool chunks are never block aliged, which is a good
    /// way to tell if a pointer is a pool or a block pointer.
    pub unsafe fn free_chunk(alloc: &NanAlloc, chunk: *mut u8) {
        if is_block_aligned(chunk) {
            return;
        }

        unsafe {
            // Get our pool pointer
            let pool_ptr = get_block_ptr(chunk) as *mut ClassPool;

            // Free out chunk
            (*pool_ptr).free_chunk(alloc, chunk);
        }
    }

    /// Check whether this class pool is empty (isn't currently linked to any pools)
    pub fn is_empty(&self) -> bool {
        self.root_pool.get().is_null()
    }

    /// How many available chunks this pool has in total
    pub fn available_chunks(&self) -> usize {
        self.total_available_chunks.get()
    }
}

#[cfg(target_arch = "wasm32")]
unsafe impl Send for ClassPoolList {}

#[cfg(target_arch = "wasm32")]
unsafe impl Sync for ClassPoolList {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use core::alloc::Layout;

    use crate::{
        NanAlloc,
        alloc::{BLOCK_SIZE, pool::ClassPoolList},
        wasm::{PAGE_SIZE, TEST_MAX_HEAP_PAGES, TestHeapHandle},
    };

    const LOCAL_HEAP_SIZE: usize = TEST_MAX_HEAP_PAGES * PAGE_SIZE;

    const LOCAL_TOTAL_BLOCKS: usize = LOCAL_HEAP_SIZE / BLOCK_SIZE;

    // No threads in wasm
    unsafe impl Send for ClassPoolList {}
    unsafe impl Sync for ClassPoolList {}

    // Test basic class pool functionality
    #[test]
    fn test_class_pool() {
        let _heap = TestHeapHandle::new();

        let alloc = NanAlloc::new();

        const POOL_SIZE: usize = 64;

        // We'll create a 64 byte pool
        static POOL_LIST64: ClassPoolList = ClassPoolList::new(POOL_SIZE);

        assert_eq!(POOL_LIST64.chunk_size, POOL_SIZE);

        assert_eq!(ClassPoolList::new(31).chunk_size, 32);
        assert_eq!(ClassPoolList::new(63).chunk_size, 64);

        // By default our pools are empty
        assert!(POOL_LIST64.is_empty());
        assert_eq!(POOL_LIST64.available_chunks(), 0);

        let mut chunks = Vec::new();

        // The amount of chunks we're expecting
        let available_chunks = BLOCK_SIZE / POOL_SIZE - 1;

        // The moment we request a chunk however - they allocate pools and start redistributing blocks
        chunks.push(POOL_LIST64.take_chunk(&alloc).unwrap());

        assert!(!POOL_LIST64.is_empty());

        // We also got some chunks
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks - 1);

        // Let's take all the remaining chunks
        for _ in 0..(available_chunks - 1) {
            chunks.push(POOL_LIST64.take_chunk(&alloc).unwrap());
        }

        // At this point our list should be empty, since ALL our chunks were allocated
        assert!(POOL_LIST64.is_empty());
        assert_eq!(POOL_LIST64.available_chunks(), 0);

        // Unless we free just a single chunk back. Which will relink back our pool.
        unsafe {
            ClassPoolList::free_chunk(&alloc, chunks.pop().unwrap());
        }
        assert!(!POOL_LIST64.is_empty());
        assert_eq!(POOL_LIST64.available_chunks(), 1);

        // Now we're going to allocate a bit more

        // Take our remaining chunk
        chunks.push(POOL_LIST64.take_chunk(&alloc).unwrap());

        // And take one more, which in turn will allocate a yet another block for an another pool
        let new_pool_chunk = POOL_LIST64.take_chunk(&alloc).unwrap();

        // We're back to square one, even though we now have two pools:
        // one is fully taken, and the other is almost fully free
        assert!(!POOL_LIST64.is_empty());
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks - 1);

        // Our allocation model makes sure that if we do end up with multiple allocated pools, and
        // those eventually get freed - only one pool should remain.

        // We'll free all chunks from the previous pool EXCEPT for one
        while chunks.len() > 1 {
            unsafe {
                ClassPoolList::free_chunk(&alloc, chunks.pop().unwrap());
            }
        }

        // Now, we got two almost fully free pools, but we're keeping allocated their one remaining chunk
        assert_eq!(POOL_LIST64.available_chunks(), (available_chunks - 1) * 2);

        // We would like to keep our first ever made pool the remaining one. For this, we're going to free it first
        unsafe {
            ClassPoolList::free_chunk(&alloc, chunks.pop().unwrap());
        }

        // Only one chunk remains
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks * 2 - 1);

        // Free the remaining chunk from the newest pool
        unsafe {
            ClassPoolList::free_chunk(&alloc, new_pool_chunk);
        }

        // And this will automatically clean it up.
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);

        // As a result, we only got 1 pool ready for any new allocations.
    }

    #[test]
    fn test_class_pool_leaks() {
        // This test will test for specific leak patterns that can appear when using the pool.
        // Because I'm ultra lazy, it will also test that our blocks are properly given back to the general allocator.

        let _heap = TestHeapHandle::new();

        let alloc = NanAlloc::new();

        const POOL_SIZE: usize = 64;

        // We'll create a 64 byte pool
        static POOL_LIST64: ClassPoolList = ClassPoolList::new(POOL_SIZE);

        // Remove and free all allocated pointers from the provided vector
        fn free_all_chunks(chunks: &mut Vec<*mut u8>, alloc: &NanAlloc) {
            for c in chunks.drain(..) {
                unsafe { ClassPoolList::free_chunk(&alloc, c) };
            }
        }

        // Take all chunks and add them to the provided vector
        fn take_all_chunks(chunks: &mut Vec<*mut u8>, alloc: &NanAlloc, available_chunks: usize) {
            for _ in 0..available_chunks {
                chunks.push(POOL_LIST64.take_chunk(&alloc).unwrap());
            }
        }

        unsafe fn alloc_blocks(alloc: &NanAlloc, blocks: usize) -> Option<*mut u8> {
            assert!(blocks.is_power_of_two());
            unsafe {
                alloc.ex_alloc(Layout::from_size_align(blocks * BLOCK_SIZE, BLOCK_SIZE).unwrap())
            }
        }

        unsafe fn free_block(alloc: &NanAlloc, ptr: *mut u8) {
            unsafe {
                alloc.ex_dealloc(
                    ptr,
                    Layout::from_size_align(BLOCK_SIZE, BLOCK_SIZE).unwrap(),
                );
            }
        }

        /// Test that this amount of blocks can be allocated
        fn can_allocate_blocks(alloc: &NanAlloc, blocks: usize) -> bool {
            assert!(blocks.is_power_of_two());

            match unsafe { alloc_blocks(alloc, 1) } {
                Some(ptr) => {
                    unsafe { free_block(alloc, ptr) };
                    true
                }
                None => false,
            }
        }

        // We're going to take ALL our existing blocks to later test if it actually cleans everything up.
        //
        // Because we're running this through miri, we're allocating blocks exponentially: out of say 4096 blocks, we're going to:
        // allocate 2048+1024+512+256+128+64+32+16+8, then the remaining 4 leave as it is. Because we specifically need 3 free blocks, we're just
        // going to allocate one final block and stop.
        {
            let mut total_blocks_left = LOCAL_TOTAL_BLOCKS;
            while total_blocks_left > 4 {
                unsafe { alloc_blocks(&alloc, total_blocks_left / 2) };
                total_blocks_left /= 2;
            }

            unsafe { alloc_blocks(&alloc, 1) };
        }

        let available_chunks = BLOCK_SIZE / POOL_SIZE - 1;

        let mut chunks1 = Vec::new();
        let mut chunks2 = Vec::new();
        let mut chunks3 = Vec::new();

        take_all_chunks(&mut chunks1, &alloc, available_chunks);
        take_all_chunks(&mut chunks2, &alloc, available_chunks);
        take_all_chunks(&mut chunks3, &alloc, available_chunks);

        // We have allocated 3 full pools
        assert!(POOL_LIST64.is_empty());

        // All blocks have been taken, can no longer allocate
        assert!(!can_allocate_blocks(&alloc, 1));

        // Currently they're all unlinked. What we would like to do, is free one of them one at a time in different ways.
        // (pool1) -> (pool2) -> (pool3)
        //
        // We must ensure that freeing a pool at no matter which position (pool1, pool2 or pool3) will unlink everything successfully

        // We'll free our second pool first
        free_all_chunks(&mut chunks2, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(!can_allocate_blocks(&alloc, 1));

        // And now our first one
        free_all_chunks(&mut chunks1, &alloc);

        // This should free the first pool entirely
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 1));

        // Finally, our third one
        free_all_chunks(&mut chunks3, &alloc);

        // Only one pool left.
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 2));

        // Let's repeat the test, but now keeping the first pool as the last one

        take_all_chunks(&mut chunks2, &alloc, available_chunks);
        take_all_chunks(&mut chunks1, &alloc, available_chunks);
        take_all_chunks(&mut chunks3, &alloc, available_chunks);
        assert!(!can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks1, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(!can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks3, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks2, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 2));

        // And finally, repeat, but this time keeping the last pool as the last one

        take_all_chunks(&mut chunks1, &alloc, available_chunks);
        take_all_chunks(&mut chunks3, &alloc, available_chunks);
        take_all_chunks(&mut chunks2, &alloc, available_chunks);
        assert!(!can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks3, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(!can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks1, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 1));

        free_all_chunks(&mut chunks2, &alloc);
        assert_eq!(POOL_LIST64.available_chunks(), available_chunks);
        assert!(can_allocate_blocks(&alloc, 2));
    }
}
