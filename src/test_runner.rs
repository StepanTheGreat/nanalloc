//! Integration tests for wasm. This simple runner runs tiny tests via node

use core::alloc::{GlobalAlloc, Layout};

use crate::NanAlloc;

#[global_allocator]
static ALLOC: NanAlloc = NanAlloc::new();

#[link(wasm_import_module = "js")]
unsafe extern "C" {
    fn js_panic(line: u32, file_ptr: *const u8, file_len: usize, msg_ptr: *const u8, msg_len: usize);
}

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn _panic_handler(info: &core::panic::PanicInfo<'_>) -> ! {
    let (line, file) = match info.location() {
        Some(location) => (location.line(), location.file()),
        None => (u32::MAX, "<no_file_name>")
    };

    let msg = info.message().as_str().unwrap_or("<no_error_message>");
    
    unsafe { js_panic(line, file.as_ptr(), file.len(), msg.as_ptr(), msg.len()) };

    loop {}
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn run() {
    basic_alloc_tests();
}

unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
    unsafe { ALLOC.alloc(Layout::from_size_align(size, align).unwrap()) }
}

unsafe fn dealloc(ptr: *mut u8, size: usize, align: usize) {
    unsafe { ALLOC.dealloc(ptr, Layout::from_size_align(size, align).unwrap()) }
}

fn basic_alloc_tests() {

    // Try allocate a small block
    let s = unsafe { alloc(8192, 2) };
    assert!(!s.is_null());
    unsafe { s.write_bytes(1, 8192) };

    

    // Try allocate 512 blocks
    let large_block = unsafe { alloc(512*8192, 8192)};
    assert!(!large_block.is_null());
    unsafe { large_block.write_bytes(1, 512*8192) };
    

    // Allocating the entire heap will fail
    assert!(unsafe { alloc(2usize.pow(30), 8192) }.is_null());

    // Deallocate our blocks
    unsafe { dealloc(s, 8192, 2) };
    unsafe { dealloc(s, 2*8192, 8192) };

    // We're not going to allocate the entire heap in these tests. Unfortunately.
}