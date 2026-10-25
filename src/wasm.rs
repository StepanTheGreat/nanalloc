
#[cfg(not(target_arch = "wasm32"))]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(target_arch = "wasm32")]
use core::arch::wasm32;

use core::usize;

/// The index of the wasm memory (there theoretically can be multiple)
const WASM_MEM_INDEX: u32 = 0;

/// The size of a single wasm page
pub const PAGE_SIZE: usize = 2usize.pow(16); 

#[cfg(not(target_arch = "wasm32"))]
static MEMORY_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Get the total amount of allocated pages
/// 
/// Equivalent to `wasm32::memory_size`
pub fn memory_size() -> usize {

    #[cfg(target_arch = "wasm32")]
    return wasm32::memory_size::<WASM_MEM_INDEX>();
    
    #[cfg(not(target_arch = "wasm32"))]
    return MEMORY_PAGES.load(Ordering::Relaxed);
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
        return Some(
            MEMORY_PAGES.fetch_add(pages, Ordering::Relaxed)
        );
    }
}