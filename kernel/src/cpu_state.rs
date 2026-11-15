//! Eager x86-64 x87/SSE2 state. Hardware copies are CPU-owned; saved copies
//! remain bound to a complete Thread/Node/address-space identity.
use crate::{node::ActiveContext, paging::canonical};

pub const BYTES: usize = 512;
pub const REQUIRED_EDX: u32 = 1 | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26);
pub const DEFAULT_MXCSR: u32 = 0x1f80;
pub const FALLBACK_MXCSR_MASK: u32 = 0xffbf;
pub const NO_CPU: u64 = u64::MAX;

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxImage(pub [u8; BYTES]);
impl FxImage {
    pub const fn zero() -> Self {
        Self([0; BYTES])
    }
    pub fn initial(mask: u32) -> Result<Self, Error> {
        let mask = normalized_mask(mask)?;
        let mut image = Self::zero();
        image.0[..2].copy_from_slice(&0x037fu16.to_le_bytes());
        image.0[24..28].copy_from_slice(&DEFAULT_MXCSR.to_le_bytes());
        image.0[28..32].copy_from_slice(&mask.to_le_bytes());
        Ok(image)
    }
    pub fn mxcsr(&self) -> u32 {
        u32::from_le_bytes(self.0[24..28].try_into().unwrap())
    }
    pub fn mask(&self) -> u32 {
        u32::from_le_bytes(self.0[28..32].try_into().unwrap())
    }
    pub fn valid(&self, mask: u32) -> bool {
        normalized_mask(mask).is_ok_and(|mask| self.mxcsr() & !mask == 0)
    }
}

pub fn normalized_mask(mask: u32) -> Result<u32, Error> {
    let mask = if mask == 0 { FALLBACK_MXCSR_MASK } else { mask };
    if mask & !0xffff != 0 || DEFAULT_MXCSR & !mask != 0 {
        return Err(Error::Unsupported);
    }
    Ok(mask)
}
pub fn supported(edx: u32) -> bool {
    edx & REQUIRED_EDX == REQUIRED_EDX
}
pub fn valid_tls(base: u64) -> bool {
    canonical(base) && base < 0x0000_8000_0000_0000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Identity,
    Busy,
    Invalid,
    Exhausted,
}

#[repr(C, align(16))]
pub struct Slot {
    pub image: FxImage,
    /// Thread, Node, CR3 and AddressSpace; zero after retirement.
    pub identity: [u64; 4],
    pub fs_base: u64,
    pub saves: u64,
    pub loads: u64,
    pub live: u64,
    pub cpu: u64,
    pub retired_generation: u64,
}
impl Slot {
    const fn empty() -> Self {
        Self {
            image: FxImage::zero(),
            identity: [0; 4],
            fs_base: 0,
            saves: 0,
            loads: 0,
            live: 0,
            cpu: NO_CPU,
            retired_generation: 0,
        }
    }
    fn matches(&self, active: ActiveContext) -> bool {
        self.live == 1 && self.identity == identity(active)
    }
}
fn identity(active: ActiveContext) -> [u64; 4] {
    [
        active.thread.raw(),
        active.node.raw(),
        active.context.root,
        active.context.space.raw(),
    ]
}

#[repr(C)]
pub struct Contexts<const N: usize> {
    pub slots: [Slot; N],
    pub mask: u32,
    pub cpu_count: usize,
}
impl<const N: usize> Default for Contexts<N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<const N: usize> Contexts<N> {
    pub const fn new() -> Self {
        Self {
            slots: [const { Slot::empty() }; N],
            mask: FALLBACK_MXCSR_MASK,
            cpu_count: crate::cpu::MAX_CPUS,
        }
    }
    pub fn configure(&mut self, masks: &[u32], features: &[u32]) -> Result<(), Error> {
        if masks.is_empty()
            || masks.len() > crate::cpu::MAX_CPUS
            || masks.len() != features.len()
            || self.slots.iter().any(|s| s.live != 0)
        {
            return Err(Error::Busy);
        }
        if features.iter().any(|f| !supported(*f)) {
            return Err(Error::Unsupported);
        }
        let mask = normalized_mask(masks[0])?;
        if masks.iter().any(|m| normalized_mask(*m) != Ok(mask)) {
            return Err(Error::Unsupported);
        }
        self.mask = mask;
        self.cpu_count = masks.len();
        Ok(())
    }
    pub fn install(&mut self, active: ActiveContext) -> Result<(), Error> {
        if active.context.root == 0
            || active.context.root & 0xfff != 0
            || crate::paging::physical(active.context.root, 52).is_err()
            || active.context.space == crate::paging::SpaceId::KERNEL
        {
            return Err(Error::Invalid);
        }
        let image = FxImage::initial(self.mask)?;
        let slot = self
            .slots
            .get_mut(active.thread.index())
            .ok_or(Error::Identity)?;
        if slot.live != 0 || slot.cpu != NO_CPU {
            return Err(Error::Busy);
        }
        if active.thread.raw() >> 32 <= slot.retired_generation {
            return Err(Error::Identity);
        }
        slot.image = image;
        slot.identity = identity(active);
        slot.fs_base = 0;
        slot.saves = 0;
        slot.loads = 0;
        slot.live = 1;
        Ok(())
    }
    fn slot(&self, active: ActiveContext) -> Result<&Slot, Error> {
        let slot = self
            .slots
            .get(active.thread.index())
            .ok_or(Error::Identity)?;
        if !slot.matches(active) {
            return Err(Error::Identity);
        }
        Ok(slot)
    }
    fn slot_mut(&mut self, active: ActiveContext) -> Result<&mut Slot, Error> {
        let slot = self
            .slots
            .get_mut(active.thread.index())
            .ok_or(Error::Identity)?;
        if !slot.matches(active) {
            return Err(Error::Identity);
        }
        Ok(slot)
    }
    pub fn load(&mut self, cpu: usize, active: ActiveContext) -> Result<(FxImage, u64), Error> {
        if cpu >= self.cpu_count {
            return Err(Error::Identity);
        }
        if self.slots.iter().any(|s| s.cpu == cpu as u64) {
            return Err(Error::Busy);
        }
        let mask = self.mask;
        let slot = self.slot_mut(active)?;
        if slot.cpu != NO_CPU {
            return Err(Error::Busy);
        }
        if !slot.image.valid(mask) || !valid_tls(slot.fs_base) {
            return Err(Error::Invalid);
        }
        let loads = slot.loads.checked_add(1).ok_or(Error::Exhausted)?;
        slot.cpu = cpu as u64;
        slot.loads = loads;
        Ok((slot.image, slot.fs_base))
    }
    pub fn save(
        &mut self,
        cpu: usize,
        active: ActiveContext,
        image: FxImage,
        fs_base: u64,
    ) -> Result<(), Error> {
        if cpu >= self.cpu_count {
            return Err(Error::Identity);
        }
        let mask = self.mask;
        let slot = self.slot_mut(active)?;
        if slot.cpu != cpu as u64 {
            return Err(Error::Busy);
        }
        if !image.valid(mask) || !valid_tls(fs_base) {
            return Err(Error::Invalid);
        }
        let saves = slot.saves.checked_add(1).ok_or(Error::Exhausted)?;
        slot.image = image;
        slot.fs_base = fs_base;
        slot.saves = saves;
        slot.cpu = NO_CPU;
        Ok(())
    }
    pub fn tls(&self, active: ActiveContext) -> Result<u64, Error> {
        Ok(self.slot(active)?.fs_base)
    }
    pub fn set_tls(&mut self, active: ActiveContext, base: u64) -> Result<(), Error> {
        if !valid_tls(base) {
            return Err(Error::Invalid);
        }
        let slot = self.slot_mut(active)?;
        if slot.cpu != NO_CPU {
            return Err(Error::Busy);
        }
        slot.fs_base = base;
        Ok(())
    }
    pub fn retire(&mut self, active: ActiveContext) -> Result<(), Error> {
        let slot = self.slot_mut(active)?;
        if slot.cpu != NO_CPU || slot.saves != slot.loads {
            return Err(Error::Busy);
        }
        let generation = active.thread.raw() >> 32;
        *slot = Slot::empty();
        slot.retired_generation = generation;
        Ok(())
    }
    pub fn clean(&self) -> bool {
        self.slots.iter().all(|s| {
            s.live == 0
                && s.cpu == NO_CPU
                && s.identity == [0; 4]
                && s.fs_base == 0
                && s.loads == 0
                && s.saves == 0
                && s.image == FxImage::zero()
        })
    }
}

const _: () = assert!(core::mem::size_of::<FxImage>() == BYTES);
const _: () = assert!(core::mem::align_of::<FxImage>() == 16);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        node::{Nodes, ThreadContext},
        paging::Registry,
    };
    fn active() -> ActiveContext {
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        let space = registry.create_space().unwrap();
        registry.bind_root(space, 0x1000).unwrap();
        let node = nodes.create().unwrap();
        nodes.configure(node).unwrap();
        nodes.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let thread = nodes.initial_thread(node, context).unwrap();
        ActiveContext {
            node,
            thread,
            context,
        }
    }
    fn pair() -> [ActiveContext; 2] {
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        core::array::from_fn(|index| {
            let space = registry.create_space().unwrap();
            let root = (index as u64 + 1) * 0x1000;
            registry.bind_root(space, root).unwrap();
            let node = nodes.create().unwrap();
            nodes.configure(node).unwrap();
            nodes.bind_space(node, space).unwrap();
            let context = ThreadContext {
                space,
                root,
                entry: 0x400000,
                rsp: 0x404ff8 + index as u64 * 0x4000,
            };
            let thread = nodes.initial_thread(node, context).unwrap();
            ActiveContext {
                node,
                thread,
                context,
            }
        })
    }
    #[test]
    fn initial_image_zeros_all_payload_and_reserved_bytes() {
        let image = FxImage::initial(0xffff).unwrap();
        assert_eq!(&image.0[..2], &0x37fu16.to_le_bytes());
        assert!(image.0[2..24].iter().all(|b| *b == 0));
        assert_eq!(image.mxcsr(), 0x1f80);
        assert!(image.0[32..].iter().all(|b| *b == 0));
        assert!(image.valid(0xffff));
    }
    #[test]
    fn missing_features_and_nonportable_masks_refuse_before_installation() {
        for bit in [0, 23, 24, 25, 26] {
            assert!(!supported(REQUIRED_EDX & !(1 << bit)));
        }
        let mut contexts = Contexts::<3>::new();
        assert_eq!(
            contexts.configure(&[0xffff, 0xffbf], &[REQUIRED_EDX; 2]),
            Err(Error::Unsupported)
        );
        assert_eq!(
            contexts.configure(&[0xffff], &[REQUIRED_EDX & !1]),
            Err(Error::Unsupported)
        );
        assert!(contexts.clean());
        contexts
            .configure(&[0, 0xffbf], &[REQUIRED_EDX; 2])
            .unwrap();
        assert_eq!(contexts.mask, 0xffbf);
    }
    #[test]
    fn mxcsr_reserved_bits_and_invalid_masks_are_rejected() {
        assert_eq!(normalized_mask(1 << 16), Err(Error::Unsupported));
        assert_eq!(normalized_mask(1), Err(Error::Unsupported));
        let mut image = FxImage::initial(0xffff).unwrap();
        image.0[24..28].copy_from_slice(&(1u32 << 16).to_le_bytes());
        assert!(!image.valid(0xffff));
    }
    #[test]
    fn ownership_prevents_double_load_foreign_save_and_premature_retirement() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        let (image, base) = contexts.load(1, active).unwrap();
        assert_eq!(base, 0);
        assert_eq!(contexts.load(2, active), Err(Error::Busy));
        assert_eq!(contexts.save(2, active, image, 0), Err(Error::Busy));
        assert_eq!(contexts.retire(active), Err(Error::Busy));
        assert_eq!(contexts.set_tls(active, 0x402200), Err(Error::Busy));
        contexts.save(1, active, image, 0).unwrap();
        assert_eq!(contexts.save(1, active, image, 0), Err(Error::Busy));
    }
    #[test]
    fn identity_includes_node_root_and_address_space() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        let mut foreign = active;
        foreign.context.root += 0x1000;
        assert_eq!(contexts.load(0, foreign), Err(Error::Identity));
        assert_eq!(contexts.tls(foreign), Err(Error::Identity));
        assert_eq!(contexts.retire(foreign), Err(Error::Identity));
        assert_eq!(contexts.install(active), Err(Error::Busy));
    }
    #[test]
    fn migration_preserves_exact_architectural_bytes_and_tls() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        contexts.set_tls(active, 0x402200).unwrap();
        let (mut image, base) = contexts.load(1, active).unwrap();
        for (i, b) in image.0[32..416].iter_mut().enumerate() {
            *b = i as u8;
        }
        contexts.save(1, active, image, base).unwrap();
        assert_eq!(contexts.load(3, active).unwrap(), (image, base));
        contexts.save(3, active, image, base).unwrap();
        assert_eq!(contexts.slots[0].loads, 2);
        assert_eq!(contexts.slots[0].saves, 2);
    }
    #[test]
    fn invalid_tls_and_save_do_not_mutate_a_live_context() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        contexts.set_tls(active, 0x402200).unwrap();
        for base in [0x800000000000, 0xffff800000000000, u64::MAX] {
            assert_eq!(contexts.set_tls(active, base), Err(Error::Invalid));
            assert_eq!(contexts.tls(active).unwrap(), 0x402200);
        }
        let (image, base) = contexts.load(0, active).unwrap();
        assert_eq!(
            contexts.save(0, active, image, u64::MAX),
            Err(Error::Invalid)
        );
        assert_eq!(contexts.slots[0].cpu, 0);
        contexts.save(0, active, image, base).unwrap();
    }
    #[test]
    fn retirement_scrubs_saved_bytes_and_refuses_stale_generation() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        contexts.set_tls(active, 0x402200).unwrap();
        let (mut image, base) = contexts.load(0, active).unwrap();
        image.0[160..416].fill(0xaa);
        contexts.save(0, active, image, base).unwrap();
        contexts.retire(active).unwrap();
        assert!(contexts.clean());
        assert_eq!(contexts.tls(active), Err(Error::Identity));
        assert_eq!(contexts.install(active), Err(Error::Identity));
        assert_eq!(contexts.slots[0].retired_generation, 1);
    }
    #[test]
    fn counter_exhaustion_cannot_publish_partial_ownership() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.install(active).unwrap();
        contexts.slots[0].loads = u64::MAX;
        assert_eq!(contexts.load(0, active), Err(Error::Exhausted));
        assert_eq!(contexts.slots[0].cpu, NO_CPU);
        contexts.slots[0].loads = 0;
        let (image, base) = contexts.load(0, active).unwrap();
        contexts.slots[0].saves = u64::MAX;
        assert_eq!(contexts.save(0, active, image, base), Err(Error::Exhausted));
        assert_eq!(contexts.slots[0].cpu, 0);
    }
    #[test]
    fn each_cpu_can_own_only_one_loaded_thread() {
        let [first, second] = pair();
        let mut contexts = Contexts::<3>::new();
        contexts.install(first).unwrap();
        contexts.install(second).unwrap();
        let (image, base) = contexts.load(0, first).unwrap();
        assert_eq!(contexts.load(0, second), Err(Error::Busy));
        assert_eq!(contexts.slots[second.thread.index()].loads, 0);
        contexts.save(0, first, image, base).unwrap();
        assert!(contexts.load(0, second).is_ok());
    }
    #[test]
    fn foreign_node_and_space_cannot_read_or_load_an_existing_thread() {
        let [first, second] = pair();
        let mut contexts = Contexts::<3>::new();
        contexts.install(first).unwrap();
        for foreign in [
            ActiveContext {
                node: second.node,
                ..first
            },
            ActiveContext {
                context: ThreadContext {
                    space: second.context.space,
                    ..first.context
                },
                ..first
            },
        ] {
            assert_eq!(contexts.tls(foreign), Err(Error::Identity));
            assert_eq!(contexts.load(0, foreign), Err(Error::Identity));
            assert_eq!(contexts.retire(foreign), Err(Error::Identity));
        }
        assert_eq!(contexts.slots[0].loads, 0);
    }
    #[test]
    fn malformed_hardware_image_and_reconfiguration_cannot_release_ownership() {
        let active = active();
        let mut contexts = Contexts::<3>::new();
        contexts.configure(&[0xffff], &[REQUIRED_EDX]).unwrap();
        contexts.install(active).unwrap();
        assert_eq!(
            contexts.configure(&[0xffff], &[REQUIRED_EDX]),
            Err(Error::Busy)
        );
        let (image, base) = contexts.load(0, active).unwrap();
        let mut bad = image;
        bad.0[24..28].copy_from_slice(&0x10000u32.to_le_bytes());
        assert_eq!(contexts.save(0, active, bad, base), Err(Error::Invalid));
        assert_eq!(contexts.slots[0].cpu, 0);
        assert_eq!(contexts.slots[0].saves, 0);
        assert_eq!(contexts.load(1, active), Err(Error::Identity));
        contexts.save(0, active, image, base).unwrap();
    }
    #[test]
    fn reused_thread_generation_starts_zero_and_refuses_its_old_identity() {
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        let space = registry.create_space().unwrap();
        registry.bind_root(space, 0x1000).unwrap();
        let context = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let node = nodes.create().unwrap();
        nodes.configure(node).unwrap();
        nodes.bind_space(node, space).unwrap();
        let thread = nodes.initial_thread(node, context).unwrap();
        nodes.ready(node).unwrap();
        let first = nodes.start(thread).unwrap();
        let mut contexts = Contexts::<3>::new();
        contexts.install(first).unwrap();
        contexts.set_tls(first, 0x402200).unwrap();
        let (mut image, base) = contexts.load(0, first).unwrap();
        image.0[32..416].fill(0x55);
        contexts.save(0, first, image, base).unwrap();
        nodes
            .complete(first, crate::node::ExitReason::Normal(0))
            .unwrap();
        contexts.retire(first).unwrap();
        nodes.reclaim_thread(thread).unwrap();
        nodes.release_space(node, space).unwrap();
        nodes.reclaim(node).unwrap();
        let new_node = nodes.create().unwrap();
        nodes.configure(new_node).unwrap();
        nodes.bind_space(new_node, space).unwrap();
        let new_thread = nodes.initial_thread(new_node, context).unwrap();
        let second = ActiveContext {
            node: new_node,
            thread: new_thread,
            context,
        };
        assert_eq!(new_thread.index(), thread.index());
        assert_eq!(new_thread.raw() >> 32, 2);
        contexts.install(second).unwrap();
        assert_eq!(contexts.load(0, first), Err(Error::Identity));
        assert_eq!(contexts.tls(first), Err(Error::Identity));
        let (image, base) = contexts.load(0, second).unwrap();
        assert_eq!(image, FxImage::initial(contexts.mask).unwrap());
        assert_eq!(base, 0);
    }
    #[test]
    fn sse_vector_19_lifecycle_scrubs_faulted_siblings_and_preserves_foreign_owner() {
        use crate::{
            context::{SavedContext, StackBinding},
            node::{ExitReason, Fault, FaultSource},
            smp_scheduler::Scheduler,
        };
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        let mut active = [None; 3];
        for (index, place) in active.iter_mut().enumerate().take(2) {
            let space = registry.create_space().unwrap();
            let root = (index as u64 + 1) * 0x1000;
            registry.bind_root(space, root).unwrap();
            let node = nodes.create().unwrap();
            nodes.configure(node).unwrap();
            nodes.bind_space(node, space).unwrap();
            let context = ThreadContext {
                space,
                root,
                entry: 0x400000,
                rsp: 0x404ff8 + index as u64 * 0x4000,
            };
            let thread = nodes.initial_thread(node, context).unwrap();
            nodes.ready(node).unwrap();
            *place = Some(ActiveContext {
                node,
                thread,
                context,
            });
        }
        let a1 = active[0].unwrap();
        let b = active[1].unwrap();
        let context = ThreadContext {
            rsp: 0x40cff8,
            ..a1.context
        };
        let thread = nodes.member_thread(a1.node, context).unwrap();
        let a2 = ActiveContext {
            node: a1.node,
            thread,
            context,
        };
        active[2] = Some(a2);
        let mut scheduler = Scheduler::new(4).unwrap();
        let mut contexts = Contexts::<3>::new();
        for (owner, cpu) in [(a1, 0), (a2, 1), (b, 2)] {
            contexts.install(owner).unwrap();
            contexts
                .set_tls(owner, 0x402200 + owner.thread.index() as u64 * 0x20)
                .unwrap();
            nodes
                .install_execution(
                    owner.thread,
                    SavedContext::initial_preemptible(owner, StackBinding::new(owner)),
                )
                .unwrap();
            scheduler.admit(&mut nodes, cpu, owner.thread, 15).unwrap();
            assert_eq!(
                scheduler.dispatch(&mut nodes, cpu).unwrap().unwrap().active,
                owner
            );
        }
        let (mut sibling, sibling_base) = contexts.load(0, a1).unwrap();
        sibling.0[160..416].fill(0x11);
        let (mut image, base) = contexts.load(1, a2).unwrap();
        image.0[24..28].copy_from_slice(&0x1d84u32.to_le_bytes());
        image.0[160..416].fill(0x33);
        let (foreign, foreign_base) = contexts.load(2, b).unwrap();
        contexts.save(1, a2, image, base).unwrap();
        assert_eq!(scheduler.begin_fault(&nodes, 1, a2).unwrap(), 1);
        contexts.save(0, a1, sibling, sibling_base).unwrap();
        scheduler
            .finish(&mut nodes, 0, a1, ExitReason::Cancelled { by: a2.thread })
            .unwrap();
        contexts.retire(a1).unwrap();
        let fault = Fault {
            source: FaultSource::Cpu,
            vector: 19,
            error: 0,
            rip: 0x400500,
            rsp: a2.context.rsp,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x10202,
            cr2: 0,
        };
        scheduler
            .finish(&mut nodes, 1, a2, ExitReason::UserFault(fault))
            .unwrap();
        contexts.retire(a2).unwrap();
        assert_eq!(
            nodes.thread(a2.thread).unwrap().outcome,
            Some(ExitReason::UserFault(fault))
        );
        assert_eq!(contexts.slots[a1.thread.index()].image, FxImage::zero());
        assert_eq!(contexts.slots[a2.thread.index()].image, FxImage::zero());
        assert_eq!(contexts.load(0, a2), Err(Error::Identity));
        assert_eq!(nodes.active_on(2), Some(b));
        assert_eq!(contexts.slots[b.thread.index()].cpu, 2);
        assert_eq!(contexts.tls(b), Ok(foreign_base));
        contexts.save(2, b, foreign, foreign_base).unwrap();
        assert_eq!(contexts.load(3, b), Ok((foreign, foreign_base)));
    }
}
