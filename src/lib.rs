#![no_std]
#![no_main]

mod buddytree;

mod util;

#[cfg(target_family = "wasm")]
mod alloc;

#[cfg(target_family = "wasm")]
pub use alloc::NanAlloc;

#[panic_handler]
fn _panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}