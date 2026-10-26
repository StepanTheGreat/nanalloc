//! This module abstracts page allocation. 
//! On wasm it uses its actual operations like `memory_grow` and `memory_size`,
//! on any other environment it simulates wasm behaviour using an std allocator.

#[cfg(not(target_arch = "wasm32"))]
use core::{alloc::Layout, sync::atomic::{AtomicUsize, Ordering}};

#[cfg(target_arch = "wasm32")]
use core::arch::wasm32;

/// The index of the wasm memory (there theoretically can be multiple)
#[cfg(target_arch = "wasm32")]
const WASM_MEM_INDEX: u32 = 0;

/// The size of a single wasm page
pub const PAGE_SIZE: usize = 2usize.pow(16); 

/// The total amount of heap pages the testing environment can allocate
#[cfg(not(target_arch = "wasm32"))]
pub const TEST_MAX_HEAP_PAGES: usize = 128;

/// How many heap pages this environment has already given out
#[cfg(not(target_arch = "wasm32"))]
static TEST_HEAP_MEMORY_PAGES: AtomicUsize = AtomicUsize::new(0);

/// With how many pages this allocator starts
#[cfg(not(target_arch = "wasm32"))]
static TEST_BASE_MEMORY_PAGES: AtomicUsize = AtomicUsize::new(0);

/// A one-time initialization procedure that initializes simulated heap
#[cfg(not(target_arch = "wasm32"))]
fn test_init_heap() {
    if TEST_BASE_MEMORY_PAGES.load(Ordering::Relaxed) != 0 {
        return;
    }

    // Allocate our heap pointer
    let heap_ptr = unsafe { std::alloc::alloc(
        Layout::from_size_align(TEST_MAX_HEAP_PAGES*PAGE_SIZE, PAGE_SIZE).unwrap()
    )};

    // Make sure that our heap pointer is actually page aligned
    assert_eq!(heap_ptr as usize % PAGE_SIZE, 0);

    // Store the total amount of base pages
    TEST_BASE_MEMORY_PAGES.store((heap_ptr as usize) / PAGE_SIZE, Ordering::Relaxed);
}

/// Get the total amount of allocated pages
/// 
/// Equivalent to `wasm32::memory_size`
pub fn memory_size() -> usize {

    #[cfg(target_arch = "wasm32")]
    return wasm32::memory_size::<WASM_MEM_INDEX>();
    
    #[cfg(not(target_arch = "wasm32"))]
    {
        test_init_heap();

        let base_pages = TEST_BASE_MEMORY_PAGES.load(Ordering::Relaxed);
        let heap_pages = TEST_HEAP_MEMORY_PAGES.load(Ordering::Relaxed);

        return base_pages+heap_pages;
    }
}

/// Grow this memory by the provided amount of pages
/// 
/// If successful - returns `Some`, with the previous amount of pages.
/// 
/// Never fails when testing
pub fn memory_grow(pages: usize) -> Option<usize> {
    #[cfg(target_arch = "wasm32")]
    {
        return match wasm32::memory_grow::<WASM_MEM_INDEX>(pages) {
            usize::MAX => None,
            pages => Some(pages)
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        test_init_heap();

        let base_pages = TEST_BASE_MEMORY_PAGES.load(Ordering::Relaxed);
        let heap_pages = TEST_HEAP_MEMORY_PAGES.load(Ordering::Relaxed);

        // Don't exceed our limits
        if (heap_pages+pages) > TEST_MAX_HEAP_PAGES {
            return None;
        }

        // Add our new page delta 
        TEST_HEAP_MEMORY_PAGES.fetch_add(pages, Ordering::Relaxed);
        
        // Return the old amount of pages
        return Some(base_pages+heap_pages);
    }
}