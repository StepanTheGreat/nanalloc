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

#[cfg(not(target_arch = "wasm32"))]
use core::alloc::Layout;
use core::{alloc::GlobalAlloc, cell::{Cell, OnceCell}, ptr::null_mut};

use crate::{
    buddytree::{BuddyNode, BuddyTree, BuddyTreePage}, 
    util::FixedStack, 
    wasm::{PAGE_SIZE, memory_grow, memory_size}
};

/// The total size of the heap. We're putting here a reasonable constant that both makes our implementation simple, 
const HEAP_SIZE: usize = 2usize.pow(31);

/// The size of a single allocatable memory block
const BLOCK_SIZE: usize = PAGE_SIZE/4; 

/// The total amount of blocks
const TOTAL_BLOCKS: usize = HEAP_SIZE / BLOCK_SIZE;

/// How many binary nodes in a tree. The total amount of lead nodes * 2 - 1
const TOTAL_NODES: usize = TOTAL_BLOCKS*2 - 1;

/// The amount of bitsets we need to store. A single block takes 2 bits, since it needs in total 3 values.
/// 
/// The total amount 
const TOTAL_BLOCK_BITSETS: usize = (TOTAL_NODES * 2).div_ceil(BuddyTreePage::BITS as usize);

/// This is a hardcoded value, since log2 isn't const-friendly. Essentially it's just `TOTAL_BLOCKS.log2()`
const TOTAL_TREE_LAYERS: usize = 18;

/// An ultra minimal allocator
/// 
/// Some properties and quirks:
/// - Has a maximum heap size of 2^31 bytes
/// - Allocates memory in blocks of 8192 bytes (1/8 of a single wasm page)
/// - Only guarantees alignment for up to 8192 bytes (doesn't fail to allocate though).
pub struct NanAlloc {
    /// The pointer to our block tree
    block_tree: OnceCell<BuddyTree<TOTAL_BLOCK_BITSETS>>,
    
    /// The pointer to our usable heap
    heap_ptr: OnceCell<*mut u8>,
    
    /// How many pages were already allocated
    pages_allocated: Cell<usize>,

    /// Was this allocator initialized? If not, it's unusable
    initialized: Cell<bool>
}

// No threading on wasm
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for NanAlloc {}

// Same thing
#[cfg(target_arch = "wasm32")]
unsafe impl Send for NanAlloc {}

impl NanAlloc {
    pub const fn new() -> Self {
        Self {
            block_tree: OnceCell::new(),
            heap_ptr: OnceCell::new(),
            pages_allocated: Cell::new(0),
            initialized: Cell::new(false)
        }
    }

    fn alloc_pages(&self, pages: usize) -> Option<*mut u8> {
        if pages == 0 {
            return None;
        }

        // Make a pointer to the edge 
        let ptr = (memory_size() * PAGE_SIZE) as *mut u8;

        // If successful - return a pointer
        match memory_grow(pages) {
            None => None,
            Some(_) => {
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

        if let Some(block_tree_ptr) = self.alloc_pages(1) {

            // We're doing this for testing purposes ONLY - we need an actual address where 
            // we can store our metadata.
            #[cfg(not(target_arch = "wasm32"))]
            let block_tree_ptr = unsafe {
                std::alloc::alloc(Layout::from_size_align(PAGE_SIZE, 2).unwrap())
            };

            let block_tree_ptr = block_tree_ptr as *mut u32;
            let heap_ptr = (memory_size() * PAGE_SIZE) as *mut u8;
            
            // Initialize our block tree
            let _ = self.block_tree.set(unsafe { BuddyTree::<TOTAL_BLOCK_BITSETS>::new(block_tree_ptr) });
            let _ = self.heap_ptr.set(heap_ptr);
        } else {
            return false;
        }

        self.initialized.set(true);
        true
    }

    /// Prealocate pages to satisfy a block request
    fn preallocate_pages_for(&self, ptr: *const u8, size: usize) -> Option<()> {
        let heap_start = self.heap_ptr.get().copied()? as *const u8;

        // Already satisfied
        if heap_start >= ptr {
            return Some(());
        }

        // Compute the amount of pages we might need
        let req_pages = ((ptr as usize + size) - (heap_start as usize)).div_ceil(PAGE_SIZE);
        if req_pages > 0 {
            self.alloc_pages(req_pages)?;
        }

        Some(())
    }

    /// Find a block that can satisfy the requested amount of bytes 
    fn find_block(&self, bytes: usize) -> Option<*mut u8> {
        let po2_bytes = bytes.next_power_of_two().max(BLOCK_SIZE);
    
        if po2_bytes > HEAP_SIZE {
            return None;
        } 

        let block_tree = self.block_tree.get()?;

        // A stack of (parent ID, direction to process Some(left = true, false = false))
        let mut node_stack: FixedStack<(usize, Option<bool>), TOTAL_TREE_LAYERS> = FixedStack::new((0usize, Some(true)));
        
        node_stack.push((0, Some(true)));
        
        while let Some((node, to_process)) = node_stack.pop() {

            // Whether we're processing left or right child
            let process_left_child = if let Some(left_child) = to_process {
                left_child
            } else {
                continue;
            };
            
            // Get the layer of this node in the tree
            let layer = block_tree.layer(node)?;
            let layer_ind = block_tree.layer_ind(node)?;

            // The total size of this node
            let node_size = 2usize.pow(31-layer as u32);

            // Get the node pointer that we can return
            let node_ptr = unsafe { self.heap_ptr.get()?.add(node_size * layer_ind) };

            match block_tree.get(node)? {
                BuddyNode::Full => continue,
                BuddyNode::Partial => {},
                BuddyNode::Free => {
                    if node_size == po2_bytes {
                        // We found the perfect block - take it!
                        block_tree.occupy(node);
                        self.preallocate_pages_for(node_ptr, node_size);
                        return Some(node_ptr);
                    }
                }
            }

            // If we have children - let's push them
            if let Some((lchild, rchild)) = block_tree.children(node) {
                
                // Which child to push
                let child_to_push = if process_left_child { lchild } else { rchild };
                
                // Push ourselves once more, but now for the right child
                node_stack.push((node, if process_left_child { Some(false) } else { None }));
                
                // Push our child
                node_stack.push((child_to_push, Some(true)));
            }

        }

        // In any other case we cannot satisfy the request
        None
    }

    fn free_block(&self, ptr: *mut u8) -> Option<()> {
        let heap_start = self.heap_ptr.get().copied()?;

        if ptr < heap_start {
            // Our block start earlier than our heap????
            return None;
        }

        let block_tree = self.block_tree.get()?;

        // No matter which block we take in the tree, there always exists a leaf block with the same position.
        // Thanks to the way we track taken blocks: we're going to traverse our tree from its bottom to the top; the moment we
        // encounter an occupied parent - we found the block that was actually taken (children are not marked as occupied when their parents are). 
        let leaf_block_ind = ((ptr as usize) - (heap_start as usize)) / BLOCK_SIZE;

        // Find the bottom most leaf node
        let mut current_node = block_tree.node_from_local(TOTAL_TREE_LAYERS-1, leaf_block_ind);


        while let Some(node) = current_node {
            let status = block_tree.get(node)?;
            // That's our node! Free it! 
            if status == BuddyNode::Full {
                block_tree.free(node);
                return Some(());
            } else {
                // In any other case get its parent and continue
                current_node = block_tree.parent(node);
            }
        }

        // If there's actually no full parent - that's probably a user bug.
        None
    }
}

unsafe impl GlobalAlloc for NanAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        if !self.initialize() {
            return null_mut();
        }

        // Try find a block or return a null pointer
        self.find_block(layout.size()).unwrap_or(null_mut())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if !self.initialize() {
            return;
        }

        // Try free the pointer if possible
        let _ = self.free_block(ptr);
    }
}

#[cfg(test)]
mod tests {
    use crate::{NanAlloc, alloc::{BLOCK_SIZE, TOTAL_BLOCKS}};
    use core::{alloc::{GlobalAlloc, Layout}, ptr::null_mut};

    macro_rules! init_alloc {
        () => {
            struct NanAllocSync(NanAlloc);

            unsafe impl Send for NanAllocSync {}
            unsafe impl Sync for NanAllocSync {}

            static ALLOC: NanAllocSync = NanAllocSync(NanAlloc::new());

            // Utility functions

            unsafe fn alloc(size: usize) -> *mut u8 {
                unsafe { 
                    ALLOC.0.alloc(Layout::from_size_align(size, 2).unwrap())
                }
            }

            unsafe fn dealloc(ptr: *mut u8) {
                unsafe { 
                    ALLOC.0.dealloc(ptr, Layout::from_size_align(2, 2).unwrap())
                }
            }       
        };
    }

    /// Test basic allocator functionality
    #[test]
    fn test_basic_alloc() {
        init_alloc!();

        // We're going to allocate all of the blocks within our entire heap. Individually.
        let mut block_ptrs = std::vec![null_mut::<u8>(); TOTAL_BLOCKS];

        // This experiment is then repeated multiple times for stability.
        for _ in 0..4 {

            // Allocate EACH block from the heap
            for bind in 0..TOTAL_BLOCKS {
                block_ptrs[bind] = unsafe { alloc(BLOCK_SIZE) };
    
                // This pointer cannot be null!
                assert!(!block_ptrs[bind].is_null());
            }

            // We no longer have any more blocks
            assert!(unsafe { alloc(BLOCK_SIZE) }.is_null());

            // Let's free all of them now
            for bind in 0..TOTAL_BLOCKS {
                unsafe { dealloc(block_ptrs[bind]) };
            }

            // Zero all our pointers
            block_ptrs.fill(null_mut());
        }
        
    }
}