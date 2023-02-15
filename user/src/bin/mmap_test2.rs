#![no_std]
#![no_main]
#![macro_use]
extern crate alloc;
extern crate user_lib;

use user_lib::{mmap, open, OpenFlags, println};

#[no_mangle]
pub fn main() {
    assert!(mmap(999999999, 1024, 0) < 0); // invalid fd
    assert!(mmap(0, 1024, 0) < 0); // stdin, not a regular file

    let fd = open("filea\0", OpenFlags::RDONLY);
    if fd < 0 {
        panic!("Error occured when opening file");
    }

    let fd = fd as usize;
    assert!(mmap(fd, 4096, 42) < 0); // offset not page-aligned
    assert!(mmap(fd, 4096, 1048576) < 0); // offset too large

    let res = mmap(fd, 100, 0);
    let p = res as *mut u8;
    println!("first char: {}", unsafe { *p as char });
    println!("OK but garbage: {}", unsafe { *p.offset(123) as char });

    unsafe {
        *p = 6; // not writable, this will trigger page fault
    }
    panic!("should not reach here!");
}
