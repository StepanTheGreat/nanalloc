use core::{alloc::{GlobalAlloc, Layout}, ptr::null_mut};
use crate::NanAlloc;

#[global_allocator]
static ALLOC: NanAlloc = NanAlloc::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn alloc(size: usize, align: usize) -> *mut u8 {
    if let Ok(layout) = Layout::from_size_align(size, align) {
        unsafe { ALLOC.alloc(layout) }
    } else {
        null_mut()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    if let Ok(layout) = Layout::from_size_align(size, align) {
        unsafe { ALLOC.dealloc(ptr, layout) };
    }
}