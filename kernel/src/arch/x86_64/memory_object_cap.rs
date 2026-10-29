//! K8B live capability fixture witness. The descriptor contains production
//! storage addresses, field offsets and identity metadata only. QMP reads the
//! raw capability, paging, channel, page-table and allocator state while the
//! sole CPU is stopped in `neu_memory_cap_wait`.
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
static neu_memory_cap_witness: [AtomicU64; CAP_WORDS] = [const { AtomicU64::new(0) }; CAP_WORDS];
global_asm!(
    r#"
.section .text
.global neu_memory_cap_wait
.global neu_memory_cap_wait_end
neu_memory_cap_wait:
    mov ecx, 0x10000000
1:  dec ecx
    jnz 1b
neu_memory_cap_wait_end:
    ret
"#
);

pub(crate) const CAP_MAGIC: u64 = 0x4e45_554d_4f00_0003;
pub(crate) const CAP_WORDS: usize = 194;
pub(crate) const CAP_VALUE: u64 = 0x4e45_554d_4f43_4150;
pub(crate) const CAP_SECOND: u64 = 0x5a5a_0123_4567_89ac;
pub(crate) const CAP_RESULT: u64 = 0x4c49_5645_4341_5001;
pub(crate) const CAP_VA: u64 = 0x0060_0000;
pub(crate) const CAP_BLOCK_A: usize = 0x240;
pub(crate) const CAP_BLOCK_B: usize = 0x2c0;
pub(crate) const CAP_VALUE_WORD: usize = 0x00;
pub(crate) const CAP_SECOND_WORD: usize = 0x08;
pub(crate) const CAP_READY: usize = 0x10;
pub(crate) const CAP_RESULT_WORD: usize = 0x18;
pub(crate) const CAP_GATE_FAILED: usize = 0x20;
pub(crate) const CAP_READBACK: usize = 0x30;
pub(crate) const CAP_FAILED_STATUS: usize = 0x38;
pub(crate) const CAP_SEND_STATUS: usize = 0x40;
pub(crate) const CAP_HANDLE: usize = 0x48;
pub(crate) const CAP_RECEIVED: usize = 0x50;
pub(crate) const CAP_GATE_SENT: usize = 0x58;

pub(crate) struct CapIds {
    pub node_a: u64,
    pub node_b: u64,
    pub space_a: u64,
    pub space_b: u64,
    pub thread_a1: u64,
    pub thread_b1: u64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn checkpoint(
    paging: &Paging<'_, '_>,
    stage: u64,
    object: Option<MemoryObjectId>,
    a: Option<&UserSpace>,
    b: Option<&UserSpace>,
    saved: [Option<u64>; 2],
    ids: &CapIds,
    table_a: Option<[u64; 20]>,
    table_b: Option<[u64; 20]>,
) -> Result<()> {
    let (objects, channels, stats, queued, pending) =
        super::address_space::capability_diagnostics().map_err(|_| Error::Corrupt)?;
    let (creator, mappings, pages) = object
        .and_then(|object| paging.live_counts(object).ok())
        .unwrap_or((0, 0, 0));
    let capability_refs = object
        .and_then(|object| paging.memory_object_capability_refs(object).ok())
        .unwrap_or(0);
    let mut words = [0u64; CAP_WORDS];
    words[..16].copy_from_slice(&[
        CAP_MAGIC,
        stage,
        1,
        paging.root(),
        CAP_VA,
        object.map(MemoryObjectId::raw).unwrap_or(0),
        CAP_VALUE,
        CAP_SECOND,
        CAP_RESULT,
        ids.node_a,
        ids.node_b,
        ids.space_a,
        ids.space_b,
        ids.thread_a1,
        ids.thread_b1,
        0,
    ]);
    words[16..55].copy_from_slice(&paging.cap_registry_layout());
    if let Some(layout) = table_a {
        words[56..76].copy_from_slice(&layout);
    }
    if let Some(layout) = table_b {
        words[76..96].copy_from_slice(&layout);
    }
    words[96..114].copy_from_slice(&objects);
    words[114..146].copy_from_slice(&channels);
    words[146..154].copy_from_slice(&paging.cap_allocator_layout());
    words[154..160].copy_from_slice(&paging.cap_tables_layout());
    words[160] = saved[0].unwrap_or(0);
    words[161] = saved[1].unwrap_or(0);
    words[162] = CAP_BLOCK_A as u64;
    words[163] = CAP_BLOCK_B as u64;
    words[164] = CAP_READY as u64;
    words[165] = CAP_GATE_FAILED as u64;
    words[166] = CAP_GATE_SENT as u64;
    words[167] = CAP_FAILED_STATUS as u64;
    words[168] = CAP_SEND_STATUS as u64;
    words[169] = CAP_HANDLE as u64;
    words[170] = CAP_RECEIVED as u64;
    words[171] = stats.live as u64;
    words[172] = stats.references as u64;
    words[173] = stats.pins as u64;
    words[174] = queued as u64;
    words[175] = pending as u64;
    words[186] = creator as u64;
    words[187] = mappings as u64;
    words[188] = capability_refs as u64;
    words[189] = pages as u64;
    words[190] = CAP_VALUE_WORD as u64;
    words[191] = CAP_SECOND_WORD as u64;
    words[192] = CAP_RESULT_WORD as u64;
    words[193] = CAP_READBACK as u64;
    for (index, token) in [a, b].into_iter().enumerate() {
        if let Some(space) = token {
            words[176 + index * 5..181 + index * 5]
                .copy_from_slice(&super::paging::memory_object_live::cap_space_layout(space));
        }
    }
    for (cell, value) in neu_memory_cap_witness.iter().zip(words) {
        cell.store(value, Ordering::Release);
    }
    serial::write(format_args!("[NEU] Cap checkpoint: stage={}\n", stage))
        .map_err(|_| Error::Corrupt)?;
    // SAFETY: BSP, IF=0, exclusive Paging borrow. The assembly only delays;
    // QMP freezes the CPU before inspecting the published descriptor.
    unsafe {
        neu_memory_cap_wait();
    }
    Ok(())
}

unsafe extern "C" {
    fn neu_memory_cap_wait();
}
