//! Minimal Limine base-revision-6 adapter; no third-party Rust dependencies.
//! Layouts/IDs: limine-protocol commit 3a0526b700e356f0eac1b71a77697b3fd1c707a3,
//! include/limine.h and PROTOCOL.md. Phase 1A additionally requests the actual
//! executable address for reservations; HHDM is requested only by the #PF test.

use core::{
    cell::UnsafeCell,
    mem, ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::arch;
use crate::memory::{MAX_REGIONS, MemoryMap, Region as MemoryRegion, Reservation};

#[repr(transparent)]
struct LoaderCell<T>(UnsafeCell<T>);

// SAFETY: The loader writes these cells before entering the kernel. Afterwards
// only the BSP reads them, with interrupts disabled; no runtime writer exists.
unsafe impl<T> Sync for LoaderCell<T> {}

#[repr(C)]
struct Request<T> {
    id: [u64; 4],
    revision: u64,
    response: *const T,
}

impl<T> Request<T> {
    const fn new(id: [u64; 2]) -> Self {
        Self {
            id: [0xc7b1_dd30_df4c_8b88, 0x0a82_e883_a194_f07b, id[0], id[1]],
            revision: 0,
            response: ptr::null(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FirmwareResponse {
    revision: u64,
    firmware_type: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryMapResponse {
    revision: u64,
    count: u64,
    entries: *const *const MemoryRegion,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ExecutableAddress {
    revision: u64,
    physical_base: u64,
    virtual_base: u64,
}

#[cfg(feature = "test-page-fault")]
#[repr(C)]
#[derive(Clone, Copy)]
struct HhdmResponse {
    revision: u64,
    offset: u64,
}

struct KernelRegions(UnsafeCell<[MemoryRegion; MAX_REGIONS]>);
// SAFETY: A once-only BSP initialization publishes immutable descriptors. No
// loader or kernel writer retains access once initialize() returns.
unsafe impl Sync for KernelRegions {}
static REGIONS: KernelRegions = KernelRegions(UnsafeCell::new([MemoryRegion::EMPTY; MAX_REGIONS]));
static COPIED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

pub fn kernel_virtual_range() -> (usize, usize) {
    (
        ptr::addr_of!(__kernel_start) as usize,
        ptr::addr_of!(__kernel_end) as usize,
    )
}

// Compile-time checks for the exact C-layout prefixes consumed by this adapter.
const _: () = assert!(mem::size_of::<Request<FirmwareResponse>>() == 48);
const _: () = assert!(mem::align_of::<Request<FirmwareResponse>>() == 8);
const _: () = assert!(mem::offset_of!(Request<FirmwareResponse>, response) == 40);
const _: () = assert!(mem::size_of::<FirmwareResponse>() == 16);
const _: () = assert!(mem::size_of::<MemoryMapResponse>() == 24);
const _: () = assert!(mem::size_of::<MemoryRegion>() == 24);

#[used]
#[unsafe(link_section = ".limine_requests_start")]
static START: [u64; 4] = [
    0xf6b8_f4b3_9de7_d1ae,
    0xfab9_1a69_40fc_b9cf,
    0x785c_6ed0_15d3_e316,
    0x181e_920a_7852_b9d9,
];

#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE: LoaderCell<[u64; 3]> = LoaderCell(UnsafeCell::new([
    0xf956_2b2d_5c95_a6c8,
    0x6a7b_3849_4453_6bdc,
    6,
]));

#[used]
#[unsafe(link_section = ".limine_requests")]
static FIRMWARE: LoaderCell<Request<FirmwareResponse>> =
    LoaderCell(UnsafeCell::new(Request::new([
        0x8c2f_75d9_0bef_28a8,
        0x7045_a468_8eac_00c3,
    ])));

#[used]
#[unsafe(link_section = ".limine_requests")]
static EXECUTABLE: LoaderCell<Request<ExecutableAddress>> =
    LoaderCell(UnsafeCell::new(Request::new([
        0x71ba_7686_3cc5_5f63,
        0xb264_4a48_c516_a487,
    ])));

#[cfg(feature = "test-page-fault")]
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: LoaderCell<Request<HhdmResponse>> = LoaderCell(UnsafeCell::new(Request::new([
    0x48dc_f1cb_8ad2_b852,
    0x6398_4e95_9a98_244b,
])));

#[used]
// The negative-test image deliberately leaves this request outside the markers.
// Its null response must then be rejected before any response dereference.
#[cfg_attr(
    not(feature = "test-missing-memory-map"),
    unsafe(link_section = ".limine_requests")
)]
static MEMORY_MAP: LoaderCell<Request<MemoryMapResponse>> =
    LoaderCell(UnsafeCell::new(Request::new([
        0x67cf_3d9d_378a_806f,
        0xe304_acdf_c50c_3c62,
    ])));

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static END: [u64; 2] = [0xadc0_e053_1bb1_0d03, 0x9572_709f_3176_4c62];

pub struct BootInfo {
    pub memory_regions: u64,
    pub usable_bytes: u64,
    pub memory_map: MemoryMap<'static>,
}

#[cfg(feature = "test-page-fault")]
pub fn test_hhdm_offset() -> Result<u64, &'static str> {
    // SAFETY: Called after successful base negotiation; the response follows
    // the same trusted immutable loader mapping contract as other responses.
    let response =
        unsafe { read_boot_value(ptr::read_volatile(ptr::addr_of!((*HHDM.0.get()).response)))? };
    if !arch::valid_boot_range(response.offset as usize, 4096, 4096) {
        return Err("invalid test HHDM offset");
    }
    Ok(response.offset)
}

/// The loader must supply mapped, initialized, correctly sized objects that
/// remain immutable after entry. Numeric checks cannot prove arbitrary pointers
/// are mapped. This is a trusted loader contract, not a general user-copy API.
unsafe fn read_boot_value<T: Copy>(address: *const T) -> Result<T, &'static str> {
    if !arch::valid_boot_range(address as usize, mem::size_of::<T>(), mem::align_of::<T>()) {
        return Err("null, unaligned, overflowing, or non-higher-half boot pointer");
    }
    // SAFETY: Caller supplies a Limine-owned pointer satisfying the contract
    // above. All ownership stays with the loader; no Rust reference is created.
    Ok(unsafe { ptr::read_volatile(address) })
}

pub fn initialize() -> Result<BootInfo, &'static str> {
    // SAFETY: The request cells are mapped in the ELF and loading has finished.
    // Validate base semantics before interpreting any response pointer.
    unsafe {
        if ptr::read_volatile(BASE.0.get().cast::<u64>().add(2)) != 0
            || ptr::read_volatile(BASE.0.get().cast::<u64>().add(1)) != 6
        {
            return Err("Limine base revision 6 was not negotiated");
        }
        let firmware = read_boot_value(ptr::read_volatile(ptr::addr_of!(
            (*FIRMWARE.0.get()).response
        )))?;
        if firmware.firmware_type != 2 {
            return Err("Phase 0 requires 64-bit UEFI firmware");
        }
        let response = ptr::read_volatile(ptr::addr_of!((*MEMORY_MAP.0.get()).response));
        if response.is_null() {
            return Err("required Limine memory-map response is missing");
        }
        let map = read_boot_value(response)?;
        if map.count == 0 || map.count > 4096 {
            return Err("invalid or excessive memory-map entry count");
        }
        let count = map.count as usize; // Bounded above before conversion.
        if !arch::valid_boot_range(map.entries as usize, count * 8, 8) {
            return Err("invalid memory-map pointer array");
        }

        let executable = read_boot_value(ptr::read_volatile(ptr::addr_of!(
            (*EXECUTABLE.0.get()).response
        )))?;
        let (kernel_start, kernel_end) = kernel_virtual_range();
        if executable.virtual_base != kernel_start as u64 {
            return Err("unexpected executable virtual base");
        }
        let reservation =
            Reservation::new(executable.physical_base, (kernel_end - kernel_start) as u64)?;
        if COPIED.swap(true, Ordering::Relaxed) {
            return Err("boot map initialized twice");
        }
        let destination = REGIONS.0.get().cast::<MemoryRegion>();
        if (destination as usize) < kernel_start
            || (destination as usize)
                .checked_add(mem::size_of::<MemoryRegion>() * MAX_REGIONS)
                .is_none_or(|end| end > kernel_end)
        {
            return Err("boot map storage is outside the reserved kernel image");
        }
        for index in 0..count {
            let region = read_boot_value(ptr::read_volatile(map.entries.add(index)))?;
            ptr::write(destination.add(index), region);
        }
        // The fixed array is initialized, within the kernel reservation, and
        // never mutated after publication. The constructor validates every copy.
        let copied = core::slice::from_raw_parts(destination, count);
        let memory_map = MemoryMap::new(copied, reservation)?;
        // Audit preservation against the still-live loader data before retiring
        // all raw descriptor pointers. Retaining boot memory makes this safe.
        for (index, region) in copied.iter().enumerate() {
            if *region != read_boot_value(ptr::read_volatile(map.entries.add(index)))? {
                return Err("kernel-owned memory-map copy differs from loader data");
            }
        }
        Ok(BootInfo {
            memory_regions: map.count,
            usable_bytes: memory_map.usable_bytes(),
            memory_map,
        })
    }
}
