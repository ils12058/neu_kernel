//! K8C runtime mapping fixture witness. The descriptor contains production
//! storage addresses, field offsets and identity metadata only. QMP reads the
//! raw capability, mapping, channel, page-table and allocator state while the
//! sole CPU is stopped in `neu_memory_object_runtime_wait`.
use super::{
    paging::{Paging, UserSpace},
    serial,
};
use core::{
    arch::global_asm,
    sync::atomic::{AtomicU64, Ordering},
};
use neu_kernel::paging::{Error, MemoryObjectId, Result};

#[unsafe(no_mangle)]
static neu_memory_object_runtime_witness: [AtomicU64; RUNTIME_WORDS] =
    [const { AtomicU64::new(0) }; RUNTIME_WORDS];
global_asm!(
    r#"
.section .text
.global neu_memory_object_runtime_wait
.global neu_memory_object_runtime_wait_end
neu_memory_object_runtime_wait:
    mov ecx, 0x10000000
1:  dec ecx
    jnz 1b
neu_memory_object_runtime_wait_end:
    ret
"#
);

pub(crate) const RUNTIME_MAGIC: u64 = 0x4e45_554d_4f00_0004;
pub(crate) const RUNTIME_WORDS: usize = 256;
pub(crate) const RUNTIME_VALUE: u64 = 0x4e45_554d_4f52_554e;
pub(crate) const RUNTIME_SECOND: u64 = 0x5a5a_5255_4e54_494d;
pub(crate) const RUNTIME_WRITE: u64 = 0x4231_5752_4954_4501;
pub(crate) const RUNTIME_RESULT: u64 = 0x4c49_5645_5245_4144;
pub(crate) const RUNTIME_VA_A: u64 = 0x0060_0000;
pub(crate) const RUNTIME_VA_B: u64 = 0x0070_0000;
pub(crate) const RUNTIME_VA_B_WRITE: u64 = 0x0070_1000;
/// Reserved-range probe: the low 64 KiB above the image base is refused by the
/// ordinary request validator before any authority is consulted.
pub(crate) const RUNTIME_VA_RESERVED: u64 = 0x0040_1000;
/// Unmapped probe inside the destination Node's own user window. The Ring 3
/// store is the genuine fault used by the cancellation profile.
pub(crate) const RUNTIME_VA_B2_FAULT: u64 = 0x0040_7000;
pub(crate) const RUNTIME_BLOCK_A: u64 = 0x240;
pub(crate) const RUNTIME_BLOCK_B: u64 = 0x2c0;
pub(crate) const RUNTIME_BLOCK_B2: u64 = 0x340;
pub(crate) const RUNTIME_VALUE_WORD: usize = 0x00;
pub(crate) const RUNTIME_SECOND_WORD: usize = 0x08;
pub(crate) const RUNTIME_READY: usize = 0x10;
pub(crate) const RUNTIME_RESULT_WORD: usize = 0x18;
pub(crate) const RUNTIME_GATE: usize = 0x20;
pub(crate) const RUNTIME_SPIN: usize = 0x28;
pub(crate) const RUNTIME_READBACK: usize = 0x30;
pub(crate) const RUNTIME_STALE_STATUS: usize = 0x38;
pub(crate) const RUNTIME_MAIN_STATUS: usize = 0x40;
pub(crate) const RUNTIME_MEM_HANDLE: usize = 0x48;
pub(crate) const RUNTIME_RECEIVED: usize = 0x50;
pub(crate) const RUNTIME_GATE_SECOND: usize = 0x58;
pub(crate) const RUNTIME_GATE_THIRD: usize = 0x60;
pub(crate) const RUNTIME_MODE_WORD: usize = 0x68;
pub(crate) const RUNTIME_CLOSE_STATUS: usize = 0x70;
pub(crate) const RUNTIME_AFTER_STATUS: usize = 0x78;

pub(crate) const RUNTIME_GOLDEN: u64 = 0;
pub(crate) const RUNTIME_WRITABLE: u64 = 1;
pub(crate) const RUNTIME_FAULT: u64 = 2;
pub(crate) const RUNTIME_RIGHTS: u64 = 3;
pub(crate) const RUNTIME_PEER_EXIT: u64 = 4;
pub(crate) const RUNTIME_CANCEL: u64 = 5;
pub(crate) const RUNTIME_READ_DENIED: u64 = 6;

/// Per-mode Ring 3 probe/verdict block in the requesting Node's DATA page.
/// The controller copies raw syscall status words here; QMP reads them through
/// the destination root's own page tables.
pub(crate) const RUNTIME_P_BASE: u64 = 0x3c0;
pub(crate) const RUNTIME_P_STALE: usize = 0x00;
pub(crate) const RUNTIME_P_FORGED: usize = 0x08;
pub(crate) const RUNTIME_P_TYPE: usize = 0x10;
pub(crate) const RUNTIME_P_RANGE: usize = 0x18;
pub(crate) const RUNTIME_P_PAGES: usize = 0x20;
pub(crate) const RUNTIME_P_OFFSET: usize = 0x28;
pub(crate) const RUNTIME_P_GENERATION: usize = 0x30;
pub(crate) const RUNTIME_P_UNALIGNED: usize = 0x38;
pub(crate) const RUNTIME_P_OVERLAP: usize = 0x40;
pub(crate) const RUNTIME_P_WRITE: usize = 0x48;
pub(crate) const RUNTIME_P_CLOSE: usize = 0x50;
pub(crate) const RUNTIME_P_AFTER: usize = 0x58;
pub(crate) const RUNTIME_P_REPLAY: usize = 0x60;
pub(crate) const RUNTIME_P_MAIN: usize = 0x68;
pub(crate) const RUNTIME_P_DENIED_READ: usize = 0x70;
pub(crate) const RUNTIME_P_DENIED_WRITE: usize = 0x78;
pub(crate) const RUNTIME_P_DONE: usize = 0x80;
/// Initial value of every controller-owned P word. QMP rejects a word that
/// still holds the sentinel, so an unperformed probe cannot read as success.
pub(crate) const RUNTIME_P_SENTINEL: u64 = 0xBAD0_0000_0000_0000;
pub(crate) const RUNTIME_P_OFFSETS: [usize; 18] = [
    RUNTIME_P_BASE as usize,
    RUNTIME_P_STALE,
    RUNTIME_P_FORGED,
    RUNTIME_P_TYPE,
    RUNTIME_P_RANGE,
    RUNTIME_P_PAGES,
    RUNTIME_P_OFFSET,
    RUNTIME_P_GENERATION,
    RUNTIME_P_UNALIGNED,
    RUNTIME_P_OVERLAP,
    RUNTIME_P_WRITE,
    RUNTIME_P_CLOSE,
    RUNTIME_P_AFTER,
    RUNTIME_P_REPLAY,
    RUNTIME_P_MAIN,
    RUNTIME_P_DENIED_READ,
    RUNTIME_P_DENIED_WRITE,
    RUNTIME_P_DONE,
];
/// Bounded Ring 3 command stream areas inside the Node DATA page, one 8-word
/// record per command. Only the trusted fixture writes them.
pub(crate) const RUNTIME_SCRIPT_B: usize = 0x500;
pub(crate) const RUNTIME_SCRIPT_B2: usize = 0xC00;
pub(crate) const RUNTIME_SCRIPT_STRIDE: usize = 0x40;
pub(crate) const RUNTIME_SCRIPT_STATUS: usize = 0x30;

pub(crate) const fn mode() -> u64 {
    if cfg!(feature = "test-memory-object-runtime-writable") {
        RUNTIME_WRITABLE
    } else if cfg!(feature = "test-memory-object-runtime-fault") {
        RUNTIME_FAULT
    } else if cfg!(feature = "test-memory-object-runtime-rights") {
        RUNTIME_RIGHTS
    } else if cfg!(feature = "test-memory-object-runtime-read-denied") {
        RUNTIME_READ_DENIED
    } else if cfg!(feature = "test-memory-object-runtime-peer-exit") {
        RUNTIME_PEER_EXIT
    } else if cfg!(feature = "test-memory-object-runtime-cancel") {
        RUNTIME_CANCEL
    } else {
        RUNTIME_GOLDEN
    }
}

pub(crate) struct RuntimeIds {
    pub node_a: u64,
    pub node_b: u64,
    pub space_a: u64,
    pub space_b: u64,
    pub thread_a1: u64,
    pub thread_b1: u64,
    pub thread_b2: u64,
}

impl RuntimeIds {
    pub const EMPTY: Self = Self {
        node_a: 0,
        node_b: 0,
        space_a: 0,
        space_b: 0,
        thread_a1: 0,
        thread_b1: 0,
        thread_b2: 0,
    };
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn checkpoint(
    paging: &Paging<'_, '_>,
    stage: u64,
    mode: u64,
    object: Option<MemoryObjectId>,
    a: Option<&UserSpace>,
    b: Option<&UserSpace>,
    saved: [Option<u64>; 3],
    ids: &RuntimeIds,
    table_a: Option<[u64; 20]>,
    table_b: Option<[u64; 20]>,
    order: u64,
    audits: [u64; 10],
) -> Result<()> {
    let (objects, channels, stats, queued, pending) =
        super::address_space::capability_diagnostics().map_err(|_| Error::Corrupt)?;
    let diagnostics = super::address_space::request_diagnostics().map_err(|_| Error::Corrupt)?;
    let (creator, mappings, pages) = object
        .and_then(|object| paging.live_counts(object).ok())
        .unwrap_or((0, 0, 0));
    let capability_refs = object
        .and_then(|object| paging.memory_object_capability_refs(object).ok())
        .unwrap_or(0);
    let object_physical = |page: usize| {
        object
            .and_then(|object| paging.live_physical(object, page).ok())
            .unwrap_or(0)
    };
    let mut words = [0u64; RUNTIME_WORDS];
    words[..16].copy_from_slice(&[
        RUNTIME_MAGIC,
        stage,
        mode,
        paging.root(),
        RUNTIME_VA_A,
        RUNTIME_VA_B,
        RUNTIME_VALUE,
        RUNTIME_SECOND,
        RUNTIME_WRITE,
        ids.node_a,
        ids.node_b,
        ids.space_a,
        ids.space_b,
        ids.thread_a1,
        ids.thread_b1,
        ids.thread_b2,
    ]);
    words[16..55].copy_from_slice(&paging.cap_registry_layout());
    if let Some(layout) = table_a {
        words[55..75].copy_from_slice(&layout);
    }
    if let Some(layout) = table_b {
        words[75..95].copy_from_slice(&layout);
    }
    words[95..113].copy_from_slice(&objects);
    words[113..145].copy_from_slice(&channels);
    words[145..153].copy_from_slice(&paging.cap_allocator_layout());
    words[153..159].copy_from_slice(&paging.cap_tables_layout());
    words[159] = saved[0].unwrap_or(0);
    words[160] = saved[1].unwrap_or(0);
    words[161] = saved[2].unwrap_or(0);
    words[162] = RUNTIME_BLOCK_A;
    words[163] = RUNTIME_BLOCK_B;
    words[164] = RUNTIME_BLOCK_B2;
    words[165] = RUNTIME_VALUE_WORD as u64;
    words[166] = RUNTIME_SECOND_WORD as u64;
    words[167] = RUNTIME_READY as u64;
    words[168] = RUNTIME_RESULT_WORD as u64;
    words[169] = RUNTIME_GATE as u64;
    words[170] = RUNTIME_SPIN as u64;
    words[171] = RUNTIME_READBACK as u64;
    words[172] = RUNTIME_STALE_STATUS as u64;
    words[173] = RUNTIME_MAIN_STATUS as u64;
    words[174] = RUNTIME_MEM_HANDLE as u64;
    words[175] = RUNTIME_RECEIVED as u64;
    words[176] = RUNTIME_GATE_SECOND as u64;
    words[177] = RUNTIME_GATE_THIRD as u64;
    words[178] = RUNTIME_MODE_WORD as u64;
    words[179] = super::address_space::map_witness_address();
    words[180] = core::ptr::addr_of!(super::native::NEU_LIVE_FAULT) as u64;
    words[181] = RUNTIME_VA_B_WRITE;
    words[182] = RUNTIME_VA_RESERVED;
    words[183] = RUNTIME_RESULT;
    words[184] = stats.live as u64;
    words[185] = stats.references as u64;
    words[186] = stats.pins as u64;
    words[187] = queued as u64;
    words[188] = pending as u64;
    words[189] = creator as u64;
    words[190] = mappings as u64;
    words[191] = capability_refs as u64;
    words[192] = pages as u64;
    words[204] = RUNTIME_CLOSE_STATUS as u64;
    words[205] = RUNTIME_AFTER_STATUS as u64;
    for (index, offset) in RUNTIME_P_OFFSETS.iter().enumerate() {
        words[206 + index] = *offset as u64;
    }
    words[224] = order;
    words[225..233].copy_from_slice(&audits[..8]);
    words[233] = RUNTIME_SCRIPT_STRIDE as u64;
    words[234] = RUNTIME_SCRIPT_B as u64;
    words[235] = RUNTIME_SCRIPT_B2 as u64;
    words[236] = RUNTIME_SCRIPT_STATUS as u64;
    let fault_sites = super::multithread::runtime_fault_sites();
    words[237] = RUNTIME_VA_B2_FAULT;
    words[238] = fault_sites[0];
    words[239] = fault_sites[1];
    words[240] = RUNTIME_P_BASE;
    words[241] = RUNTIME_P_SENTINEL;
    words[242] = object_physical(0);
    words[243] = object_physical(1);
    words[244] = object.map(|object| object.raw()).unwrap_or(0);
    words[245..250].copy_from_slice(&[
        diagnostics[40],
        diagnostics[41],
        diagnostics[42],
        diagnostics[0],
        diagnostics[15],
    ]);
    words[250..254].copy_from_slice(&[
        diagnostics[43],
        diagnostics[20],
        diagnostics[35],
        diagnostics[36],
    ]);
    words[254] = audits[8];
    words[255] = audits[9];
    for (index, token) in [a, b].into_iter().enumerate() {
        if let Some(space) = token {
            words[193 + index * 5..198 + index * 5]
                .copy_from_slice(&super::paging::memory_object_live::cap_space_layout(space));
        }
    }
    words[203] = u64::from(interrupts_flags() & 0x200 != 0);
    for (cell, value) in neu_memory_object_runtime_witness.iter().zip(words) {
        cell.store(value, Ordering::Release);
    }
    serial::write(format_args!(
        "[NEU] Runtime checkpoint: stage={} mode={}\n",
        stage, mode
    ))
    .map_err(|_| Error::Corrupt)?;
    // SAFETY: BSP, IF=0, exclusive Paging borrow. The assembly only delays;
    // QMP freezes the CPU before inspecting the published descriptor.
    unsafe {
        neu_memory_object_runtime_wait();
    }
    Ok(())
}

fn interrupts_flags() -> u64 {
    let flags: u64;
    // SAFETY: Read-only RFLAGS query on the kernel stack; no state changes.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    flags
}

unsafe extern "C" {
    fn neu_memory_object_runtime_wait();
}
