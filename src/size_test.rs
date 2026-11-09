use crate::NanAlloc;
use core::{
    alloc::{GlobalAlloc, Layout},
    ptr,
};

#[global_allocator]
static ALLOC: NanAlloc = NanAlloc::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn alloc(size: usize, align: usize) -> *mut u8 {
    if let Ok(layout) = Layout::from_size_align(size, align) {
        unsafe { ALLOC.alloc(layout) }
    } else {
        ptr::null_mut()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    if let Ok(layout) = Layout::from_size_align(size, align) {
        unsafe { ALLOC.dealloc(ptr, layout) };
    }
}

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn _panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
