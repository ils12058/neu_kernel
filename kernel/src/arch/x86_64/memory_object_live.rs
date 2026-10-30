//! Live Ring 3 MemoryObject sharing fixture. The trusted controller creates
//! one object, maps it into two independent roots with different rights, and
//! publishes only layout/address metadata. QMP reads production registry,
//! page-table and physical backing state while the sole CPU is stopped here.
use super::*;
use core::{
    arch::global_asm,
    mem::{offset_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

#[unsafe(no_mangle)]
static neu_memory_live_witness: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
global_asm!(
    r#"
.section .text
.global neu_memory_live_wait
.global neu_memory_live_wait_end
neu_memory_live_wait:
    mov ecx, 0x10000000
1:  dec ecx
    jnz 1b
neu_memory_live_wait_end:
    ret
"#
);

pub(crate) const LIVE_MAGIC: u64 = 0x4e45_554d_4f00_0002;
pub(crate) const LIVE_VALUE: u64 = 0x4e45_554d_4f4c_4956;
pub(crate) const LIVE_SECOND: u64 = 0x5a5a_0123_4567_89ab;
pub(crate) const LIVE_RESULT: u64 = 0x4c49_5645_4f4b_0001;
pub(crate) const LIVE_VA_A: u64 = 0x0060_0000;
pub(crate) const LIVE_VA_B: u64 = 0x0060_2000;
pub(crate) const LIVE_DATA: u64 = 0x0040_2000;
/// Per-label private block inside the shared Node DATA page. Each block is
/// 72 bytes so the final post-read gate does not alias the next label.
pub(crate) const fn live_block(label: u64) -> usize {
    0x140 + (label as usize - 1) * 0x48
}
pub(crate) const LIVE_VALUE_WORD: usize = 0x00;
pub(crate) const LIVE_SECOND_WORD: usize = 0x08;
pub(crate) const LIVE_READY: usize = 0x10;
pub(crate) const LIVE_RESULT_WORD: usize = 0x18;
pub(crate) const LIVE_GATE: usize = 0x20;
pub(crate) const LIVE_COUNTER: usize = 0x28;
pub(crate) const LIVE_READBACK: usize = 0x30;
pub(crate) const LIVE_FAULT_MARKER: usize = 0x38;
pub(crate) const LIVE_POST: usize = 0x40;

pub(crate) fn cap_space_layout(space: &UserSpace) -> [u64; 5] {
    [
        ptr::addr_of!(space.root) as u64,
        ptr::addr_of!(space.id) as u64,
        space.tables.as_ptr() as u64,
        ptr::addr_of!(space.teardown) as u64,
        space.stack_tables.as_ptr() as u64,
    ]
}

impl Paging<'_, '_> {
    pub fn live_authorize(
        &self,
        object: MemoryObjectId,
        space: SpaceId,
        writable: bool,
    ) -> Result<logic::MemoryMapAuthority> {
        self.registry
            .authorize_memory_mapping(object, space, writable)
    }
    pub fn live_map(
        &mut self,
        space: &mut UserSpace,
        grant: &logic::MemoryMapAuthority,
        base: u64,
        permissions: Permissions,
    ) -> Result<()> {
        self.map_memory_backing(space, grant, 0, base, 2, permissions)
    }
    pub fn live_release_creator(&mut self, object: MemoryObjectId) -> Result<()> {
        self.registry.release_memory_object(object)
    }
    pub fn live_collect(&mut self, object: MemoryObjectId) -> Result<()> {
        self.collect_memory_object(object)
    }
    pub fn live_counts(&self, object: MemoryObjectId) -> Result<(usize, usize, usize)> {
        self.registry.memory_object_counts(object)
    }
    /// Supervisor-only descriptors for the K8B capability fixture. These
    /// expose production storage addresses and field offsets, never values
    /// interpreted as authority.
    pub fn cap_registry_layout(&self) -> [u64; 39] {
        self.registry.diagnostic_layout()
    }
    pub fn cap_allocator_layout(&self) -> [u64; 8] {
        self.memory.frames.diagnostic_layout()
    }
    pub fn cap_tables_layout(&self) -> [u64; 6] {
        [
            self.memory.tables.as_ptr() as u64,
            size_of::<TableOwner>() as u64,
            TABLES as u64,
            offset_of!(TableOwner, physical) as u64,
            offset_of!(TableOwner, level) as u64,
            offset_of!(TableOwner, pinned) as u64,
        ]
    }
    pub fn live_physical(&self, object: MemoryObjectId, page: usize) -> Result<u64> {
        let frame = self.registry.memory_object_frame(object, page)?;
        self.registry.physical(frame)
    }
    /// Bounded private control/observation words in the Node DATA page.
    pub fn live_words(&mut self, space: &UserSpace) -> Result<[u64; 128]> {
        self.member_boundary(space)?;
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            core::array::from_fn(|i| {
                u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap())
            })
        })
    }
    pub fn live_write(&mut self, space: &UserSpace, offset: usize, value: u64) -> Result<()> {
        self.member_boundary(space)?;
        if !offset.is_multiple_of(8) || offset + 8 > PAGE as usize {
            return Err(Error::Address);
        }
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        })
    }
    /// Bounded word access anywhere in the Node's owned DATA page. The runtime
    /// fixture keeps its command stream and probe results there.
    pub fn live_data_word(&mut self, space: &UserSpace, offset: usize) -> Result<u64> {
        self.member_boundary(space)?;
        if !offset.is_multiple_of(8) || offset + 8 > PAGE as usize {
            return Err(Error::Address);
        }
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
        })
    }
    /// Raw object-page access used only by the trusted controller to seed and
    /// verify the shared payload while the owning Node is not executing.
    pub fn live_object_write(
        &mut self,
        object: MemoryObjectId,
        page: usize,
        offset: usize,
        value: u64,
    ) -> Result<()> {
        let frame = self.registry.memory_object_frame(object, page)?;
        if !offset.is_multiple_of(8) || offset + 8 > PAGE as usize {
            return Err(Error::Address);
        }
        self.with_frame_mut(frame, |bytes| {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes())
        })
    }
    pub fn live_object_word(
        &mut self,
        object: MemoryObjectId,
        page: usize,
        offset: usize,
    ) -> Result<u64> {
        let frame = self.registry.memory_object_frame(object, page)?;
        if !offset.is_multiple_of(8) || offset + 8 > PAGE as usize {
            return Err(Error::Address);
        }
        self.with_frame(frame, |bytes| {
            u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
        })
    }
    pub fn live_block_word(&mut self, space: &UserSpace, label: u64, offset: usize) -> Result<u64> {
        if !(1..=3).contains(&label) || !offset.is_multiple_of(8) {
            return Err(Error::Address);
        }
        let words = self.live_words(space)?;
        let at = live_block(label) + offset;
        words.get(at / 8).copied().ok_or(Error::Corrupt)
    }
    pub fn live_block_write(
        &mut self,
        space: &UserSpace,
        label: u64,
        offset: usize,
        value: u64,
    ) -> Result<()> {
        if !(1..=3).contains(&label) || !offset.is_multiple_of(8) {
            return Err(Error::Address);
        }
        self.live_write(space, live_block(label) + offset, value)
    }
    /// Publish one bounded checkpoint and delay in kernel mode. The descriptor
    /// contains addresses, sizes and raw production values only; it never
    /// asserts success or grants an operation.
    #[allow(clippy::too_many_arguments)]
    pub fn live_checkpoint(
        &self,
        stage: u64,
        mode: u64,
        a: Option<&UserSpace>,
        b: Option<&UserSpace>,
        saved: [Option<u64>; 3],
        node_a: u64,
        node_b: u64,
        space_a: u64,
        space_b: u64,
        threads: [u64; 3],
    ) -> Result<()> {
        let mut words = [0u64; 128];
        words[..14].copy_from_slice(&[
            LIVE_MAGIC,
            stage,
            mode,
            self.tree.root,
            LIVE_VA_A,
            LIVE_VA_B,
            LIVE_VALUE,
            LIVE_SECOND,
            1,
            node_a,
            node_b,
            space_a,
            space_b,
            ptr::addr_of!(crate::arch::native::NEU_LIVE_FAULT) as u64,
        ]);
        words[14] = LIVE_RESULT;
        words[16..55].copy_from_slice(&self.registry.diagnostic_layout());
        words[64..72].copy_from_slice(&self.memory.frames.diagnostic_layout());
        words[80..86].copy_from_slice(&[
            self.memory.tables.as_ptr() as u64,
            size_of::<TableOwner>() as u64,
            TABLES as u64,
            offset_of!(TableOwner, physical) as u64,
            offset_of!(TableOwner, level) as u64,
            offset_of!(TableOwner, pinned) as u64,
        ]);
        for (i, token) in [a, b].into_iter().enumerate() {
            if let Some(space) = token {
                words[90 + i * 4..94 + i * 4].copy_from_slice(&[
                    ptr::addr_of!(space.root) as u64,
                    ptr::addr_of!(space.id) as u64,
                    space.tables.as_ptr() as u64,
                    ptr::addr_of!(space.teardown) as u64,
                ]);
                words[98 + i] = space.stack_tables.as_ptr() as u64;
            }
        }
        for (i, address) in saved.into_iter().enumerate() {
            words[100 + i] = address.unwrap_or(0);
        }
        words[104..108].copy_from_slice(&[
            live_block(1) as u64,
            live_block(3) as u64,
            live_block(2) as u64,
            LIVE_RESULT,
        ]);
        words[108] = LIVE_GATE as u64;
        words[109] = LIVE_FAULT_MARKER as u64;
        words[110] = LIVE_READBACK as u64;
        words[111] = LIVE_POST as u64;
        words[112..116].copy_from_slice(&[threads[0], threads[1], threads[2], LIVE_DATA]);
        words[116] = logic::thread_user_stack(0);
        words[117] = logic::thread_user_stack(1);
        words[118] = logic::thread_user_stack(2);
        for (cell, value) in neu_memory_live_witness.iter().zip(words) {
            cell.store(value, Ordering::Release);
        }
        super::super::serial::write(format_args!(
            "[NEU] Live checkpoint: stage={} mode={}\n",
            stage, mode
        ))
        .map_err(|_| Error::Corrupt)?;
        // SAFETY: BSP, IF=0, exclusive live Paging borrow. The assembly only
        // delays; the host freezes the CPU before inspecting any marker.
        unsafe {
            neu_memory_live_wait();
        }
        Ok(())
    }
}
unsafe extern "C" {
    fn neu_memory_live_wait();
}
