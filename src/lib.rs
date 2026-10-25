#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(not(target_arch = "wasm32"))]
extern crate std;

mod wasm;
mod buddytree;
mod util;
mod alloc;
pub use alloc::NanAlloc;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn _panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}