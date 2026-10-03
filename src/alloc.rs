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

use core::{alloc::GlobalAlloc, arch::wasm32::{self, memory_size}, cell::Cell, ptr::null_mut};

use crate::util::FixedStack;

/// The index of wasm memory
const WASM_MEM_INDEX: u32 = 0;

/// The size of a single wasm page
const PAGE_SIZE: usize = 2usize.pow(16); 

/// The total size of the heap. We're putting here a reasonable constant that both makes our implementation simple, 
const HEAP_SIZE: usize = 2usize.pow(31);

/// The size of a single allocatable memory block
const BLOCK_SIZE: usize = PAGE_SIZE/8; 

/// The total amount of blocks
const TOTAL_BLOCKS: usize = HEAP_SIZE / BLOCK_SIZE;

/// The amount of bitsets we need to store. A single blocks takes 2 bits, since it needs in total 3 values.
const TOTAL_BLOCK_BITSETS: usize = (TOTAL_BLOCKS * 2).div_ceil(u32::BITS as usize);

/// A block node that supports 3 states and tightly packed as a 2bit integer
enum BlockNode {
    BothFree,
    OneFree,
    NoneFree
}

/// An ultra minimal allocator
pub struct Allocator {
    /// The pointer to our block tre
    block_tree_ptr: Cell<Option<*mut [u32; TOTAL_BLOCK_BITSETS]>>,
    
    /// The pointer to our usable heap
    heap_ptr: Cell<Option<*mut u8>>,
    
    /// How many pages were already allocated
    pages_allocated: Cell<usize>,

    /// Was this allocator initialized? If not, it's unusable
    initialized: Cell<bool>
}

impl Allocator {
    fn new() -> Self {
        Self {
            block_tree_ptr: Cell::new(None),
            heap_ptr: Cell::new(None),
            pages_allocated: Cell::new(0),
            initialized: Cell::new(false)
        }
    }

    fn alloc_pages(&self, pages: usize) -> Option<*mut u8> {
        if pages == 0 {
            return None;
        }

        // Make a pointer to the edge 
        let ptr = (wasm32::memory_size::<WASM_MEM_INDEX>() * PAGE_SIZE) as *mut u8;

        // If successful - return a pointer    
        match wasm32::memory_grow::<WASM_MEM_INDEX>(pages) {
            usize::MAX => None,
            _ => {
                self.pages_allocated.set(self.pages_allocated.get() + pages);
                Some(ptr)
            },
        }
    }

    /// Initialize and return the status of initialization
    fn initialize(&self) -> bool {
        if self.initialized.get() {
            return true;
        }

        if let Some(block_page_ptr) = self.alloc_pages(1) {
            let block_page_ptr = block_page_ptr as *mut [u32; TOTAL_BLOCK_BITSETS];
            let heap_ptr = (memory_size::<0>() * PAGE_SIZE) as *mut u8;

            // Fill everything with zeros
            unsafe { &mut *block_page_ptr }.fill(0);
            
            // Set our block tree
            self.block_tree_ptr.set(Some(block_page_ptr));
            self.heap_ptr.set(Some(heap_ptr));
        } else {
            return false;
        }

        self.initialized.set(true);
        true
    }

    fn get_node(&self, node: usize) -> Option<BlockNode> {
        if !self.initialize() {
            return None;
        } else if node == 0 || node > TOTAL_BLOCKS {
            return None
        }

        let bitset_ind = node / 16;
        let bit_ind = (node % 16) * 2;
    }

    fn find_block(bytes: usize) -> Option<*mut u8> {
        let po2_bytes = bytes.next_power_of_two().max(BLOCK_SIZE);
    
        if po2_bytes > HEAP_SIZE {
            return None;
        } 

        // A stack of (parent ID, direction to process Some(left = true, false = false))
        let mut node_stack = FixedStack::new((0usize, Some(true)));
        
        // 0 node is ignored in our tree
        node_stack.push((1, Some(true)));
        
        while let Some((node, to_process)) = node_stack.pop() {
            
            // Get the layer of this node in the tree
            let layer = (node as f32).log2() as usize;

            // It's block-size
            let block_size = 2.pow(31-layer);
            
            if po2_bytes > block_size {
                
            }
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        if !self.initialize() {
            return null_mut();
        }

        null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if !self.initialize() {
            return;
        }
    }
}