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

use core::cell::{Cell, OnceCell};

use crate::{
    alloc::{BLOCK_SIZE, HEAP_SIZE}, buddytree::{BuddyNode, BuddyTree, BuddyTreePage}, util::FixedStack, wasm::{PAGE_SIZE, memory_grow, memory_size},
};

/// The total amount of blocks
const TOTAL_BLOCKS: usize = HEAP_SIZE / BLOCK_SIZE;

/// How many binary nodes in a tree. The total amount of lead nodes * 2 - 1
const TOTAL_NODES: usize = TOTAL_BLOCKS * 2 - 1;

/// The amount of bitsets we need to store. A single block takes 2 bits, since it needs in total 3 values.
///
/// The total amount
const TOTAL_BLOCK_BITSETS: usize = (TOTAL_NODES * 2).div_ceil(BuddyTreePage::BITS as usize);

/// We're using leading_zeros trick to constantly compute the total amount of layers.
///
/// Look for [layer_from_block_size] for a more detailed explanation
const TOTAL_TREE_LAYERS: usize =
    (BLOCK_SIZE.leading_zeros() - HEAP_SIZE.leading_zeros() + 1) as usize;

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

    Some((block_zeros - heap_zeros) as usize)
}

/// An ultra minimal block allocator
///
/// Some properties and quirks:
/// - Has a maximum heap size of 2^31 bytes
/// - Allocates memory in blocks of 8192 bytes (1/8 of a single wasm page)
/// - Only guarantees alignment for up to 8192 bytes (doesn't fail to allocate though).
pub struct BlockAllocator {
    /// The pointer to our block tree
    block_tree: OnceCell<BuddyTree<TOTAL_BLOCK_BITSETS>>,

    /// The pointer to our usable heap
    heap_ptr: OnceCell<*mut u8>,

    /// How many pages were already allocated
    pages_allocated: Cell<usize>,

    /// Was this allocator initialized? If not, it's unusable
    initialized: Cell<bool>,
}

impl Default for BlockAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockAllocator {
    pub const fn new() -> Self {
        Self {
            block_tree: OnceCell::new(),
            heap_ptr: OnceCell::new(),
            pages_allocated: Cell::new(0),
            initialized: Cell::new(false),
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
            }
        }
    }

    /// Initialize and return the status of initialization
    fn initialize(&self) -> bool {
        if self.initialized.get() {
            return true;
        }

        if let Some(block_tree_ptr) = self.alloc_pages(1) {
            // Our usable heap starts one page to the right
            let heap_ptr = unsafe { block_tree_ptr.add(PAGE_SIZE) };

            let block_tree_ptr = block_tree_ptr as *mut u32;

            // Initialize our block tree
            let _ = self
                .block_tree
                .set(unsafe { BuddyTree::<TOTAL_BLOCK_BITSETS>::new(block_tree_ptr) });
            let _ = self.heap_ptr.set(heap_ptr);
        } else {
            return false;
        }

        self.initialized.set(true);
        true
    }

    /// Prealocate pages to satisfy a block request
    unsafe fn preallocate_pages_for(&self, node_ind: usize, node_size: usize) -> Option<*mut u8> {
        // Get our heap pointer
        let heap_ptr: *mut u8 = self.heap_ptr.get().copied()?;

        // How much to step from the heap to access our node
        let node_offset = node_ind * node_size;

        // Compute the required egde (node pointer + its size)
        let required_end = (heap_ptr as usize + node_offset) + node_size;

        // Compute the total allocated by us end
        let allocated_end = heap_ptr as usize + self.pages_allocated.get() * PAGE_SIZE;

        // Already satisfied
        if required_end < allocated_end {
            return Some(unsafe { heap_ptr.add(node_offset) });
        }

        // Compute the amount of pages we might need
        let req_pages = (required_end - allocated_end).div_ceil(PAGE_SIZE);
        if req_pages > 0 {
            self.alloc_pages(req_pages)?;
        }

        Some(unsafe { heap_ptr.add(node_offset) })
    }

    /// Find a block that can satisfy the requested amount of bytes
    #[inline]
    pub fn find_block(&self, bytes: usize) -> Option<*mut u8> {
        if !self.initialize() {
            return None;
        }

        let po2_bytes = bytes.next_power_of_two().max(BLOCK_SIZE);

        if po2_bytes > HEAP_SIZE {
            return None;
        }

        let block_tree = self.block_tree.get()?;

        // A stack of (parent ID, direction to process Some(left = true, false = false))
        let mut node_stack: FixedStack<(usize, Option<bool>), TOTAL_TREE_LAYERS> =
            FixedStack::new((0usize, Some(true)));

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
            let node_size = 2usize.pow(30 - layer as u32);

            match block_tree.get(node)? {
                BuddyNode::Full => continue,
                BuddyNode::Partial => {}
                BuddyNode::Free => {
                    // We found the perfect block
                    if node_size == po2_bytes {
                        // Let's try to preallocate pages for it first. If this operation fails - we cannot procede further.
                        let node_ptr = unsafe { self.preallocate_pages_for(layer_ind, node_size)? };
                        block_tree.occupy(node);
                        return Some(node_ptr);
                    }
                }
            }

            // Useless afterwards, we need our specific node size
            if node_size < po2_bytes {
                continue;
            }

            // If we have children - let's push them
            if let Some((lchild, rchild)) = block_tree.children(node) {
                // Which child to push
                let child_to_push = if process_left_child { lchild } else { rchild };

                // Push ourselves once more, but now for the right child
                node_stack.push((
                    node,
                    if process_left_child {
                        Some(false)
                    } else {
                        None
                    },
                ));

                // Push our child
                node_stack.push((child_to_push, Some(true)));
            }
        }

        // In any other case we cannot satisfy the request
        None
    }

    /// Try free the provided block of bytes
    #[inline]
    pub fn free_block(&self, ptr: *mut u8, bytes: usize) -> Option<()> {
        if !self.initialize() {
            return None;
        }

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
        let block_ind = ((ptr as usize) - (heap_start as usize)).checked_div_euclid(po2_bytes).unwrap_or(0);

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
}

#[cfg(target_arch = "wasm32")]
unsafe impl Send for BlockAllocator {}

#[cfg(target_arch = "wasm32")]
unsafe impl Sync for BlockAllocator {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use crate::{alloc::BLOCK_SIZE, wasm::{PAGE_SIZE, TEST_MAX_HEAP_PAGES, TestHeapHandle}};
    use super::BlockAllocator;
    use core::ptr;

    /// Because we cannot sacrifice 1GB of RAM for our tests
    const LOCAL_HEAP_SIZE: usize = TEST_MAX_HEAP_PAGES * PAGE_SIZE;

    const LOCAL_TOTAL_BLOCKS: usize = LOCAL_HEAP_SIZE / BLOCK_SIZE;

    /// How many local tree layers we got
    const LOCAL_TOTAL_TREE_LAYERS: usize =
        (BLOCK_SIZE.leading_zeros() - LOCAL_HEAP_SIZE.leading_zeros() + 1) as usize;

    thread_local! {
        static ALLOC: BlockAllocator = BlockAllocator::new();
    }

    unsafe fn alloc(size: usize) -> Option<*mut u8> {
        ALLOC.with(|a| a.find_block(size))
    }

    unsafe fn dealloc(ptr: *mut u8, size: usize) -> Option<()> {
        ALLOC.with(|a| a.free_block(ptr, size))
    }

    /// Test basic allocator functionality
    #[test]
    #[cfg_attr(miri, ignore = "heavy for miri")]
    fn test_basic_block_alloc() {
        let _heap = TestHeapHandle::new();

        // We're going to allocate all of the blocks within our entire heap. Individually.
        let mut block_ptrs = std::vec![(ptr::null_mut::<u8>(), 0); LOCAL_TOTAL_BLOCKS];

        const ITER_COUNT: usize = 4;

        // This experiment is then repeated multiple times for stability.
        for _ in 0..ITER_COUNT {
            // Allocate EACH block from the heap
            for bind in 0..LOCAL_TOTAL_BLOCKS {
                // This pointer cannot be null!
                block_ptrs[bind] = (unsafe { alloc(BLOCK_SIZE) }.unwrap(), BLOCK_SIZE);
            }

            // We no longer have any more blocks
            assert!(unsafe { alloc(BLOCK_SIZE) }.is_none());

            // Let's free all of them now
            for bind in 0..LOCAL_TOTAL_BLOCKS {
                unsafe { dealloc(block_ptrs[bind].0, block_ptrs[bind].1) }.unwrap();
            }

            // Zero all our pointers
            block_ptrs.fill((ptr::null_mut(), 0));
        }

        // We'll take all blocks again
        for bind in 0..LOCAL_TOTAL_BLOCKS {
            // This pointer cannot be null!
            block_ptrs[bind] = (unsafe { alloc(BLOCK_SIZE) }.unwrap(), BLOCK_SIZE);
        }

        // But this time we'll try to allocate one large block instead
        assert!(unsafe { alloc(BLOCK_SIZE * 2) }.is_none());

        // Free our previous two
        unsafe { dealloc(block_ptrs[0].0, block_ptrs[0].1).unwrap() };
        unsafe { dealloc(block_ptrs[1].0, block_ptrs[1].1).unwrap() };

        // Allocate one larger block instead
        block_ptrs[0] = (ptr::null_mut(), 0);
        block_ptrs[1] = (unsafe { alloc(BLOCK_SIZE * 2) }.unwrap(), BLOCK_SIZE * 2);

        // Memory is still perfectly taken
        assert!(unsafe { alloc(BLOCK_SIZE * 2) }.is_none());

        // Deallocate all our blocks again
        for bind in 0..LOCAL_TOTAL_BLOCKS {
            if !block_ptrs[bind].0.is_null() {
                unsafe { dealloc(block_ptrs[bind].0, block_ptrs[bind].1) }.unwrap();
            }
        }

        block_ptrs.fill((ptr::null_mut(), 0));
    }

    /// Test various allocator properties
    #[test]
    fn test_block_alloc_props() {
        let _heap = TestHeapHandle::new();

        // Let's test alignment first

        // The maximum allocatable amount is equal to our heap
        assert!(unsafe { alloc(LOCAL_HEAP_SIZE + 1) }.is_none());

        // This however will work perfectly fine
        unsafe {
            let ptr = alloc(LOCAL_HEAP_SIZE).unwrap();
            dealloc(ptr, LOCAL_HEAP_SIZE).unwrap();
        }
    }

    /// We're going to split our tree into two parts:
    /// 1 will allocate from bottom to the top
    /// 2 will allocate from top to the bottom
    ///
    /// The allocator should easily handle various sizes
    #[test]
    #[cfg_attr(miri, ignore = "heavy for miri")]
    fn test_recursive_block_alloc() {
        let _heap = TestHeapHandle::new();

        let mut left = Vec::new();
        let mut right = Vec::new();

        let mut layer_left = 0;
        let mut layer_right = LOCAL_TOTAL_TREE_LAYERS as u32 - 2;

        loop {
            let block_size_left =
                BLOCK_SIZE * 2usize.pow((LOCAL_TOTAL_TREE_LAYERS as u32 - 2) - layer_left);
            let block_size_right =
                BLOCK_SIZE * 2usize.pow((LOCAL_TOTAL_TREE_LAYERS as u32 - 2) - layer_right);

            let blocks_left = (LOCAL_HEAP_SIZE / 2) / block_size_left;
            let blocks_right = (LOCAL_HEAP_SIZE / 2) / block_size_right;

            // Allocate the entire left part
            for _ in 0..blocks_left {
                left.push(unsafe { alloc(block_size_left) }.unwrap());
            }

            // Right part
            for _ in 0..blocks_right {
                right.push(unsafe { alloc(block_size_right) }.unwrap());
            }

            // Free everything on the left
            while let Some(ptr_left) = left.pop() {
                unsafe { dealloc(ptr_left, block_size_left) }.unwrap();
            }

            // On the right as well
            while let Some(ptr_right) = right.pop() {
                unsafe { dealloc(ptr_right, block_size_right) }.unwrap();
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

    /// Test basic allocator functionality with miri
    #[test]
    fn test_basic_block_alloc_miri() {
        let _heap = TestHeapHandle::new();

        unsafe {
            // 1/2 of our heap
            let ptr1 = alloc(LOCAL_HEAP_SIZE / 2).unwrap();

            // 1/4 of our heap
            let ptr2 = alloc(LOCAL_HEAP_SIZE / 4).unwrap();

            // 1/8 of our heap
            let ptr3 = alloc(LOCAL_HEAP_SIZE / 8).unwrap();

            // 1/8 of our heap
            let ptr4 = alloc(LOCAL_HEAP_SIZE / 8).unwrap();

            // In total 1

            // No more available blocks
            assert!(alloc(BLOCK_SIZE).is_none());

            // Deallocate our last to pointers
            dealloc(ptr3, LOCAL_HEAP_SIZE / 8).unwrap();

            dealloc(ptr4, LOCAL_HEAP_SIZE / 8).unwrap();

            // This should in turn free some place for an another quarter
            let ptr3 = alloc(LOCAL_HEAP_SIZE / 4).unwrap();

            // Same situation
            assert!(alloc(BLOCK_SIZE).is_none());

            // Free those again
            dealloc(ptr2, LOCAL_HEAP_SIZE / 4).unwrap();
            dealloc(ptr3, LOCAL_HEAP_SIZE / 4).unwrap();

            // Now we can take the remaining half

            let ptr2 = alloc(LOCAL_HEAP_SIZE / 2).unwrap();

            assert!(alloc(BLOCK_SIZE).is_none());

            // Free those
            dealloc(ptr1, LOCAL_HEAP_SIZE / 2).unwrap();
            dealloc(ptr2, LOCAL_HEAP_SIZE / 2).unwrap();

            // And take the remaining heap!
            let ptr1 = alloc(LOCAL_HEAP_SIZE).unwrap();

            assert!(alloc(BLOCK_SIZE).is_none());

            dealloc(ptr1, LOCAL_HEAP_SIZE).unwrap();
        }
    }
}
