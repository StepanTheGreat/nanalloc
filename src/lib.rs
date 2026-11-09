#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(not(target_arch = "wasm32"))]
extern crate std;

mod alloc;
mod buddytree;
mod util;
mod wasm;

pub use alloc::NanAlloc;

#[cfg(all(target_arch = "wasm32", feature = "size_test"))]
mod size_test;

#[cfg(all(target_arch = "wasm32", feature = "test_runner"))]
mod test_runner;