use crate::{
    config::PAGE_SIZE,
    fs::OSInode,
    mm::{
        frame_alloc, FrameTracker, MapPermission, PTEFlags, PageTable, PhysAddr, PhysPageNum,
        VirtAddr, VirtPageNum,
    },
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use easy_fs::Inode;

use crate::{fs::File, task::current_task};

/// Base virtual address for mmap
pub const MMAP_AREA_BASE: usize = 0x0000_0001_0000_0000;

/// A naive linear virtual address space allocator
pub struct VirtualAddressAllocator {
    cur_va: VirtAddr,
}

impl VirtualAddressAllocator {
    /// Create a new allocator with given base virtual address
    pub fn new(base: usize) -> Self {
        Self {
            cur_va: base.into(),
        }
    }

    /// Allocate a virtual address area
    pub fn alloc(&mut self, len: usize) -> VirtAddr {
        let start = self.cur_va;
        let end: VirtAddr = (self.cur_va.0 + len).into();
        self.cur_va = end.ceil().into();
        start
    }
}

#[derive(Clone)]
struct MapRange {
    start: VirtAddr,
    len: usize,    // length in bytes
    offset: usize, // offset in file
    perm: MapPermission,
}

impl MapRange {
    fn new(start: VirtAddr, len: usize, offset: usize, perm: MapPermission) -> Self {
        Self {
            start,
            len,
            offset,
            perm,
        }
    }

    fn contains(&self, va: VirtAddr) -> bool {
        self.start <= va && va.0 < self.start.0 + self.len
    }

    fn va_offset(&self, vpn: VirtPageNum) -> usize {
        let aligned_va: VirtAddr = vpn.into();
        aligned_va.0 - self.start.0
    }

    fn file_offset(&self, vpn: VirtPageNum) -> usize {
        self.va_offset(vpn) + self.offset
    }
}

/// Structure to describe file mappings
pub struct FileMapping {
    file: Arc<Inode>,
    ranges: Vec<MapRange>,
    frames: Vec<FrameTracker>,
    dirty_parts: BTreeSet<usize>, // file segments that need writing back
    map: BTreeMap<usize, PhysPageNum>, // file offset -> ppn
}

impl FileMapping {
    fn new_empty(file: Arc<Inode>) -> Self {
        Self {
            file,
            ranges: Vec::new(),
            frames: Vec::new(),
            dirty_parts: BTreeSet::new(),
            map: BTreeMap::new(),
        }
    }

    fn push(&mut self, start: VirtAddr, len: usize, offset: usize, perm: MapPermission) {
        self.ranges.push(MapRange::new(start, len, offset, perm));
    }

    /// Check whether a virtual address belongs to this mapping
    fn contains(&self, va: VirtAddr) -> bool {
        self.ranges.iter().any(|r| r.contains(va))
    }

    /// Create mapping for given virtual address
    fn map(&mut self, va: VirtAddr) -> Option<(PhysPageNum, MapRange, bool)> {
        // Note: currently virtual address ranges never intersect
        let vpn = va.floor();
        for range in &self.ranges {
            if !range.contains(va) {
                continue;
            }
            let offset = range.file_offset(vpn);
            let (ppn, shared) = match self.map.get(&offset) {
                Some(&ppn) => (ppn, true),
                None => {
                    let frame = frame_alloc().unwrap();
                    let ppn = frame.ppn;
                    self.frames.push(frame);
                    self.map.insert(offset, ppn);
                    (ppn, false)
                }
            };
            if range.perm.contains(MapPermission::W) {
                self.dirty_parts.insert(offset);
            }
            return Some((ppn, range.clone(), shared));
        }
        None
    }

    /// Write back all dirty pages
    pub fn sync(&self) {
        let file_size = self.file.get_size();
        for &offset in self.dirty_parts.iter() {
            let ppn = self.map.get(&offset).unwrap();
            if offset < file_size {
                // WARNING: this can still cause garbage written
                //  to file when sharing physical page
                let va_len = self
                    .ranges
                    .iter()
                    .map(|r| {
                        if r.offset <= offset && offset < r.offset + r.len {
                            PAGE_SIZE.min(r.offset + r.len - offset)
                        } else {
                            0
                        }
                    })
                    .max()
                    .unwrap();
                let write_len = va_len.min(file_size - offset);

                self.file
                    .write_at(offset, &ppn.get_bytes_array()[..write_len]);
            }
        }
    }
}

/// This is a simplified version of mmap which only supports file-backed mapping
pub fn sys_mmap(fd: usize, len: usize, offset: usize) -> isize {
    if len == 0 {
        // invalid length
        return -1;
    }
    if (offset & (PAGE_SIZE - 1)) != 0 {
        // offset must be page size aligned
        return -1;
    }

    let task = current_task().unwrap();
    let mut tcb = task.inner_exclusive_access();
    if fd >= tcb.fd_table.len() {
        return -1;
    }
    if tcb.fd_table[fd].is_none() {
        return -1;
    }

    let fp = tcb.fd_table[fd].as_ref().unwrap();
    let opt_inode = fp.as_any().downcast_ref::<OSInode>();
    if opt_inode.is_none() {
        // must be a regular file
        return -1;
    }

    let inode = opt_inode.unwrap();
    let perm = parse_permission(inode);
    let file = inode.clone_inner_inode();
    if offset >= file.get_size() {
        // file offset exceeds size limit
        return -1;
    }

    let start = tcb.mmap_va_allocator.alloc(len);
    let mappings = &mut tcb.file_mappings;
    if let Some(m) = find_file_mapping(mappings, &file) {
        m.push(start, len, offset, perm);
    } else {
        let mut m = FileMapping::new_empty(file);
        m.push(start, len, offset, perm);
        mappings.push(m);
    }
    start.0 as isize
}

/// Try to handle page fault caused by demand paging
/// Returns whether this page fault is fixed
pub fn handle_page_fault(fault_addr: usize) -> bool {
    let fault_va: VirtAddr = fault_addr.into();
    let fault_vpn = fault_va.floor();
    let task = current_task().unwrap();
    let mut tcb = task.inner_exclusive_access();

    if let Some(pte) = tcb.memory_set.translate(fault_vpn) {
        if pte.is_valid() {
            return false; // fault va already mapped, we cannot handle this
        }
    }

    match tcb.file_mappings.iter_mut().find(|m| m.contains(fault_va)) {
        Some(mapping) => {
            let file = Arc::clone(&mapping.file);
            // fix vm mapping
            let (ppn, range, shared) = mapping.map(fault_va).unwrap();
            tcb.memory_set.map(fault_vpn, ppn, range.perm);

            if !shared {
                // load file content
                let file_size = file.get_size();
                let file_offset = range.file_offset(fault_vpn);
                assert!(file_offset < file_size);

                // let va_offset = range.va_offset(fault_vpn);
                // let va_len = range.len - va_offset;
                // Note: we do not limit `read_len` with `va_len`
                // consider two overlapping areas with different lengths

                let read_len = PAGE_SIZE.min(file_size - file_offset);
                file.read_at(file_offset, &mut ppn.get_bytes_array()[..read_len]);
            }
            true
        }
        None => false,
    }
}

fn parse_permission(inode: &OSInode) -> MapPermission {
    let mut perm = MapPermission::U;
    if inode.readable() {
        perm |= MapPermission::R;
    }
    if inode.writable() {
        perm |= MapPermission::W;
    }
    perm
}

fn find_file_mapping<'a>(
    mappings: &'a mut Vec<FileMapping>,
    file: &Arc<Inode>,
) -> Option<&'a mut FileMapping> {
    mappings.iter_mut().find(|m| Arc::ptr_eq(&m.file, file))
}
