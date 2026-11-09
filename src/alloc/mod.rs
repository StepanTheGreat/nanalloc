//! ## The general allocation implementation
//!
//! The allocator design is divided into two core parts:
//! 1. Large block allocator (that can allocate up to a limit, large continous blocks)
//! 2. Slab allocators (responsible for tiny allocations. They reuse larger blocks and partition them into tiny ones)
//!
//! Compared to a linked list approach, this design in advance allocates an entire page to store its internal binary tree for blocks.
//! That does mean that this design essentially "wastes" 65KB worth of ram, but this makes the implementation much simpler overall.
//!
//! For looking up blocks, we're using a binary tree. This makes the entire block search logarithmic in complexity, while also
//! simpler in implementation. Each node is represented as 2 bits (0 - all free, 1 - one free, 2 - none free), packed tightly into 32 bit integers.
//! The first node entry is zeroed and ignored.  

use core::{alloc::GlobalAlloc, ptr};
use crate::{alloc::{blocks::BlockAllocator, pool::ClassPoolList}, wasm::PAGE_SIZE};

pub mod blocks;
pub mod pool;

/// The total size of the heap. We're putting here a reasonable constant that both makes our implementation simple,
const HEAP_SIZE: usize = 2usize.pow(30);

/// The size of a single allocatable memory block
const BLOCK_SIZE: usize = PAGE_SIZE / 8;

const TOTAL_CLASS_POOLS: usize = 6;

/// Check whether this pointer is block aligned
pub fn is_block_aligned(ptr: *mut u8) -> bool {
    (ptr.addr() % BLOCK_SIZE)  == 0
}

/// Get a block-aligned pointer from an arbitrary pointer
///
/// # Safety
/// For the same reason as [sub] on pointers.
pub unsafe fn get_block_ptr(ptr: *mut u8) -> *mut u8 {
    // How much to offset our pointer to the left, to make it block aligned
    let offset = ptr.addr() % BLOCK_SIZE;

    if offset > 0 {
        // Move it to the left
        unsafe { ptr.sub(offset) }
    } else {
        // In any case return it as it is
        ptr
    }
}

const LARGEST_CLASS_POOL_SIZE: usize = 2048;

/// Simply declares static allocator state. This macro exists solely to simplify static initialization for normal use and testing.
macro_rules! declare_static_alloc_state {
    () => {
        static ALLOC_BLOCKS: BlockAllocator = BlockAllocator::new();
        static ALLOC_POOLS: [ClassPoolList; TOTAL_CLASS_POOLS] = [
            ClassPoolList::new(64),
            ClassPoolList::new(128),
            ClassPoolList::new(256),
            ClassPoolList::new(512),
            ClassPoolList::new(1024),
            ClassPoolList::new(LARGEST_CLASS_POOL_SIZE),
        ];
    };
}

#[cfg(target_arch = "wasm32")]
declare_static_alloc_state!();

/// A tiny wasm32 allocator. 
/// 
/// Some of its properties:
/// - Has a heap limit of 1GB or 2^30
/// - Guarantees alignment up to 8KB or 2^13 (beyond that returns null)
/// - Has 7 classes for tiny allocations (64, 128, 256, 512, 1024 and 2048)
/// - Any allocation beyond those 7 classes automatically uses full block allocation. A single block is ALWAYS 8KB minimum.
/// - Not thread safe, expected to be used in threadless wasm
pub struct NanAlloc {
    blocks: &'static BlockAllocator,
    pools: &'static [ClassPoolList]
}

impl NanAlloc {
    #[cfg(target_arch = "wasm32")]
    pub const fn new() -> Self {
        Self {
            blocks: &ALLOC_BLOCKS,
            pools: &ALLOC_POOLS
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub const fn from_state(blocks: &'static BlockAllocator, pools: &'static [ClassPoolList]) -> Self {
        Self {
            blocks,
            pools
        }
    }

    unsafe fn ex_alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        if layout.align() > BLOCK_SIZE {
            return ptr::null_mut();
        }

        let req_size = layout.size().max(layout.align());
        
        for pool in self.pools {
            if req_size <= pool.chunk_size() {
                return pool.take_chunk(self.blocks).unwrap_or(ptr::null_mut());
            }
        }

        self.blocks.find_block(req_size).unwrap_or(ptr::null_mut())
    }

    unsafe fn ex_dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if layout.align() > BLOCK_SIZE {
            return;
        }

        let req_size = layout.size().max(layout.align());
        
        if req_size <= LARGEST_CLASS_POOL_SIZE {
            unsafe { ClassPoolList::free_chunk(self.blocks, ptr) };
        } else {
            let _ = self.blocks.free_block(ptr, req_size);
        }

    }
}

unsafe impl GlobalAlloc for NanAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        unsafe { self.ex_alloc(layout) }
        
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe { self.ex_dealloc(ptr, layout) };
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use core::alloc::{GlobalAlloc, Layout};

    use crate::alloc::{BLOCK_SIZE, NanAlloc};
    use crate::alloc::{blocks::BlockAllocator, pool::ClassPoolList};
    use crate::wasm::{PAGE_SIZE, TEST_MAX_HEAP_PAGES, TestHeapHandle};
    use super::{TOTAL_CLASS_POOLS, LARGEST_CLASS_POOL_SIZE};

    const LOCAL_HEAP_SIZE: usize = TEST_MAX_HEAP_PAGES * PAGE_SIZE;

    const LOCAL_TOTAL_BLOCKS: usize = LOCAL_HEAP_SIZE / BLOCK_SIZE;

    // No threads in wasm
    unsafe impl Send for BlockAllocator {}
    unsafe impl Sync for BlockAllocator {}

    fn make_layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn test_basic_alloc() {

        declare_static_alloc_state!();
        static ALLOC: NanAlloc = NanAlloc::from_state(&ALLOC_BLOCKS, &ALLOC_POOLS);

        let _heap = TestHeapHandle::new();

        // Let's first check some properties

        // This will fail, since this block size is much greater than our available heap
        assert!(unsafe { ALLOC.alloc(make_layout(LOCAL_HEAP_SIZE+1, BLOCK_SIZE))}.is_null() );

        // This will succeed though
        {
            let ptr = unsafe { ALLOC.alloc(make_layout(LOCAL_HEAP_SIZE, BLOCK_SIZE))};
            assert!(!ptr.is_null());
            unsafe { ALLOC.dealloc(ptr, make_layout(LOCAL_HEAP_SIZE, BLOCK_SIZE))};
        }

        // Our allocator also cannot guarantee alignment beyond our block size
        assert!(unsafe { ALLOC.alloc(make_layout(BLOCK_SIZE, BLOCK_SIZE*2))}.is_null() );

        // But this will of course work
        {
            let ptr = unsafe { ALLOC.alloc(make_layout(BLOCK_SIZE, BLOCK_SIZE))};
            assert!(!ptr.is_null());
            unsafe { ALLOC.dealloc(ptr, make_layout(BLOCK_SIZE, BLOCK_SIZE))};
        }

        // Let's now allocate a few things.

        let mut ptrs = Vec::new();

        for size in [64, 128, 256, 512, 1024, 2048, 8192] {
            let layout = make_layout(size, 2);
            let ptr = unsafe { ALLOC.alloc(layout) };
            assert!(!ptr.is_null());

            ptrs.push((ptr, layout));
        }

        // Now we'll perform much more precise tests, since we have direct knowledge of how the allocator works.
        // As of now, only 7 blocks are allocated (6 of them are pools).

        // We'll use the same miri-friendly trick from the pool tests
        let mut reserved_blocks = Vec::new();
        {
            let mut total_blocks_left = LOCAL_TOTAL_BLOCKS;
            while total_blocks_left > 8 {
                let layout = make_layout(BLOCK_SIZE*total_blocks_left/2, 2);
                let ptr = unsafe { ALLOC.alloc (layout) };

                assert!(!ptr.is_null());

                reserved_blocks.push((ptr, layout));
                
                total_blocks_left /= 2;
            }

            let layout = make_layout(BLOCK_SIZE, 2);
            let ptr = unsafe { ALLOC.alloc (layout) };

            assert!(!ptr.is_null());

            reserved_blocks.push((ptr, layout));
        }

        // Right now, literally all heap blocks are taken, so it's impossible to allocate anything
        assert!(unsafe { ALLOC.alloc (make_layout(BLOCK_SIZE, 2)) }.is_null());


        // For each class, we're going to take all remaining chunks from its pool
        for size in [64, 128, 256, 512, 1024, 2048] {

            // How many chunks we can still allocate
            let chunks_left = (BLOCK_SIZE / size - 1)-1;

            for _ in 0..chunks_left {
                let layout = make_layout(size, 2);
                let ptr = unsafe { ALLOC.alloc(layout) };
                assert!(!ptr.is_null());
    
                ptrs.push((ptr, layout));
            }
        }

        // Even though all our blocks are taken, pools still have some free capacity that we were able to use.


        // Let's free ALL of our chunks and our one block

        for (ptr, layout) in ptrs.drain(..) {
            unsafe { ALLOC.dealloc(ptr, layout) };
        }

        // As of now, 6 blocks are still allocated (pools always keep one pool worth of chunks always allocated), but our remaining 7th block no longer is.

        // Let's allocate two pools worth of chunks for a class of our choice

        for _ in 0..(BLOCK_SIZE/64-1)*2 {
            let layout = make_layout(64, 2);
            let ptr = unsafe { ALLOC.alloc(layout) };
            assert!(!ptr.is_null());

            ptrs.push((ptr, layout));
        }

        // This in turn allocates the remaining 7th block back, but now for our 64 byte pool

        // Still cannot allocate
        assert!(unsafe { ALLOC.alloc (make_layout(64, 2)) }.is_null());
        assert!(unsafe { ALLOC.alloc (make_layout(BLOCK_SIZE, 2)) }.is_null());

        // Let's now free all our pointers and check if it frees back the 7th block

        for (ptr, layout) in ptrs.drain(..) {
            unsafe { ALLOC.dealloc(ptr, layout) };
        }

        // Now this should succeed
        {
            let ptr = unsafe { ALLOC.alloc(make_layout(BLOCK_SIZE, 2)) };
            assert!(!ptr.is_null());
            unsafe { ALLOC.dealloc(ptr, make_layout(BLOCK_SIZE, 2));}
        }

        // Our final test will be about alignment. Our allocator should make sure that it allocates the greatest request (either based on size, or alignment)

        {
            // Request 2 bytes, but with a block size alignment
            let ptr = unsafe { ALLOC.alloc(make_layout(2, BLOCK_SIZE)) };
            assert!(!ptr.is_null());

            // Because our alignment was block-size wide, we allocated an entire block back. Cannot allocate anything else now
            assert!(unsafe { ALLOC.alloc (make_layout(BLOCK_SIZE, 2)) }.is_null());

            unsafe { ALLOC.dealloc(ptr, make_layout(2, BLOCK_SIZE));}
        }
    }
}