//! This module abstracts page allocation.
//! On wasm it uses its actual operations like `memory_grow` and `memory_size`,
//! on any other environment it simulates wasm behaviour using an std allocator.

#[cfg(not(target_arch = "wasm32"))]
use core::{alloc::Layout, cell::Cell};

#[cfg(target_arch = "wasm32")]
use core::arch::wasm32;

/// The index of the wasm memory (there theoretically can be multiple)
#[cfg(target_arch = "wasm32")]
const WASM_MEM_INDEX: u32 = 0;

/// The size of a single wasm page
pub const PAGE_SIZE: usize = 2usize.pow(16);

/// The total amount of heap pages the testing environment can allocate. An additional page is given exclusively for the tree itself
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const TEST_MAX_HEAP_PAGES: usize = 512;

/// A test heap handle for tests. It automatically allocates and deallocates test heap.
#[cfg(not(target_arch = "wasm32"))]
pub struct TestHeapHandle(Layout);

#[cfg(not(target_arch = "wasm32"))]
impl TestHeapHandle {
    pub fn new() -> Self {
        if TEST_BASE_MEMORY_PAGES.with(|p| p.get()) != 0 {
            panic!("Already initialized heap");
        }

        // We're allocating one more page for our allocator state. This is a more of a fix to the fact that our
        // allocator doesn't enforce any page limits. Ideally it expects the entire heap + a single heap for itself.
        let layout =
            Layout::from_size_align((TEST_MAX_HEAP_PAGES + 1) * PAGE_SIZE, PAGE_SIZE).unwrap();

        // Allocate our heap pointer
        let heap_ptr = unsafe { std::alloc::alloc(layout) };

        // Make sure that our heap pointer is actually page aligned
        assert_eq!(heap_ptr as usize % PAGE_SIZE, 0);

        // Store the total amount of base pages
        TEST_BASE_MEMORY_PAGES.with(|p| p.set((heap_ptr as usize) / PAGE_SIZE));

        Self(layout)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TestHeapHandle {
    fn drop(&mut self) {
        // A cleanup procedure that should be called at the end of each test. Simply deallocates our pages
        let ptr = TEST_BASE_MEMORY_PAGES.with(|p| p.get() * PAGE_SIZE) as *mut u8;

        unsafe {
            std::alloc::dealloc(ptr, self.0);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// How many heap pages this environment has already given out
    static TEST_HEAP_MEMORY_PAGES: Cell<usize> = const { Cell::new(0) };

    /// With how many pages this allocator starts
    static TEST_BASE_MEMORY_PAGES: Cell<usize> = const { Cell::new(0) };
}

/// Get the total amount of allocated pages
///
/// Equivalent to `wasm32::memory_size`
pub fn memory_size() -> usize {
    #[cfg(target_arch = "wasm32")]
    return wasm32::memory_size::<WASM_MEM_INDEX>();

    #[cfg(not(target_arch = "wasm32"))]
    {
        let base_pages = TEST_BASE_MEMORY_PAGES.with(|p| p.get());
        let heap_pages = TEST_HEAP_MEMORY_PAGES.with(|p| p.get());

        assert_ne!(base_pages, 0, "Unitialized test heap");

        base_pages + heap_pages
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
            pages => Some(pages),
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let base_pages = TEST_BASE_MEMORY_PAGES.with(|p| p.get());
        let heap_pages = TEST_HEAP_MEMORY_PAGES.with(|p| p.get());

        assert_ne!(base_pages, 0, "Unitialized test heap");

        // Don't exceed our limits
        if (heap_pages + pages) > TEST_MAX_HEAP_PAGES {
            return None;
        }

        // Add our new page delta
        TEST_HEAP_MEMORY_PAGES.with(|p| p.set(p.get() + pages));

        // Return the old amount of pages
        Some(base_pages + heap_pages)
    }
}
