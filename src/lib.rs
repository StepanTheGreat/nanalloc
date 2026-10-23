#![no_std]
#![no_main]

mod buddytree;

pub mod util;
mod alloc;

#[panic_handler]
fn _panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}