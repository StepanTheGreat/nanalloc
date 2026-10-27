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

use core::{alloc::GlobalAlloc, ptr::null_mut, cell::{Cell, OnceCell}};

use crate::{
    buddytree::{BuddyNode, BuddyTree, BuddyTreePage}, 
    util::FixedStack, 
    wasm::{PAGE_SIZE, memory_grow, memory_size}
};

/// The total size of the heap. We're putting here a reasonable constant that both makes our implementation simple, 
const HEAP_SIZE: usize = 2usize.pow(30);

/// The size of a single allocatable memory block
const BLOCK_SIZE: usize = PAGE_SIZE/8; 

/// The total amount of blocks
const TOTAL_BLOCKS: usize = HEAP_SIZE / BLOCK_SIZE;

/// How many binary nodes in a tree. The total amount of lead nodes * 2 - 1
const TOTAL_NODES: usize = TOTAL_BLOCKS*2 - 1;

/// The amount of bitsets we need to store. A single block takes 2 bits, since it needs in total 3 values.
/// 
/// The total amount 
const TOTAL_BLOCK_BITSETS: usize = (TOTAL_NODES * 2).div_ceil(BuddyTreePage::BITS as usize);

/// We're using leading_zeros trick to constantly compute the total amount of layers.
/// 
/// Look for [layer_from_block_size] for a more detailed explanation
const TOTAL_TREE_LAYERS: usize = (BLOCK_SIZE.leading_zeros()-HEAP_SIZE.leading_zeros()+1) as usize;

/// Get block's layer index in the tree from its size. 
/// 
/// This will fail if: 
/// - The block is greater than our heap
/// - The block is smaller than a single block size
const fn layer_from_block_size(block: usize) -> Option<usize> {
    let block = block.next_power_of_two();

    if block < BLOCK_SIZE || block > HEAP_SIZE {
        return None;
    }

    // The way we extract our layer is by counting the amount of leading zeros relative to our heap
    // block. Because our blocks are powers of two, this offset translates naturally into layer index.
    let heap_zeros = HEAP_SIZE.leading_zeros();
    let block_zeros = block.leading_zeros();

    Some((block_zeros-heap_zeros) as usize)
}

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
    #[inline]
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
            let node_size = 2usize.pow(30-layer as u32);

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

    /// Try free the provided block of bytes
    #[inline]
    fn free_block(&self, ptr: *mut u8, bytes: usize) -> Option<()> {
        let po2_bytes = bytes.next_power_of_two().max(BLOCK_SIZE);
        
        if po2_bytes > HEAP_SIZE {
            // We couldn't possibly allocate more than our heap
            return None;
        } 

        let heap_start = self.heap_ptr.get().copied()?;

        if ptr < heap_start {
            // Our block start earlier than our heap????
            return None;
        }

        let block_tree = self.block_tree.get()?;

        // Compute the block index in its tree layer
        let block_ind = ((ptr as usize) - (heap_start as usize)) / po2_bytes;

        // Try extract this block's layer from its block size
        let block_layer = layer_from_block_size(po2_bytes)?;

        // Map it to actual coordinates
        let node_to_free = block_tree.node_from_local(block_layer, block_ind)?;
        
        // If it's actually full - free it
        if block_tree.get(node_to_free)? == BuddyNode::Full {
            block_tree.free(node_to_free);
            return Some(());
        }

        // In any other case do nothing
        None
    }

    #[inline]
    pub(crate) unsafe fn ex_alloc(&self, layout: core::alloc::Layout) -> Option<*mut u8> {
        if !self.initialize() {
            return None;
        } else if layout.align() > BLOCK_SIZE {
            // Our allocator only guarantees alignment up to 8KB
            return None;
        }

        // Try find a block or return a null pointer
        self.find_block(layout.size())
    }

    /// The only difference between [GlobalAlloc::dealloc] is that this returns [Option] that 
    /// signifies whether the deallocation was successful (only ever useful when testing)
    #[inline]
    pub(crate) unsafe fn ex_dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) -> Option<()> {
        if !self.initialize() {
            return None;
        } else if layout.align() > BLOCK_SIZE {
            // The same reason as for [alloc]
            return None;
        }

        // Try free the pointer if possible
        self.free_block(ptr, layout.size())
    }
    
}

unsafe impl GlobalAlloc for NanAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        unsafe { self.ex_alloc(layout).unwrap_or(null_mut()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        unsafe { self.ex_dealloc(ptr, layout) };
    }
}

#[cfg(test)]
mod tests {
    use crate::{NanAlloc, alloc::{BLOCK_SIZE, HEAP_SIZE, TOTAL_BLOCKS, TOTAL_TREE_LAYERS}};
    use core::{alloc::Layout, ptr::null_mut};

    // This macro only serves as a utility for tests. For each test we'll create a separate
    // static with a Send+Sync implementation. Why? Due to thread safe.
    macro_rules! init_alloc {
        () => {
            struct NanAllocSync(NanAlloc);

            unsafe impl Send for NanAllocSync {}
            unsafe impl Sync for NanAllocSync {}

            static ALLOC: NanAllocSync = NanAllocSync(NanAlloc::new());

            // Utility functions

            unsafe fn alloc(size: usize, align: usize) -> Option<*mut u8> {
                unsafe { 
                    ALLOC.0.ex_alloc(Layout::from_size_align(size, align).unwrap())
                }
            }

            unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) -> Option<()> {
                unsafe { 
                    ALLOC.0.ex_dealloc(ptr, Layout::from_size_align(size, align).unwrap())
                }
            }       
        };
    }

    /// Test basic allocator functionality
    #[test]
    fn test_basic_alloc() {
        init_alloc!();

        // We're going to allocate all of the blocks within our entire heap. Individually.
        let mut block_ptrs = std::vec![(null_mut::<u8>(), 0); TOTAL_BLOCKS];

        // This experiment is then repeated multiple times for stability.
        for _ in 0..4 {

            // Allocate EACH block from the heap
            for bind in 0..TOTAL_BLOCKS {
                // This pointer cannot be null!
                block_ptrs[bind] = (
                    unsafe { alloc(BLOCK_SIZE, 1) }.unwrap(), 
                    BLOCK_SIZE
                );    
            }

            // We no longer have any more blocks
            assert!(unsafe { alloc(BLOCK_SIZE, 1) }.is_none());

            // Let's free all of them now
            for bind in 0..TOTAL_BLOCKS {
                unsafe { dealloc(block_ptrs[bind].0, block_ptrs[bind].1, 1) }.unwrap();
            }

            // Zero all our pointers
            block_ptrs.fill((null_mut(), 0));
        }

        // We'll take all blocks again
        for bind in 0..TOTAL_BLOCKS {
            // This pointer cannot be null!
            block_ptrs[bind] = (
                unsafe { alloc(BLOCK_SIZE, 1) }.unwrap(), 
                BLOCK_SIZE
            );

        }
        
        // But this time we'll try to allocate one large block instead
        assert!(unsafe { alloc(BLOCK_SIZE*2, 1) }.is_none());

        // Free our previous two
        unsafe { dealloc(block_ptrs[0].0, block_ptrs[0].1, 1).unwrap() };
        unsafe { dealloc(block_ptrs[1].0, block_ptrs[1].1, 1).unwrap() };

        // Allocate one larger block instead
        block_ptrs[0] = (null_mut(), 0);
        block_ptrs[1] = (
            unsafe { alloc(BLOCK_SIZE*2, 1)}.unwrap(), 
            BLOCK_SIZE*2
        );

        // Memory is still perfectly taken 
        assert!(unsafe { alloc(BLOCK_SIZE*2, 1) }.is_none());

        // Deallocate all our blocks again
        for bind in 0..TOTAL_BLOCKS {
            if !block_ptrs[bind].0.is_null() {
                unsafe { dealloc(block_ptrs[bind].0, block_ptrs[bind].1, 1) }.unwrap();
            }
        }
        block_ptrs.fill((null_mut(), 0));
    }

    /// Test various allocator properties
    #[test]
    fn test_alloc_props() {
        init_alloc!();

        // Let's test alignment first

        // The maximum allocatable amount is equal to our heap
        assert!(unsafe { alloc(HEAP_SIZE+1, BLOCK_SIZE*2) }.is_none());

        // This however will work perfectly fine
        unsafe {
            let ptr = alloc(HEAP_SIZE, BLOCK_SIZE).unwrap();
            dealloc(ptr, HEAP_SIZE, BLOCK_SIZE).unwrap();
        }

        // Will fail, since our allocator only guarantees layout up to 8KB
        assert!(unsafe { alloc(BLOCK_SIZE, BLOCK_SIZE*2) }.is_none());

        // But when it's less or equal to block size = it's fine.
        unsafe {
            let ptr = alloc(BLOCK_SIZE, BLOCK_SIZE).unwrap();
            dealloc(ptr, BLOCK_SIZE, BLOCK_SIZE).unwrap();
        }


    }

    /// We're going to split our tree into two parts:
    /// 1 will allocate from bottom to the top
    /// 2 will allocate from top to the bottom
    /// 
    /// The allocator should easily handle various sizes
    #[test]
    fn test_recursive_alloc() {
        init_alloc!();

        let mut left = Vec::new();
        let mut right = Vec::new();

        let mut layer_left = 0;
        let mut layer_right = TOTAL_TREE_LAYERS as u32-2;

        loop {
            let block_size_left = BLOCK_SIZE * 2usize.pow((TOTAL_TREE_LAYERS as u32-2)-layer_left);
            let block_size_right = BLOCK_SIZE * 2usize.pow((TOTAL_TREE_LAYERS as u32-2)-layer_right);

            let blocks_left = (HEAP_SIZE/2)/block_size_left;
            let blocks_right = (HEAP_SIZE/2)/block_size_right;

            // Allocate the entire left part
            for _ in 0..blocks_left {
                left.push(unsafe { alloc(block_size_left, 1) }.unwrap());
            }

            // Right part
            for _ in 0..blocks_right {
                right.push(unsafe { alloc(block_size_right, 1) }.unwrap());
            }

            // Free everything on the left
            while let Some(ptr_left) = left.pop() {
                unsafe { dealloc(ptr_left, block_size_left, 1)}.unwrap();
            }

            // On the right as well
            while let Some(ptr_right) = right.pop() {
                unsafe { dealloc(ptr_right, block_size_right, 1)}.unwrap();
            }

            // Increment indices
            if layer_right > 0 {
                layer_right -= 1;
                layer_left += 1;
            } else {
                break;
            }
        }
    }
}