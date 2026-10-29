//! Kernel-only checkpoints. QMP stops the sole CPU inside the bounded wait and
//! reads production fields via the schema below; no diagnostic scalar grants
//! authority. Atomic storage avoids a mutable static/Sync ownership shortcut.
use super::*;
use core::{
    arch::global_asm,
    mem::{offset_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

#[unsafe(no_mangle)]
static neu_memory_witness: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
global_asm!(
    r#"
.section .text
.global neu_memory_checkpoint_wait
.global neu_memory_checkpoint_wait_end
neu_memory_checkpoint_wait:
    mov ecx, 0x10000000
1:  dec ecx
    jnz 1b
neu_memory_checkpoint_wait_end:
    ret
"#
);
unsafe extern "C" {
    fn neu_memory_checkpoint_wait();
}

impl Paging<'_, '_> {
    fn memory_checkpoint(
        &self,
        stage: u64,
        mode: u64,
        vas: [u64; 2],
        a: Option<&UserSpace>,
        b: Option<&UserSpace>,
    ) -> Result<()> {
        let mut words = [0u64; 128];
        words[..6].copy_from_slice(&[
            0x4e45554d4f000001,
            stage,
            mode,
            self.tree.root,
            vas[0],
            vas[1],
        ]);
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
            }
        }
        for (cell, value) in neu_memory_witness.iter().zip(words) {
            cell.store(value, Ordering::Release);
        }
        super::super::serial::write(format_args!(
            "[NEU] Memory checkpoint: stage={} mode={}\n",
            stage, mode
        ))
        .map_err(|_| Error::Corrupt)?;
        // SAFETY: BSP, IF=0, live exclusive Paging borrow. The assembly only
        // delays. The host freezes the CPU before inspecting any metadata.
        unsafe {
            neu_memory_checkpoint_wait();
        }
        Ok(())
    }

    pub fn memory_object_test(&mut self) -> Result<()> {
        let mode = if cfg!(feature = "test-memory-object-fail-stop") {
            5
        } else if cfg!(feature = "test-memory-object-lifetime-reverse") {
            1
        } else if cfg!(feature = "test-memory-object-new-tables") {
            2
        } else if cfg!(feature = "test-memory-object-map-rollback") {
            3
        } else if cfg!(feature = "test-memory-object-new-tables-reverse") {
            4
        } else {
            0
        };
        let reverse = mode == 1 || mode == 4;
        // The low 64 KiB contains the existing image, Thread stacks and guards.
        // 0x600000 is the next eligible 2 MiB region inside the 4 MiB user range.
        let vas = if mode == 2 || mode == 4 {
            [logic::USER_BASE + (1 << 21); 2]
        } else {
            [logic::USER_BASE + 0x10000, logic::USER_BASE + 0x20000]
        };
        let before = self.stats();
        let tables = self.table_count();
        self.memory_checkpoint(1, mode, vas, None, None)?;
        let object = self.create_memory_object(2)?;
        let old_frames = [
            self.registry.memory_object_frame(object, 0)?,
            self.registry.memory_object_frame(object, 1)?,
        ];
        self.memory_checkpoint(2, mode, vas, None, None)?;
        let mut a = self.prepare_domain(&[0xc3], 0, 35, None, None)?;
        let mut b = self.prepare_domain(&[0xc3], 0, 77, None, None)?;
        let ga = self.registry.authorize_memory_mapping(object, a.id, true)?;
        let gb = self
            .registry
            .authorize_memory_mapping(object, b.id, false)?;
        self.memory_checkpoint(12, mode, vas, Some(&a), Some(&b))?;
        #[cfg(feature = "test-memory-object-fail-stop")]
        if mode == 5 {
            self.memory_checkpoint(8, mode, vas, Some(&a), Some(&b))?;
            self.memory.budget = Some(0);
            let tree = PageTree {
                root: a.root,
                bits: self.tree.bits,
            };
            let mut broken = RejectClear(&mut self.memory);
            let _ = logic::map_memory_object(
                &tree,
                &mut broken,
                &mut self.registry,
                &ga,
                0,
                logic::USER_BASE + (1 << 21) - PAGE,
                2,
                Permissions::RW,
            );
            return Err(Error::Corrupt); // The required path must panic before returning.
        }
        if mode == 3 {
            self.memory.budget = Some(0);
            let before_leaf = self.map_memory_backing(
                &mut a,
                &ga,
                0,
                logic::USER_BASE + (1 << 21),
                2,
                Permissions::RW,
            );
            self.memory.budget = None;
            if before_leaf != Err(Error::Exhausted) {
                return Err(Error::Corrupt);
            }
            // Allocate the new PT, install its first leaf, then reject the
            // second leaf write. Rollback must release that new PT and alias.
            self.memory.reject_leaf_index = Some(1);
            let after_leaf = self.map_memory_backing(
                &mut a,
                &ga,
                0,
                logic::USER_BASE + (1 << 21),
                2,
                Permissions::RW,
            );
            if after_leaf != Err(Error::Exhausted) || self.memory.reject_leaf_index.is_some() {
                return Err(Error::Corrupt);
            }
            self.memory_checkpoint(13, mode, vas, Some(&a), Some(&b))?;
            // First leaf uses an existing table; the second needs a new PT.
            self.memory.budget = Some(0);
            let result = self.map_memory_backing(
                &mut a,
                &ga,
                0,
                logic::USER_BASE + (1 << 21) - PAGE,
                2,
                Permissions::RW,
            );
            self.memory.budget = None;
            if result != Err(Error::Exhausted) {
                return Err(Error::Corrupt);
            }
            self.memory_checkpoint(8, mode, vas, Some(&a), Some(&b))?;
        }
        if self.map_memory_backing(&mut b, &ga, 0, vas[1], 2, Permissions::R)
            != Err(Error::NotOwned)
            || self.map_memory_backing(&mut b, &gb, 0, vas[1], 2, Permissions::RW)
                != Err(Error::Flags)
        {
            return Err(Error::Corrupt);
        }
        self.map_memory_backing(&mut a, &ga, 0, vas[0], 2, Permissions::RW)?;
        self.map_memory_backing(&mut b, &gb, 0, vas[1], 2, Permissions::R)?;
        self.memory_checkpoint(3, mode, vas, Some(&a), Some(&b))?;
        if mode == 2 || mode == 4 {
            self.unmap_memory_backing(&mut b, vas[1])?;
            self.unmap_memory_backing(&mut b, vas[1] + PAGE)?;
            self.memory_checkpoint(8, mode, vas, Some(&a), Some(&b))?;
            self.map_memory_backing(&mut b, &gb, 0, vas[1], 2, Permissions::R)?;
            self.memory_checkpoint(9, mode, vas, Some(&a), Some(&b))?;
        }
        self.registry.release_memory_object(object)?;
        self.memory_checkpoint(4, mode, vas, Some(&a), Some(&b))?;
        let (first, second) = if reverse {
            (&mut b, &mut a)
        } else {
            (&mut a, &mut b)
        };
        let root = first.root;
        first.root = second.root;
        if self.destroy_user(first) != Err(Error::InUse) {
            return Err(Error::Corrupt);
        }
        first.root = root;
        self.registry.activate_space(first.id)?;
        if self.destroy_user(first) != Err(Error::InUse) {
            return Err(Error::Corrupt);
        }
        self.registry.deactivate_space(first.id)?;
        let reference = self.registry.retain_space(first.id)?;
        if self.destroy_user(first) != Err(Error::InUse) {
            return Err(Error::Corrupt);
        }
        self.registry
            .release_space_ref(reference)
            .map_err(|_| Error::Corrupt)?;
        self.registry.pin_space(first.id)?;
        if self.destroy_user(first) != Err(Error::InUse) {
            return Err(Error::Corrupt);
        }
        self.registry.unpin_space(first.id)?;
        self.destroy_user(first)?;
        if self.destroy_user(first) != Err(Error::NotOwned)
            || self.collect_memory_object(object) != Err(Error::InUse)
        {
            return Err(Error::Corrupt);
        }
        self.memory_checkpoint(5, mode, vas, Some(&a), Some(&b))?;
        if reverse {
            self.destroy_user(&mut a)?;
        } else {
            self.destroy_user(&mut b)?;
        }
        self.memory_checkpoint(6, mode, vas, Some(&a), Some(&b))?;
        self.collect_memory_object(object)?;
        self.memory_checkpoint(7, mode, vas, Some(&a), Some(&b))?;
        for frame in old_frames {
            if self.registry.physical(frame) != Err(Error::NotOwned) {
                return Err(Error::Corrupt);
            }
        }
        if self.collect_memory_object(object) != Err(Error::NotOwned) {
            return Err(Error::Corrupt);
        }
        let replacement = self.create_memory_object(2)?;
        if replacement == object
            || self.registry.memory_object_counts(object) != Err(Error::NotOwned)
        {
            return Err(Error::Corrupt);
        }
        self.memory_checkpoint(10, mode, vas, Some(&a), Some(&b))?;
        self.registry.release_memory_object(replacement)?;
        self.collect_memory_object(replacement)?;
        self.memory_checkpoint(11, mode, vas, Some(&a), Some(&b))?;
        if self.stats() != before || self.table_count() != tables {
            return Err(Error::Corrupt);
        }
        super::super::serial::write(format_args!(
            "[NEU] MemoryObject lifecycle complete: mode={}\n",
            mode
        ))
        .map_err(|_| Error::Corrupt)
    }
}

// Deliberate backend corruption profile: allocation of the second leaf's PT
// fails normally; clearing the first installed leaf during rollback then fails.
#[cfg(feature = "test-memory-object-fail-stop")]
struct RejectClear<'a, 'b, 'c>(&'a mut Memory<'b, 'c>);
#[cfg(feature = "test-memory-object-fail-stop")]
impl TableMemory for RejectClear<'_, '_, '_> {
    fn allocate(&mut self, level: u8) -> Result<u64> {
        self.0.allocate(level)
    }
    fn release(&mut self, address: u64) -> Result<()> {
        self.0.release(address)
    }
    fn owned(&self, address: u64, level: u8) -> bool {
        self.0.owned(address, level)
    }
    fn pinned(&self, address: u64) -> bool {
        self.0.pinned(address)
    }
    fn read(&self, table: u64, index: usize) -> Result<u64> {
        self.0.read(table, index)
    }
    fn write(&mut self, table: u64, index: usize, value: u64) -> Result<()> {
        if value == 0 {
            Err(Error::Corrupt)
        } else {
            self.0.write(table, index, value)
        }
    }
    fn invalidate(&mut self, address: u64) {
        self.0.invalidate(address);
    }
    fn flush(&mut self) {
        self.0.flush();
    }
}
