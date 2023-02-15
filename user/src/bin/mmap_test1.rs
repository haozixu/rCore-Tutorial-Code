#![no_std]
#![no_main]
#![macro_use]
extern crate alloc;
extern crate user_lib;

use user_lib::{close, mmap, open, println, OpenFlags};

#[no_mangle]
pub fn main() -> i32 {
    let fd = open("filea\0", OpenFlags::RDWR);
    if fd < 0 {
        panic!("Error occured when opening file");
    }

    let fd = fd as usize;
    let res1 = mmap(fd, 100, 0);
    if res1 == -1 {
        panic!("first mmap failed!");
    }
    let res2 = mmap(fd, 120, 0);
    if res2 == -1 {
        panic!("second mmap failed!");
    }
    close(fd); // mappings are still available after this

    let p = res1 as *mut u8;
    unsafe {
        *p = 'h' as u8;
        *p.offset(1) = 'E' as u8;
        *p.offset(2) = 'L' as u8;
        *p.offset(3) = 'L' as u8;
        *p.offset(4) = 'O' as u8;

        println!(
            "{}{}{}{}",
            *p.offset(5) as char,
            *p.offset(6) as char,
            *p.offset(7) as char,
            *p.offset(8) as char
        );
    }

    let q = res2 as *mut u8;
    unsafe {
        *q.offset(5) = '#' as u8;
        *q.offset(6) = '#' as u8;
        *q.offset(7) = 'W' as u8;
        *q.offset(8) = 'O' as u8;
    }
    0
}
