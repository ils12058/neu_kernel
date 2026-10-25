#![no_std]

pub mod memory;
pub mod paging;
pub mod syscall;

#[cfg(test)]
extern crate std;
